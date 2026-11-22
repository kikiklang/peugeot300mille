//! The playback thread: runs the box in order, never skipping a frame (if the machine is slow the
//! preview slows down instead), so what you see is exactly what gets recorded.

use crate::audio::{Output, Sound, N_FEAT};
use crate::engine::{lut::Lut, Engine};
use crate::media::{probe, Encoder, Probe};
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
    /// Start recording what is played to this file, or stop.
    RecLive(Option<PathBuf>),
    /// Start or stop recording the knob movements.
    RecAuto(bool),
    /// Sets (or removes) the input or output LUT.
    SetLut { output: bool, lut: Option<Arc<Lut>> },
}

#[derive(Default, Clone)]
pub struct Status {
    pub file: Option<Probe>,
    pub time: f64,
    pub playing: bool,
    pub live_rec: Option<PathBuf>,
    pub auto_rec: bool,
    pub auto_frames: usize,
    pub automation: Option<Automation>,
    pub fps: f64,
    pub h_lock: f32,
    pub has_sound: bool,
    pub lut_in: Option<String>,
    pub lut_out: Option<String>,
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
        self.status.lock().unwrap().message = m.into();
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
                luts: (None, None),
                playing: false,
                live: None,
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
    luts: (Option<Arc<Lut>>, Option<Arc<Lut>>),
    playing: bool,
    live: Option<Encoder>,
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
        *self.shared.params.lock().unwrap()
    }

    fn handle(&mut self, c: Cmd) {
        match c {
            Cmd::Open(path) => {
                self.stop_live();
                self.auto = None;
                self.playing = false;
                match probe(&path) {
                    Ok(pr) => {
                        self.sound = None;
                        if pr.has_audio {
                            self.shared.say("Analyse du son…");
                            self.sound = Sound::load(&path).ok();
                        }
                        {
                            let mut st = self.shared.status.lock().unwrap();
                            st.file = Some(pr.clone());
                            st.automation = None;
                            st.has_sound = self.sound.is_some();
                        }
                        self.restart(pr, 0.0);
                        self.shared.say(format!("Ouvert : {}", path.display()));
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
                self.playing = false;
                if let Some(o) = &self.out {
                    o.clear();
                }
                if self.auto.is_some() {
                    self.finish_auto();
                }
            }
            Cmd::Seek(t) => {
                if let Some(pr) = self.run.as_ref().map(|r| r.probe.clone()) {
                    if self.live.is_some() || self.auto.is_some() {
                        self.shared.say("Arrêtez l'enregistrement avant de vous déplacer.");
                        return;
                    }
                    self.restart(pr, t);
                    self.play_t0 = None;
                }
            }
            Cmd::RecLive(Some(path)) => {
                let Some(run) = self.run.as_ref() else { return };
                let t = run.out_j as f64 / run.probe.fps();
                match Encoder::open(&run.probe, &path, Some(t)) {
                    Ok(e) => {
                        self.live = Some(e);
                        self.shared.status.lock().unwrap().live_rec = Some(path.clone());
                        self.shared.say(format!("Enregistrement dans {}", path.display()));
                        self.playing = true;
                        self.play_t0 = None;
                    }
                    Err(e) => self.shared.say(format!("{e:#}")),
                }
            }
            Cmd::RecLive(None) => self.stop_live(),
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
                });
                let mut st = self.shared.status.lock().unwrap();
                st.auto_rec = true;
                st.auto_frames = 0;
                st.automation = None;
                drop(st);
                self.shared.say("Enregistrement des réglages : bougez les boutons pendant la lecture.");
                self.playing = true;
                self.play_t0 = None;
            }
            Cmd::RecAuto(false) => self.finish_auto(),
            Cmd::SetLut { output, lut } => {
                let name = lut.as_ref().map(|l| l.title.clone());
                {
                    let mut st = self.shared.status.lock().unwrap();
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
        if let Some(o) = &self.out {
            o.clear();
        }
        match Runner::start(pr, Some(engine), self.sound.clone(), t, &|_| p) {
            Ok(mut r) => {
                if let Ok(true) = r.next(&p) {
                    self.publish(&r);
                }
                self.shown = p;
                self.run = Some(r);
            }
            Err(e) => self.shared.say(format!("{e:#}")),
        }
    }

    fn publish(&self, r: &Runner) {
        {
            let mut s = self.shared.screen.lock().unwrap();
            s.0 += 1;
            s.1.clear();
            s.1.extend_from_slice(&r.screen);
        }
        {
            let mut st = self.shared.status.lock().unwrap();
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
        let Some(run) = self.run.as_mut() else {
            self.playing = false;
            return;
        };
        let fps = run.probe.fps();
        let (t0, j0) = *self.play_t0.get_or_insert((Instant::now(), run.out_j));
        match run.next(&p) {
            Ok(true) => {}
            Ok(false) => {
                self.playing = false;
                self.stop_live();
                self.finish_auto();
                self.shared.say("Fin de la vidéo.");
                self.shared.status.lock().unwrap().playing = false;
                return;
            }
            Err(e) => {
                self.playing = false;
                self.shared.say(format!("{e:#}"));
                return;
            }
        }
        // The sound of this frame, to the speakers.
        if let Some(snd) = &self.sound {
            let out = self.out.get_or_insert_with(Output::open);
            let t = (run.out_j - 1) as f64 / fps;
            out.push(snd.track.span(t, t + 1.0 / fps));
        }
        if let Some(enc) = self.live.as_mut() {
            if let Err(e) = enc.write(&run.screen) {
                self.shared.say(format!("{e:#}"));
                self.live = None;
            }
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
            self.shared.status.lock().unwrap().fps = self.fps_meter.1 as f64 / el;
            self.fps_meter = (Instant::now(), 0);
        }
        // Real-time pacing (never faster than the video; slower if the machine cannot keep up).
        let due = t0 + Duration::from_secs_f64((run.out_j - j0) as f64 / fps);
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

    fn stop_live(&mut self) {
        if let Some(enc) = self.live.take() {
            match enc.finish() {
                Ok(p) => self.shared.say(format!("Enregistré : {}", p.display())),
                Err(e) => self.shared.say(format!("{e:#}")),
            }
        }
        self.shared.status.lock().unwrap().live_rec = None;
    }

    fn finish_auto(&mut self) {
        if let Some(a) = self.auto.take() {
            let n = a.frames.len();
            let mut st = self.shared.status.lock().unwrap();
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
    //! Needs a video: GLITCHBOX_TEST_VIDEO=/path/in.mp4 cargo test --release -- --nocapture
    use super::*;
    use std::sync::mpsc::channel;

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
        let Ok(video) = std::env::var("GLITCHBOX_TEST_VIDEO") else { return };
        let dir = std::env::temp_dir().join("glitchbox-test");
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Arc::new(Shared::new());
        let (tx, rx) = channel();
        spawn(shared.clone(), rx, || {});
        tx.send(Cmd::Open(video.into())).unwrap();
        let st = wait(&shared, |s| s.file.is_some(), 10);
        assert!(st.file.is_some(), "open failed: {}", st.message);

        // Live recording of one second, with a knob move in the middle.
        let live = dir.join("live.mp4");
        tx.send(Cmd::RecLive(Some(live.clone()))).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        shared.params.lock().unwrap().feedback_gain = 0.8;
        std::thread::sleep(Duration::from_millis(500));
        tx.send(Cmd::RecLive(None)).unwrap();
        tx.send(Cmd::Pause).unwrap();
        let st = wait(&shared, |s| s.live_rec.is_none() && !s.playing, 10);
        assert!(live.exists(), "no live file: {}", st.message);

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
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        let pr = crate::media::probe(&out).unwrap();
        eprintln!("live {:?}, auto {} frames → {:.2}s, audio {}", live, auto.frames.len(), pr.duration, pr.has_audio);
        assert!(pr.has_audio);
    }
}
