//! The playback thread: runs the box in order, never skipping a frame (if the machine is slow the
//! preview slows down instead), so what you see is exactly what gets recorded.

use crate::audio::{Output, Sound, N_FEAT};
use crate::engine::{lut::Lut, overlay::Clip, Engine};
use crate::media::{lock, probe, Probe};
use crate::params::Params;
use crate::runner::{Automation, Runner};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Cmd {
    Open(PathBuf),
    Play,
    Pause,
    Seek(f64),
    /// Start or stop recording the knob movements.
    RecAuto(bool),
    /// Sets (or removes) the input or output LUT.
    SetLut { output: bool, lut: Option<Arc<Lut>> },
    /// Sets (or removes) the video loop of an overlay slot.
    SetOverlay { slot: usize, clip: Option<Arc<Clip>> },
}

/// What the panel shows about a loaded overlay.
#[derive(Clone)]
pub struct OverlayInfo {
    pub name: String,
    pub path: PathBuf,
    pub start: f64,
    pub end: f64,
    pub duration: f64,
}

#[derive(Default, Clone)]
pub struct Status {
    pub file: Option<Probe>,
    pub time: f64,
    pub playing: bool,
    pub auto_rec: bool,
    pub auto_frames: usize,
    pub automation: Option<Automation>,
    pub fps: f64,
    pub h_lock: f32,
    pub has_sound: bool,
    /// Tempo found in the sound.
    pub bpm: Option<f32>,
    pub lut_in: Option<String>,
    pub lut_out: Option<String>,
    /// Loaded overlays.
    pub overlays: [Option<OverlayInfo>; 3],
    pub feat: [f32; N_FEAT],
    /// Spectrum of the sound at the current frame (1/12 octave bins, power).
    pub spectrum: Vec<f32>,
    pub message: String,
}

pub struct Shared {
    pub params: Mutex<Params>,
    pub status: Mutex<Status>,
    /// Latest screen (RGB8 768 × 480) and a counter that changes with each new one.
    pub screen: Mutex<(u64, Vec<u8>)>,
}

impl Shared {
    pub fn new() -> Self {
        Self { params: Mutex::new(Params::default()), status: Mutex::new(Status::default()), screen: Mutex::new((0, Vec::new())) }
    }
    fn say(&self, m: impl Into<String>) {
        lock(&self.status).message = m.into();
    }
}

/// How far the picture may trail the sound before the sound is brought back to it.
const MAX_BEHIND: f64 = 0.25;
/// At least this long between two jumps: a machine too slow for the video would otherwise repeat
/// a quarter second of sound over and over.
const JUMP_EVERY: Duration = Duration::from_secs(1);
/// Longest wait for the sound in one go, so that commands keep being answered.
const MAX_WAIT: f64 = 0.25;
/// Between two attempts at reopening a lost sound card.
const REOPEN_EVERY: Duration = Duration::from_secs(5);

/// What the picture does once a frame is out, given how far ahead of the sound it is (seconds).
#[derive(Debug, PartialEq)]
enum Pace {
    Wait(f64),
    Continue,
    JumpSound,
}

fn pace(ahead: f64, since_jump: Option<Duration>) -> Pace {
    if ahead > 0.0 {
        Pace::Wait(ahead.min(MAX_WAIT))
    } else if ahead < -MAX_BEHIND && since_jump.is_none_or(|d| d >= JUMP_EVERY) {
        Pace::JumpSound
    } else {
        Pace::Continue
    }
}

pub fn spawn(shared: Arc<Shared>, rx: Receiver<Cmd>, repaint: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("player".into())
        .spawn(move || {
            Player {
                shared,
                rx: Some(rx),
                repaint: Some(Box::new(repaint)),
                run: None,
                sound: None,
                out: None,
                out_opened: None,
                last_jump: None,
                luts: (None, None),
                clips: [None, None, None],
                playing: false,
                auto: None,
                shown: Params::default(),
                dirty_since: None,
                play_t0: None,
                fps_meter: (Instant::now(), 0),
            }
            .run()
        })
        .expect("player thread");
}

