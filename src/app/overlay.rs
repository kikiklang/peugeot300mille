//! The video-loop slots: choosing the file, the part that loops, its length in beats, download.

use super::link::{lock, Link};
use crate::download::{Download, DownloadSlot};
use crate::engine::overlay::{loop_lengths, Clip, MAX_SECONDS, MODES, MODE_HELP, N_OVERLAYS, PLACES};
use crate::engine::signal::TV_FPS;
use crate::knob::Knob;
use crate::params::{KnobRow, Params};
use crate::player::{Cmd, OverlayInfo};
use crate::ui::{self, knob_grid, ACCENT, WEAK};
use eframe::egui::{self, RichText};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const LOOP_FILES: [&str; 8] = ["mp4", "mov", "mkv", "webm", "avi", "gif", "png", "m4v"];

macro_rules! overlay_knobs {
    ($opacity:ident, $size:ident, $x:ident, $y:ident, $speed:ident) => {
        [
            (Knob::$opacity, "Opacité", "Transparence du calque."),
            (Knob::$size, "Taille", "Hauteur du calque par rapport à l'image."),
            (Knob::$x, "Position X", "Gauche (-1) à droite (+1)."),
            (Knob::$y, "Position Y", "Bas (-1) à haut (+1)."),
            (Knob::$speed, "Vitesse", "Vitesse de lecture de la boucle (négatif = à l'envers)."),
        ]
    };
}
const OVERLAY_KNOBS: [[KnobRow; 5]; N_OVERLAYS] = [
    overlay_knobs!(Ov1Opacity, Ov1Size, Ov1X, Ov1Y, Ov1Speed),
    overlay_knobs!(Ov2Opacity, Ov2Size, Ov2X, Ov2Y, Ov2Speed),
    overlay_knobs!(Ov3Opacity, Ov3Size, Ov3X, Ov3Y, Ov3Speed),
    overlay_knobs!(Ov4Opacity, Ov4Size, Ov4X, Ov4Y, Ov4Speed),
    overlay_knobs!(Ov5Opacity, Ov5Size, Ov5X, Ov5Y, Ov5Speed),
    overlay_knobs!(Ov6Opacity, Ov6Size, Ov6X, Ov6Y, Ov6Speed),
    overlay_knobs!(Ov7Opacity, Ov7Size, Ov7X, Ov7Y, Ov7Speed),
    overlay_knobs!(Ov8Opacity, Ov8Size, Ov8X, Ov8Y, Ov8Speed),
];

/// The background loads of a slot. Decoding takes seconds and can finish in any order: only the
/// latest request may reach the player, so a slow old load can't replace a newer loop or refill
/// a slot that was emptied meanwhile.
#[derive(Default)]
pub struct Loads {
    latest: AtomicU64,
    /// The load bringing a new file, if one is running (0 = none).
    new_file: AtomicU64,
    /// A new loop has landed: the panel must lower its opacity too (see `spawn`).
    hide: AtomicBool,
}

impl Loads {
    /// Numbers a new request. A new part of the old file is pointless while a new file is coming.
    fn begin(&self, replaces_file: bool) -> Option<u64> {
        if !replaces_file && self.new_file.load(Ordering::SeqCst) != 0 {
            return None;
        }
        let id = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        if replaces_file {
            self.new_file.store(id, Ordering::SeqCst);
        }
        Some(id)
    }

    /// Whether this request is still wanted; ends it either way.
    fn finish(&self, id: u64) -> bool {
        let _ = self.new_file.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
        self.latest.load(Ordering::SeqCst) == id
    }

    /// Whatever is running no longer counts.
    fn cancel(&self) {
        self.latest.fetch_add(1, Ordering::SeqCst);
        self.new_file.store(0, Ordering::SeqCst);
    }

