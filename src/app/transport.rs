//! The top bar (open, play, timeline, recording) and the status line.

use super::job::RenderJob;
use super::link::lock;
use super::App;
use crate::player::Cmd;
use crate::ui::{ACCENT, REC, WEAK};
use eframe::egui::{self, RichText};

const VIDEO_FILES: [&str; 8] = ["mp4", "mov", "mkv", "avi", "webm", "m4v", "mpg", "mts"];
const MUSIC_FILES: [&str; 8] = ["mp3", "wav", "flac", "ogg", "opus", "m4a", "aac", "aiff"];

/// m:ss.cc, rounded as a whole so that 59.999 s reads 1:00.00 and not 0:60.00.
pub(super) fn fmt_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}.{:02}", cs / 6000, cs / 100 % 60, cs % 100)
}

impl App {
    pub(super) fn transport_bar(&mut self, ctx: &egui::Context) {
        let loaded = self.view.file.is_some();
        let mut render_clicked = false;
        let bar = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 10));
        egui::TopBottomPanel::top("transport").frame(bar).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Ouvrir…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Vidéo ou musique", &[VIDEO_FILES, MUSIC_FILES].concat())
                        .add_filter("Vidéo", &VIDEO_FILES)
                        .add_filter("Musique", &MUSIC_FILES)
                        .pick_file() {
                        self.link.send(Cmd::Open(p));
                    }
                }
                ui.add_space(6.0);
                ui.add_enabled_ui(loaded, |ui| {
                    let st = &self.view;
                    let play = egui::Button::new(RichText::new(if st.playing { "⏸" } else { "▶" }).size(18.0).color(egui::Color32::BLACK)).fill(ACCENT).corner_radius(16);
                    if ui.add_sized([44.0, 32.0], play).on_hover_text("Lecture / pause (Espace)").clicked() {
                        self.link.send(if st.playing { Cmd::Pause } else { Cmd::Play });
                    }
                    let dur = st.file.map_or(1.0, |f| f.timeline());
                    let mut t = self.seek.unwrap_or(st.time);
                    ui.spacing_mut().slider_width = (ui.available_width() - 560.0).max(160.0);
                    let resp = ui.add(egui::Slider::new(&mut t, 0.0..=dur).show_value(false));
                    if resp.dragged() || resp.changed() {
                        self.seek = Some(t);
                    }
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        self.link.send(Cmd::Seek(t));
                        self.seek = None;
                    }
                    ui.label(RichText::new(format!("{} / {}", fmt_time(t), fmt_time(dur))).monospace());
                    ui.add_space(10.0);
                    let live = st.live_rec;
                    let live_txt = if live { RichText::new("⏹  Arrêter").color(REC) } else { RichText::new("⏺  En direct…") };
                    if ui.add_enabled(!st.auto_rec, egui::Button::new(live_txt)).on_hover_text("Enregistre ce qui est joué dans un fichier vidéo, avec le son d'origine.").clicked() {
                        if live {
                            self.link.send(Cmd::RecLive(None));
                        } else if let Some(p) = rfd::FileDialog::new().set_file_name("peugeot300mille-live.mp4").add_filter("MP4", &["mp4"]).save_file() {
                            self.link.send(Cmd::RecLive(Some(p)));
                        }
                    }
                    let auto_txt = if st.auto_rec { RichText::new(format!("⏹  Stop ({} img)", st.auto_frames)).color(ACCENT) } else { RichText::new("⏺  Réglages") };
                    if ui.add_enabled(!live, egui::Button::new(auto_txt)).on_hover_text("Joue la vidéo et retient chaque mouvement de bouton, pour un rendu propre ensuite.").clicked() {
                        self.link.send(Cmd::RecAuto(!st.auto_rec));
                    }
                    let can_render = st.has_automation && self.job.is_none();
                    render_clicked = ui.add_enabled(can_render, egui::Button::new("Rendre…")).on_hover_text("Rendu hors ligne du passage enregistré, identique à ce que vous avez joué.").clicked();
                });
            });
        });
        if render_clicked {
            self.start_render();
        }
    }

    fn start_render(&mut self) {
        let Some(out) = rfd::FileDialog::new()
            .set_title("Rendre l'enregistrement")
            .set_file_name("peugeot300mille.mp4")
            .add_filter("MP4", &["mp4"])
            .save_file()
        else {
            return;
        };
        // Cloned only here: a take can hold thousands of frames.
        let auto = lock(&self.link.shared.status).automation.clone();
        match auto {
            Some(a) => self.job = Some(RenderJob::start(a, out, self.link.clone())),
            None => self.link.say("Plus d'enregistrement à rendre."),
        }
    }

    pub(super) fn status_bar(&self, ctx: &egui::Context) {
        let st = &self.view;
        let status = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 6));
        egui::TopBottomPanel::bottom("status").frame(status).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if st.live_rec {
                    ui.label(RichText::new("● REC").color(REC).strong());
                }
                if st.auto_rec {
                    ui.label(RichText::new("● RÉGLAGES").color(ACCENT).strong());
                }
                if st.file.is_some() {
                    ui.label(RichText::new(format!("{:4.1} i/s", st.fps)).monospace().color(WEAK));
                    ui.label(RichText::new(format!("verrou H {:3.0} %", st.h_lock * 100.0)).monospace().color(WEAK));
                    ui.separator();
                }
                ui.label(RichText::new(&st.message).color(WEAK));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_time;

    #[test]
    fn times_never_show_sixty_seconds() {
        assert_eq!(fmt_time(0.0), "0:00.00");
        assert_eq!(fmt_time(61.5), "1:01.50");
        assert_eq!(fmt_time(59.999), "1:00.00");
        assert_eq!(fmt_time(-3.0), "0:00.00");
        assert_eq!(fmt_time(f64::NAN), "0:00.00");
    }
}
