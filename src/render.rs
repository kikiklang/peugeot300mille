//! Offline render: every frame in order, at full quality, with the source's sound.

use crate::audio::Sound;
use crate::engine::{lut::Lut, overlay::{Clip, N_OVERLAYS}, Engine};
use crate::media::{probe, Encoder};
use crate::params::Params;
use crate::runner::{out_frame_at, Automation, LutChange, OverlayChange, Runner};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub enum Knobs<'a> {
    Fixed(Params),
    Recorded(&'a Automation),
}

/// The LUTs and loops a take starts with. What can't be reloaded is left out and reported.
pub fn take_media(auto: &Automation) -> ((Option<Arc<Lut>>, Option<Arc<Lut>>), [Option<Arc<Clip>>; N_OVERLAYS], Vec<String>) {
    let mut problems = Vec::new();
    let mut load_lut = |p: &Option<String>| {
        let p = p.as_ref()?;
        match Lut::load(Path::new(p)) {
            Ok(l) => Some(Arc::new(l)),
            Err(e) => {
                problems.push(format!("LUT {p} ignorée ({e:#})"));
                None
            }
        }
    };
    let luts = (load_lut(&auto.lut_in), load_lut(&auto.lut_out));
    let mut clips: [Option<Arc<Clip>>; N_OVERLAYS] = Default::default();
    for (i, c) in clips.iter_mut().enumerate() {
        if let Some(p) = &auto.overlays[i] {
            let (s, e) = auto.overlay_ranges[i].map_or((0.0, None), |(s, e)| (s, Some(e)));
            match Clip::load_range(Path::new(p), s, e) {
                Ok(clip) => *c = Some(Arc::new(clip)),
                Err(e) => problems.push(format!("calque {} ignoré ({e:#})", i + 1)),
            }
        }
    }
    (luts, clips, problems)
}

/// Renders from `start` for `frames` output frames (None = to the end).
/// `progress(done, total)` is called after each frame; `cancel` stops early.
#[allow(clippy::too_many_arguments)]
pub fn render(
    input: &Path,
    output: &Path,
    knobs: Knobs,
    start: f64,
    frames: Option<i64>,
    luts: (Option<Arc<Lut>>, Option<Arc<Lut>>),
    overlays: [Option<Arc<Clip>>; N_OVERLAYS],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(i64, i64),
) -> Result<Vec<String>> {
    let pr = probe(input)?;
    let mut enc = Encoder::open(&pr, output, Some(out_frame_at(start, pr.fps()) as f64 / pr.fps()))?;
    let problems = render_frames(input, knobs, start, frames, luts, overlays, cancel, progress, &mut |f| enc.write(f))?;
    enc.finish()?;
    Ok(problems)
}

/// Runs the take frame by frame into `sink`: the same path whatever receives the pictures.
#[allow(clippy::too_many_arguments)]
pub fn render_frames(
    input: &Path,
    knobs: Knobs,
    start: f64,
    frames: Option<i64>,
    luts: (Option<Arc<Lut>>, Option<Arc<Lut>>),
    overlays: [Option<Arc<Clip>>; N_OVERLAYS],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(i64, i64),
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<Vec<String>> {
    let pr = probe(input)?;
    let fps = pr.fps();
    let j0 = out_frame_at(start, fps);
    let total = match (&knobs, frames) {
        (Knobs::Recorded(a), _) => a.frames.len() as i64,
        (_, Some(n)) => n,
        (_, None) => (pr.frame_count() - j0).max(0),
    };
    let p_at = |j: i64| -> Params {
        match &knobs {
            Knobs::Fixed(p) => *p,
            Knobs::Recorded(a) => a.at(j - j0),
        }
    };
    let mut problems = Vec::new();
    let sound = if pr.has_audio {
        match Sound::load(input) {
            Ok(s) => Some(s),
            Err(e) => {
                problems.push(format!("réaction au son ignorée ({e:#})"));
                None
            }
        }
    } else {
        None
    };
    let mut engine = Engine::new();
    (engine.lut_in, engine.lut_out) = luts;
    engine.overlays = overlays;
    let mut run = Runner::start(pr, Some(engine), sound, j0 as f64 / fps, &p_at)?;
    let changes: &[OverlayChange] = match &knobs {
        Knobs::Recorded(a) => &a.overlay_changes,
        Knobs::Fixed(_) => &[],
    };
    let lut_changes: &[LutChange] = match &knobs {
        Knobs::Recorded(a) => &a.lut_changes,
        Knobs::Fixed(_) => &[],
    };
    let (mut next_change, mut next_lut) = (0, 0);
    let mut done = 0;
    while done < total && !cancel.load(Ordering::Relaxed) {
        // The loops change at the frame they changed during the take.
        while let Some(c) = changes.get(next_change).filter(|c| c.frame as i64 <= done) {
            next_change += 1;
            // Several changes on one frame (a slider being dragged): only the last one shows.
            if changes.get(next_change).is_some_and(|n| n.frame == c.frame && n.slot == c.slot) {
                continue;
            }
            let slot = c.slot.min(N_OVERLAYS - 1);
            match &c.file {
                None => run.engine.overlays[slot] = None,
                Some(f) => {
                    let (s, e) = c.range.map_or((0.0, None), |(s, e)| (s, Some(e)));
                    match Clip::load_range(Path::new(f), s, e) {
                        Ok(clip) => run.engine.overlays[slot] = Some(Arc::new(clip)),
                        Err(e) => problems.push(format!("calque {} non changé ({e:#})", slot + 1)),
                    }
                }
            }
        }
        while let Some(c) = lut_changes.get(next_lut).filter(|c| c.frame as i64 <= done) {
            next_lut += 1;
            let lut = match &c.file {
                None => Ok(None),
                Some(f) => Lut::load(Path::new(f)).map(|l| Some(Arc::new(l))),
            };
            match lut {
                Ok(l) if c.output => run.engine.lut_out = l,
                Ok(l) => run.engine.lut_in = l,
                Err(e) => problems.push(format!("LUT non changée ({e:#})")),
            }
        }
        let p = p_at(run.out_j);
        if !run.next(&p)? {
            break;
        }
        sink(&run.screen)?;
        done += 1;
        progress(done, total);
    }
    Ok(problems)
}