struct Player {
    shared: Arc<Shared>,
    rx: Option<Receiver<Cmd>>,
    repaint: Option<Box<dyn Fn() + Send>>,
    run: Option<Runner>,
    sound: Option<Arc<Sound>>,
    out: Option<Output>,
    /// When the sound card was last opened, and when the sound last had to jump.
    out_opened: Option<Instant>,
    last_jump: Option<Instant>,
    luts: (Option<Arc<Lut>>, Option<Arc<Lut>>),
    clips: [Option<Arc<Clip>>; 3],
    playing: bool,
    auto: Option<Automation>,
    shown: Params,
    dirty_since: Option<Instant>,
    play_t0: Option<(Instant, i64)>,
    fps_meter: (Instant, u32),
}

impl Player {
    fn run(mut self) {
        let rx = self.rx.take().unwrap();
        loop {
            let cmd = if self.playing {
                match rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(_) => return,
                }
            } else {
                match rx.recv_timeout(Duration::from_millis(40)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            if let Some(c) = cmd {
                self.handle(c);
                continue;
            }
            if self.playing {
                self.play_one();
            } else {
                self.refresh_paused();
            }
        }
    }

    fn params(&self) -> Params {
        *lock(&self.shared.params)
    }

    fn handle(&mut self, c: Cmd) {
        match c {
            Cmd::Open(path) => {
                self.auto = None;
                lock(&self.shared.status).auto_rec = false;
                self.halt();
                match probe(&path) {
                    Ok(pr) => {
                        // An alignment belongs to the track it was clicked on.
                        lock(&self.shared.params).tempo_restart = -1.0;
                        // Every holder lets go first: two long tracks must not sit in memory
                        // together while the new one is decoded.
                        self.sound = None;
                        if let Some(r) = self.run.as_mut() {
                            r.sound = None;
                        }
                        if let Some(o) = &self.out {
                            o.release();
                        }
                        let mut note = String::new();
                        if pr.has_audio {
                            self.shared.say("Analyse du son…");
                            match Sound::load(&path) {
                                Ok(s) => {
                                    if s.track.truncated {
                                        note = " (son incomplet : fichier abîmé ou trop long pour la mémoire)".into();
                                    }
                                    self.sound = Some(s);
                                }
                                Err(e) => note = format!(" (sans le son : {e:#})"),
                            }
                        }
                        {
                            let mut st = lock(&self.shared.status);
                            st.file = Some(pr.clone());
                            st.automation = None;
                            st.has_sound = self.sound.is_some();
                            st.bpm = self.sound.as_ref().and_then(|s| s.tempo.as_ref()).map(|t| t.bpm as f32);
                            st.spectrum.clear();
                        }
                        self.restart(pr, 0.0);
                        if self.run.is_some() {
                            self.shared.say(format!("Ouvert : {}{note}", path.display()));
                        }
                    }
                    Err(e) => self.shared.say(format!("{e:#}")),
                }
            }
            Cmd::Play => {
                if self.run.is_some() {
                    self.playing = true;
                    self.play_t0 = None;
                }
            }
            Cmd::Pause => {
                self.halt();
                if self.auto.is_some() {
                    self.finish_auto();
                }
            }
            Cmd::Seek(t) => {
                if let Some(pr) = self.run.as_ref().map(|r| r.probe.clone()) {
                    if self.auto.is_some() {
                        self.shared.say("Arrêtez l'enregistrement avant de vous déplacer.");
                        return;
                    }
                    // NaN or a time past the end can come from a stale slider.
                    let end = if pr.duration > 0.0 { pr.duration } else { f64::MAX };
                    self.restart(pr, if t.is_finite() { t.clamp(0.0, end) } else { 0.0 });
                    self.play_t0 = None;
                }
            }
            Cmd::RecAuto(true) => {
                let Some(run) = self.run.as_ref() else { return };
                self.auto = Some(Automation {
                    source: run.probe.path.display().to_string(),
                    start: run.out_j as f64 / run.probe.fps(),
                    fps_num: run.probe.fps_num,
                    fps_den: run.probe.fps_den,
                    frames: Vec::new(),
                    lut_in: self.luts.0.as_ref().map(|l| l.path.display().to_string()),
                    lut_out: self.luts.1.as_ref().map(|l| l.path.display().to_string()),
                    overlays: self.clips.clone().map(|c| c.map(|c| c.path.display().to_string())),
                    overlay_ranges: self.clips.clone().map(|c| c.map(|c| (c.start, c.end))),
                });
                let mut st = lock(&self.shared.status);
                st.auto_rec = true;
                st.auto_frames = 0;
                st.automation = None;
                drop(st);
                self.shared.say("Enregistrement des réglages : bougez les boutons pendant la lecture.");
                self.playing = true;
                self.play_t0 = None;
            }
            Cmd::RecAuto(false) => self.finish_auto(),
            Cmd::SetOverlay { slot, clip } => {
                let slot = slot.min(2);
                lock(&self.shared.status).overlays[slot] = clip.as_ref().map(|c| OverlayInfo {
                    name: c.name.clone(),
                    path: c.path.clone(),
                    start: c.start,
                    end: c.end,
                    duration: c.duration,
                });
                self.clips[slot] = clip;
                if let Some(a) = self.auto.as_mut() {
                    a.overlays = self.clips.clone().map(|c| c.map(|c| c.path.display().to_string()));
                    a.overlay_ranges = self.clips.clone().map(|c| c.map(|c| (c.start, c.end)));
                }
                if let Some(r) = self.run.as_mut() {
                    r.engine.overlays = self.clips.clone();
                }
                if !self.playing {
                    if let Some(r) = self.run.as_ref() {
                        let (pr, t) = (r.probe.clone(), r.time());
                        self.restart(pr, t);
                    }
                }
            }
            Cmd::SetLut { output, lut } => {
                let name = lut.as_ref().map(|l| l.title.clone());
                {
                    let mut st = lock(&self.shared.status);
                    if output {
                        st.lut_out = name;
                    } else {
                        st.lut_in = name;
                    }
                }
                if output {
                    self.luts.1 = lut;
                } else {
                    self.luts.0 = lut;
                }
                if let Some(a) = self.auto.as_mut() {
                    let path = |l: &Option<Arc<Lut>>| l.as_ref().map(|l| l.path.display().to_string());
                    a.lut_in = path(&self.luts.0);
                    a.lut_out = path(&self.luts.1);
                }
                if !self.playing {
                    if let Some(r) = self.run.as_ref() {
                        let (pr, t) = (r.probe.clone(), r.time());
                        self.restart(pr, t);
                    }
                }
            }
        }
    }

    fn restart(&mut self, pr: Probe, t: f64) {
        let p = self.params();
        let mut engine = self.run.take().map(|r| r.engine).unwrap_or_else(Engine::new);
        (engine.lut_in, engine.lut_out) = self.luts.clone();
        engine.overlays = self.clips.clone();
        if let Some(o) = &self.out {
            o.stop();
        }
        match Runner::start(pr, Some(engine), self.sound.clone(), t, &|_| p) {
            Ok(mut r) => {
                if let Ok(true) = r.next(&p) {
                    self.publish(&r);
                }
                self.shown = p;
                self.run = Some(r);
            }
            Err(e) => {
                self.halt();
                self.shared.say(format!("{e:#}"));
            }
        }
    }

    /// Stops the picture and the sound together, and tells the panel.
    fn halt(&mut self) {
        self.playing = false;
        if let Some(o) = &self.out {
            o.stop();
        }
        lock(&self.shared.status).playing = false;
    }

    /// A sound card that went away (unplugged, sound server restarted) is opened again, not too
    /// often since it holds up the picture for a moment.
    fn check_output(&mut self) {
        let lost = self.out.as_ref().is_some_and(Output::lost);
        if lost && self.out_opened.is_none_or(|t| t.elapsed() >= REOPEN_EVERY) {
            self.out = None;
        }
        if self.out.is_none() {
            self.out = Some(Output::open());
            self.out_opened = Some(Instant::now());
        }
    }

    fn publish(&self, r: &Runner) {
        {
            let mut s = lock(&self.shared.screen);
            s.0 += 1;
            s.1.clear();
            s.1.extend_from_slice(&r.screen);
        }
        {
            let mut st = lock(&self.shared.status);
            st.time = r.time();
            st.playing = self.playing;
            st.h_lock = r.engine.h_lock();
            st.feat = r.feat;
            if let Some(snd) = &r.sound {
                st.spectrum.clear();
                st.spectrum.extend_from_slice(snd.spectrum.row(r.tv_frame() + crate::runner::LOOKAHEAD));
            }
            if let Some(a) = &self.auto {
                st.auto_frames = a.frames.len();
            }
        }
        if let Some(f) = &self.repaint {
            f();
        }
    }

    fn play_one(&mut self) {
        let p = self.params();
        if self.run.is_none() {
            self.halt();
            return;
        }
        if self.sound.is_some() {
            self.check_output();
        }
        let run = self.run.as_mut().unwrap();
        let fps = run.probe.fps();
        let (t0, j0) = *self.play_t0.get_or_insert((Instant::now(), run.out_j));
        match run.next(&p) {
            Ok(true) => {}
            Ok(false) => {
                // The track can be longer than the picture: it must not play on alone.
                self.halt();
                self.finish_auto();
                self.shared.say("Fin de la vidéo.");
                return;
            }
            Err(e) => {
                self.halt();
                self.shared.say(format!("{e:#}"));
                return;
            }
        }
        // The sound plays by itself from the card; here it is only started.
        if let (Some(snd), Some(out)) = (&self.sound, &self.out) {
            out.play(snd, (run.out_j - 1) as f64 / fps);
        }
        if let Some(a) = self.auto.as_mut() {
            a.frames.push(p);
        }
        self.shown = p;
        let run = self.run.as_ref().unwrap();
        self.publish(run);
        // FPS meter.
        self.fps_meter.1 += 1;
        let el = self.fps_meter.0.elapsed().as_secs_f64();
        if el > 0.5 {
            lock(&self.shared.status).fps = self.fps_meter.1 as f64 / el;
            self.fps_meter = (Instant::now(), 0);
        }
        // With sound, the picture follows the sound card's clock. If the machine can't keep up,
        // the sound comes back to the picture rather than drifting away from it.
        let next = run.out_j as f64 / fps;
        if let Some(out) = self.out.as_ref().filter(|_| self.sound.is_some()) {
            if let Some(now) = out.time() {
                match pace(next - now, self.last_jump.map(|t| t.elapsed())) {
                    Pace::Wait(s) => std::thread::sleep(Duration::from_secs_f64(s)),
                    Pace::JumpSound => {
                        out.seek(next);
                        self.last_jump = Some(Instant::now());
                    }
                    Pace::Continue => {}
                }
                self.play_t0 = None;
                return;
            }
        }
        // Without sound: real-time pacing (never faster than the video).
        let due = t0 + Duration::from_secs_f64(((run.out_j - j0) as f64 / fps).max(0.0));
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        } else if now - due > Duration::from_millis(500) {
            self.play_t0 = None;
        }
    }