    /// Loads (part of) a loop in the background, then hands it to the player. With `hide`, the
    /// loop arrives invisible: raise its opacity when you want it. The opacity only changes once
    /// the load has worked, so a bad file leaves the current loop as it was.
    pub fn spawn(self: &Arc<Self>, slot: usize, path: PathBuf, range: Option<(f64, f64)>, hide: bool, link: &Link) {
        let Some(id) = self.begin(range.is_none()) else { return };
        link.say("Chargement de la boucle…");
        let (loads, link) = (self.clone(), link.clone());
        std::thread::spawn(move || {
            let (s, e) = range.map_or((0.0, None), |(s, e)| (s, Some(e)));
            let clip = Clip::load_range(&path, s, e);
            if !loads.finish(id) {
                return;
            }
            match clip {
                Ok(c) => {
                    link.say(format!("Boucle chargée : {} ({:.1} s)", c.name, c.end - c.start));
                    if hide {
                        // Both: the flag covers a panel that is about to write back its own copy.
                        loads.hide.store(true, Ordering::SeqCst);
                        lock(&link.shared.params).overlays[slot].opacity = 0.0;
                    }
                    link.send(Cmd::SetOverlay { slot, clip: Some(Arc::new(c)) });
                }
                Err(e) => link.say(format!("{e:#}")),
            }
        });
    }
}

/// What the panel remembers about a slot between frames.
#[derive(Default)]
pub struct OverlaySlot {
    /// Start/end being edited, before the loop is reloaded.
    edit: Option<(f64, f64)>,
    /// The file that edit is about.
    edit_of: Option<PathBuf>,
    /// Link and running download.
    dl: DownloadSlot,
    /// The length last asked for to follow the tempo: one request each, even if it fails.
    asked: Option<f64>,
    pub loads: Arc<Loads>,
}

/// One slot, for one frame.
pub struct OverlayPanel<'a> {
    pub slot: usize,
    pub p: &'a mut Params,
    pub info: Option<&'a OverlayInfo>,
    pub state: &'a mut OverlaySlot,
    pub link: &'a Link,
    pub sound: bool,
    /// The track's tempo, with ÷2 / ×2 applied.
    pub bpm: Option<f32>,
}

