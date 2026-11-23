//! The offline render of a recorded take, and its progress window.

use super::link::{lock, Link};
use crate::engine::{lut::Lut, overlay::{Clip, N_OVERLAYS}};
use crate::render::{render, Knobs};
use crate::runner::Automation;
use crate::ui::{ACCENT, REC};
use eframe::egui;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct RenderJob {
    progress: Arc<Mutex<(i64, i64)>>,
    result: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    cancel: Arc<AtomicBool>,
    /// The outcome, once it has been picked up from the worker.
    done: Option<Result<PathBuf, String>>,
}

impl RenderJob {
    pub fn start(auto: Automation, out: PathBuf, link: Link) -> Self {
        let job = Self {
            progress: Arc::new(Mutex::new((0, auto.frames.len() as i64))),
            result: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
            done: None,
        };
        let (progress, result, cancel) = (job.progress.clone(), job.result.clone(), job.cancel.clone());
        std::thread::spawn(move || {
            // Without an outcome the window would stay open for ever, with nothing to click.
            let r = catch_unwind(AssertUnwindSafe(|| run(&auto, &out, &cancel, &progress, &link)))
                .unwrap_or_else(|_| Err("Le rendu s'est arrêté sur une erreur interne.".to_string()));
            *lock(&result) = Some(r.map(|()| out));
        });
        job
    }

    /// Shows the progress; true once the user has dismissed the result.
    pub fn window(&mut self, ctx: &egui::Context) -> bool {
        if self.done.is_none() {
            self.done = lock(&self.result).take();
        }
        let (d, n) = *lock(&self.progress);
        let mut close = false;
        let (done, cancel) = (&self.done, &self.cancel);
        egui::Window::new("Rendu").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| match done {
            None => {
                ui.add(egui::ProgressBar::new(d as f32 / n.max(1) as f32).text(format!("{d} / {n} images")).fill(ACCENT).desired_width(320.0));
                if ui.button("Annuler").clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Some(Ok(p)) => {
                ui.label(format!("Terminé : {}", p.display()));
                close = ui.button("OK").clicked();
            }
            Some(Err(e)) => {
                ui.colored_label(REC, e);
                close = ui.button("OK").clicked();
            }
        });
        close
    }
}

/// The render itself. What can't be reloaded is left out, as before, but now said.
fn run(auto: &Automation, out: &Path, cancel: &AtomicBool, progress: &Mutex<(i64, i64)>, link: &Link) -> Result<(), String> {
    let mut problems = Vec::new();
    let json = out.with_extension("automation.json");
    let saved = serde_json::to_string_pretty(auto).map_err(|e| e.to_string()).and_then(|s| std::fs::write(&json, s).map_err(|e| e.to_string()));
    if let Err(e) = saved {
        problems.push(format!("{} non écrit ({e})", json.display()));
    }
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
    let r = render(Path::new(&auto.source), out, Knobs::Recorded(auto), auto.start, None, luts, clips, cancel, &mut |d, n| {
        *lock(progress) = (d, n);
    });
    if !problems.is_empty() {
        link.say(format!("Rendu : {}", problems.join(" ; ")));
    }
    r.map_err(|e| format!("{e:#}"))?;
    // render() closes the file normally when cancelled: it must not read as a finished take.
    if cancel.load(Ordering::Relaxed) {
        return Err(format!("Rendu annulé : {} est incomplet.", out.display()));
    }
    Ok(())
}