    /// While paused, knob changes re-run the warm-up so the picture follows them.
    fn refresh_paused(&mut self) {
        let p = self.params();
        if p == self.shown {
            self.dirty_since = None;
            return;
        }
        let since = *self.dirty_since.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_millis(120) {
            return;
        }
        self.dirty_since = None;
        if let Some(r) = self.run.as_ref() {
            let pr = r.probe.clone();
            let t = r.time();
            self.restart(pr, t);
        }
    }

    fn finish_auto(&mut self) {
        if let Some(a) = self.auto.take() {
            let n = a.frames.len();
            let mut st = lock(&self.shared.status);
            st.auto_rec = false;
            st.auto_frames = n;
            st.automation = (n > 0).then_some(a);
            drop(st);
            self.shared.say(format!("{n} images de réglages enregistrées. Cliquez sur « Rendre l'enregistrement… »."));
        }
    }
}

#[cfg(test)]
mod tests {
    //! Needs a video: PEUGEOT300MILLE_TEST_VIDEO=/path/in.mp4 cargo test --release -- --nocapture
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn the_picture_waits_for_the_sound_within_limits() {
        assert_eq!(pace(0.01, None), Pace::Wait(0.01));
        assert_eq!(pace(3.0, None), Pace::Wait(MAX_WAIT));
        assert_eq!(pace(-0.1, None), Pace::Continue);
        assert_eq!(pace(f64::NAN, None), Pace::Continue);
    }