impl OverlayPanel<'_> {
    /// Runs every frame, whichever tab is showing, so that a download lands and the loop keeps
    /// its length in beats while the slot is folded or out of sight.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.state.loads.hide.swap(false, Ordering::SeqCst) {
            self.p.overlays[self.slot].opacity = 0.0;
        }
        match self.info {
            // A range only means something for the file it was picked on.
            Some(i) if self.state.edit_of.as_ref() != Some(&i.path) => {
                self.state.edit = None;
                self.state.edit_of = Some(i.path.clone());
                self.state.asked = None;
            }
            None => {
                self.state.edit = None;
                self.state.edit_of = None;
                self.state.asked = None;
            }
            _ => {}
        }
        self.follow_tempo();
        if let Some(job) = &self.state.dl.job {
            ctx.request_repaint_after(Duration::from_millis(200));
            match job.take_result() {
                Some(Ok(path)) => {
                    self.state.dl.job = None;
                    self.state.dl.url.clear();
                    self.replace_file(path);
                }
                Some(Err(e)) => {
                    self.state.dl.job = None;
                    self.link.say(e);
                }
                None => {}
            }
        }
    }

    /// A loop set to a number of beats takes that length, also when the tempo is halved or doubled.
    fn follow_tempo(&mut self) {
        let beats = self.p.overlays[self.slot].beats;
        let (Some(i), Some(bpm), true, None) = (self.info, self.bpm, beats > 0, self.state.edit) else { return };
        let (a, b) = beat_span(i.start, beats as f64 * 60.0 / bpm as f64, i.duration, MAX_SECONDS);
        if (i.end - i.start - (b - a)).abs() <= 0.005 {
            self.state.asked = None;
        } else if self.state.asked.is_none_or(|x| (x - (b - a)).abs() > 0.002) {
            self.state.asked = Some(b - a);
            self.state.loads.spawn(self.slot, i.path.clone(), Some((a, b)), false, self.link);
        }
    }

    /// A new file for this slot.
    fn replace_file(&mut self, path: PathBuf) {
        self.state.edit = None;
        self.state.loads.spawn(self.slot, path, None, true, self.link);
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let n = self.slot + 1;
        let loaded = self.info.is_some();
        let title = match self.info {
            Some(i) => RichText::new(format!("Calque {n} — {}", i.name)).color(ACCENT),
            None => RichText::new(format!("Calque {n} — vide")).color(WEAK),
        };
        // A separate state for "empty" and "loaded", so a slot opens by itself when a loop arrives.
        egui::CollapsingHeader::new(title).id_salt(("overlay", self.slot, loaded)).default_open(loaded).show(ui, |ui| {
            ui.horizontal(|ui| self.file_row(ui));
            download_ui(ui, &mut self.state.dl, self.link);
            let Some(info) = self.info else { return };
            self.range_row(ui, info);
            self.selectors(ui);
            knob_grid(ui, ("ovknobs", self.slot), self.p, &OVERLAY_KNOBS[self.slot], self.sound);
        });
    }

    fn file_row(&mut self, ui: &mut egui::Ui) {
        let loaded = self.info.is_some();
        if ui.button(if loaded { "Changer…" } else { "Charger une boucle…" }).clicked() {
            if let Some(f) = rfd::FileDialog::new().add_filter("Vidéo / image", &LOOP_FILES).pick_file() {
                self.replace_file(f);
            }
        }
        if loaded && ui.small_button("Retirer").clicked() {
            // A load still running must not bring the loop back.
            self.state.loads.cancel();
            self.state.edit = None;
            self.link.send(Cmd::SetOverlay { slot: self.slot, clip: None });
            self.link.say(format!("Calque {} retiré.", self.slot + 1));
        }
    }

    /// Which part of the file loops.
    fn range_row(&mut self, ui: &mut egui::Ui, info: &OverlayInfo) {
        let (mut a, mut b) = self.state.edit.unwrap_or((info.start, info.end));
        let release = ui::range_bar(ui, egui::Id::new(("ovrange", self.slot)), &mut a, &mut b, info.duration, MAX_SECONDS, 1.0 / TV_FPS);
        if (a - info.start).abs() > 1e-3 || (b - info.end).abs() > 1e-3 {
            self.state.edit = Some((a, b));
            if release {
                // Resizing by hand takes the loop off the tempo; moving it whole doesn't.
                if ((b - a) - (info.end - info.start)).abs() > 0.01 {
                    self.p.overlays[self.slot].beats = 0;
                }
                self.state.loads.spawn(self.slot, info.path.clone(), Some((a, b)), false, self.link);
            }
        } else {
            self.state.edit = None;
        }
    }

    fn selectors(&mut self, ui: &mut egui::Ui) {
        let (slot, bpm, meter) = (self.slot, self.bpm, self.p.tempo_meter);
        let o = &mut self.p.overlays[slot];
        egui::Grid::new(("ovsel", slot)).num_columns(2).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
            ui.label("Placement");
            egui::ComboBox::from_id_salt(("ovplace", slot)).selected_text(PLACES[o.place as usize]).show_ui(ui, |ui| {
                ui.selectable_value(&mut o.place, 0, PLACES[0]).on_hover_text("Avant le circuit : ampli, VHS, télé et caméra l'abîment aussi.");
                ui.selectable_value(&mut o.place, 1, PLACES[1]).on_hover_text("Par-dessus l'image finie, intact.");
            });
            ui.end_row();
            ui.label("Mode");
            egui::ComboBox::from_id_salt(("ovmode", slot)).selected_text(MODES[o.mode as usize]).height(420.0).show_ui(ui, |ui| {
                for (i, m) in MODES.iter().enumerate() {
                    ui.selectable_value(&mut o.mode, i as u8, *m).on_hover_text(MODE_HELP[i]);
                }
            })
            .response
            .on_hover_text(MODE_HELP[o.mode as usize]);
            ui.end_row();
            ui.label("Retourner");
            ui.horizontal(|ui| {
                ui.toggle_value(&mut o.flip_h, "Miroir");
                ui.toggle_value(&mut o.flip_v, "Renverser");
            });
            ui.end_row();
            ui.label("Longueur");
            ui.add_enabled_ui(bpm.is_some(), |ui| {
                egui::ComboBox::from_id_salt(("ovbeats", slot)).selected_text(beats_label(o.beats, meter)).show_ui(ui, |ui| {
                    for n in loop_lengths(meter) {
                        // A loop holds at most MAX_SECONDS.
                        let fits = n == 0 || bpm.is_some_and(|b| n as f64 * 60.0 / b as f64 <= MAX_SECONDS);
                        ui.add_enabled_ui(fits, |ui| {
                            ui.selectable_value(&mut o.beats, n, beats_label(n, meter));
                        });
                    }
                })
                .response
                .on_hover_text("Cale la boucle sur le tempo du morceau : elle dure ce nombre de temps et repart sur le temps, à partir du début de la sélection.")
                .on_disabled_hover_text("Aucun tempo détecté dans le son.");
            });
            ui.end_row();
        });
    }
}

fn beats_label(n: u8, meter: u8) -> String {
    match n {
        0 => "Libre".to_string(),
        n if n % meter == 0 && n / meter == 1 => format!("{n} temps (1 mesure)"),
        n if n % meter == 0 => format!("{n} temps ({} mesures)", n / meter),
        n => format!("{n} temps"),
    }
}

