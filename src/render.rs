//! Offline render: every frame in order, at full quality, with the source's sound.

use crate::audio::Sound;
use crate::engine::{lut::Lut, Engine};
use crate::media::{probe, Encoder};
use crate::params::Params;
use crate::runner::{Automation, Runner};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub enum Knobs<'a> {
    Fixed(Params),
    Recorded(&'a Automation),
}

/// Renders from `start` for `frames` output frames (None = to the end).
/// `progress(done, total)` is called after each frame; `cancel` stops early.
pub fn render(
    input: &Path,
    output: &Path,
    knobs: Knobs,
    start: f64,
    frames: Option<i64>,
    luts: (Option<Arc<Lut>>, Option<Arc<Lut>>),
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(i64, i64),
) -> Result<()> {
    let pr = probe(input)?;
    let fps = pr.fps();
    let j0 = (start * fps).round().max(0.0) as i64;
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
    let mut enc = Encoder::open(&pr, output, Some(j0 as f64 / fps))?;
    let sound = if pr.has_audio { Sound::load(input).ok() } else { None };
    let mut engine = Engine::new();
    (engine.lut_in, engine.lut_out) = luts;
    let mut run = Runner::start(pr, Some(engine), sound, j0 as f64 / fps, &p_at)?;
    let mut done = 0;
    while done < total && !cancel.load(Ordering::Relaxed) {
        let p = p_at(run.out_j);
        if !run.next(&p)? {
            break;
        }
        enc.write(&run.screen)?;
        done += 1;
        progress(done, total);
    }
    enc.finish()?;
    Ok(())
}