    #[test]
    fn a_slow_machine_does_not_stutter_the_sound() {
        assert_eq!(pace(-0.3, None), Pace::JumpSound);
        assert_eq!(pace(-0.3, Some(Duration::from_millis(300))), Pace::Continue);
        assert_eq!(pace(-0.3, Some(JUMP_EVERY)), Pace::JumpSound);
    }

    fn wait(shared: &Shared, cond: impl Fn(&Status) -> bool, secs: u64) -> Status {
        let t0 = Instant::now();
        loop {
            let st = shared.status.lock().unwrap().clone();
            if cond(&st) || t0.elapsed().as_secs() > secs {
                return st;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn play_record_render() {
        let Ok(video) = std::env::var("PEUGEOT300MILLE_TEST_VIDEO") else { return };
        let dir = std::env::temp_dir().join("peugeot300mille-test");
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Arc::new(Shared::new());
        let (tx, rx) = channel();
        spawn(shared.clone(), rx, || {});
        tx.send(Cmd::Open(video.into())).unwrap();
        let st = wait(&shared, |s| s.file.is_some(), 10);
        assert!(st.file.is_some(), "open failed: {}", st.message);

        // A first playback: opening the sound card can hold up the first frames.
        tx.send(Cmd::Play).unwrap();
        let st = wait(&shared, |s| s.time > 0.2, 10);
        assert!(st.time > 0.2, "playback did not start: {}", st.message);
        tx.send(Cmd::Pause).unwrap();
        wait(&shared, |s| !s.playing, 10);

        // Knob recording, then offline render of it.
        tx.send(Cmd::Seek(0.5)).unwrap();
        tx.send(Cmd::RecAuto(true)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        shared.params.lock().unwrap().mix = 0.7;
        std::thread::sleep(Duration::from_millis(400));
        tx.send(Cmd::RecAuto(false)).unwrap();
        let st = wait(&shared, |s| s.automation.is_some(), 10);
        let auto = st.automation.expect("automation");
        assert!(auto.frames.len() > 5);
        assert!(auto.frames.iter().any(|p| p.mix > 0.6));
        let out = dir.join("auto.mp4");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        crate::render::render(
            std::path::Path::new(&auto.source),
            &out,
            crate::render::Knobs::Recorded(&auto),
            auto.start,
            None,
            (None, None),
            [None, None, None],
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        let pr = crate::media::probe(&out).unwrap();
        eprintln!("auto {} frames → {:.2}s, audio {}", auto.frames.len(), pr.duration, pr.has_audio);
        assert!(pr.has_audio);
    }
}