/// The part of the file for a loop of this length from `start`, kept inside the file.
fn beat_span(start: f64, len: f64, duration: f64, max_len: f64) -> (f64, f64) {
    let len = len.clamp(0.05, max_len.min(duration).max(0.05));
    let s = start.min((duration - len).max(0.0));
    (s, s + len)
}

/// The link field of an overlay slot, or the progress of its download.
fn download_ui(ui: &mut egui::Ui, dl: &mut DownloadSlot, link: &Link) {
    ui.horizontal(|ui| {
        if let Some(job) = &dl.job {
            let f = *lock(&job.progress);
            let text = f.map_or("Préparation…".to_string(), |f| format!("Téléchargement {:.0} %", f * 100.0));
            ui.add(egui::ProgressBar::new(f.unwrap_or(0.0)).desired_width(240.0).text(text));
            if ui.small_button("Annuler").clicked() {
                job.cancel();
            }
            return;
        }
        let field = ui.add(egui::TextEdit::singleline(&mut dl.url).hint_text("Coller un lien YouTube…").desired_width(240.0));
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let ready = !dl.url.trim().is_empty();
        let clicked = ui.add_enabled(ready, egui::Button::new("Télécharger")).on_hover_text("Enregistre la vidéo dans Vidéos/peugeot300mille, puis la charge dans ce calque.").clicked();
        if ready && (enter || clicked) {
            link.say("Téléchargement de la vidéo…");
            dl.job = Some(Download::start(&dl.url));
        }
    });
}

#[cfg(test)]
mod load_order {
    use super::*;

    #[test]
    fn only_the_latest_request_lands() {
        let l = Loads::default();
        let old = l.begin(false).unwrap();
        let new = l.begin(false).unwrap();
        assert!(!l.finish(old));
        assert!(l.finish(new));
    }

    #[test]
    fn removing_a_slot_drops_the_load_in_flight() {
        let l = Loads::default();
        let id = l.begin(true).unwrap();
        l.cancel();
        assert!(!l.finish(id));
        assert!(l.begin(false).is_some(), "the slot is usable again");
    }

    #[test]
    fn a_new_file_is_not_replaced_by_a_range_of_the_old_one() {
        let l = Loads::default();
        let file = l.begin(true).unwrap();
        assert_eq!(l.begin(false), None);
        assert!(l.finish(file));
        assert!(l.begin(false).is_some());
    }
}

#[cfg(test)]
mod beat_length {
    use super::*;

    #[test]
    fn the_loop_stays_inside_the_file() {
        assert_eq!(beat_span(10.0, 2.0, 100.0, 20.0), (10.0, 12.0));
        assert_eq!(beat_span(99.0, 2.0, 100.0, 20.0), (98.0, 100.0));
        assert_eq!(beat_span(0.0, 50.0, 100.0, 20.0), (0.0, 20.0));
    }

    #[test]
    fn a_loop_set_in_beats_takes_that_length_once() {
        let dir = std::env::temp_dir().join(format!("beatlen{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.mp4");
        assert!(std::process::Command::new("ffmpeg").args(["-nostdin", "-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=64x48:r=30:d=6", "-pix_fmt", "yuv420p"]).arg(&f).status().unwrap().success());
        let info = OverlayInfo { name: "c".into(), path: f, start: 1.0, end: 5.0, duration: 6.0 };
        let (tx, rx) = std::sync::mpsc::channel();
        let link = Link { tx, shared: Arc::new(crate::player::Shared::new()) };
        let mut p = Params::default();
        p.overlays[0].beats = 4;
        let mut state = OverlaySlot::default();
        let ctx = egui::Context::default();
        for _ in 0..3 {
            OverlayPanel { slot: 0, p: &mut p, info: Some(&info), state: &mut state, link: &link, sound: true, bpm: Some(120.0) }.poll(&ctx);
        }
        let Ok(Cmd::SetOverlay { clip: Some(c), .. }) = rx.recv_timeout(Duration::from_secs(8)) else { panic!("no loop") };
        assert!((c.start - 1.0).abs() < 1e-6 && (c.end - c.start - 2.0).abs() < 1e-6, "{}..{}", c.start, c.end);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "asked more than once");
        let _ = std::fs::remove_dir_all(dir);
    }
}
