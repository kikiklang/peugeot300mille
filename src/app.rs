//! The desktop window.

mod job;
mod link;
mod overlay;
mod sound;
mod tabs;
mod transport;
mod view;

use crate::engine::overlay::N_OVERLAYS;
use crate::engine::signal::{H, W};
use crate::media::FRAME_BYTES;
use crate::params::Params;
use crate::player::{self, Cmd, Shared};
use crate::ui::{self, ACCENT, WEAK};
use anyhow::{anyhow, Result};
use eframe::egui::{self, RichText};
use job::RenderJob;
use link::{lock, Link};
use overlay::{OverlayPanel, OverlaySlot};
use sound::BandPoints;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::Arc;
use tabs::ImageTab;
use view::View;

pub fn run(input: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 880.0])
            .with_min_inner_size([1000.0, 600.0])
            .with_title("peugeot300mille")
            // Matches the .desktop file, so the dock shows the right icon.
            .with_app_id("peugeot300mille")
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native("peugeot300mille", options, Box::new(move |cc| Ok(Box::new(App::new(cc, input)))))
        .map_err(|e| anyhow!("{e}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Image,
    Camera,
    Sound,
}

struct App {
    tab: Tab,
    /// What each overlay slot remembers between frames.
    slots: [OverlaySlot; N_OVERLAYS],
    /// How many overlay slots the panel shows (at least those in use).
    shown_slots: usize,
    bands: BandPoints,
    link: Link,
    /// This frame's copy of the player's status.
    view: View,
    tex: Option<egui::TextureHandle>,
    seen: u64,
    seek: Option<f64>,
    job: Option<RenderJob>,
    /// Development aid: PEUGEOT300MILLE_SCREENSHOT=out.ppm captures this window and quits.
    shot: Option<(String, u32)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, input: Option<PathBuf>) -> Self {
        ui::setup(&cc.egui_ctx);
        let shared = Arc::new(Shared::new());
        let (tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        player::spawn(shared.clone(), rx, move || ctx.request_repaint());
        let link = Link { tx, shared };
        if let Some(p) = input {
            link.send(Cmd::Open(p));
        }
        let tab = match std::env::var("PEUGEOT300MILLE_TAB").as_deref() {
            Ok("camera") => Tab::Camera,
            Ok("sound") => Tab::Sound,
            _ => Tab::Image,
        };
        if let Some(q) = std::env::var("PEUGEOT300MILLE_PARAMS").ok().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| serde_json::from_str::<Params>(&s).ok()) {
            *lock(&link.shared.params) = q.clamped();
        }
        let shot = std::env::var("PEUGEOT300MILLE_SCREENSHOT").ok().map(|p| (p, 0));
        let slots: [OverlaySlot; N_OVERLAYS] = Default::default();
        if let Ok(f) = std::env::var("PEUGEOT300MILLE_OVERLAY1") {
            slots[0].loads.spawn(0, PathBuf::from(f), None, false, &link);
        }
        Self { tab, slots, shown_slots: 1, bands: BandPoints::default(), link, view: View::default(), tex: None, seen: 0, seek: None, job: None, shot }
    }

    /// Development aid, see `shot`.
    fn capture_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, n)) = self.shot.as_mut() else { return };
        *n += 1;
        ctx.request_repaint();
        if *n == 90 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let mut out = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
            for px in &img.pixels {
                out.extend_from_slice(&[px.r(), px.g(), px.b()]);
            }
            if let Err(e) = std::fs::write(&*path, out) {
                eprintln!("{path}: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Picks up a new screen from the player.
    fn update_texture(&mut self, ctx: &egui::Context) {
        let img = {
            let s = lock(&self.link.shared.screen);
            // from_rgb panics on any other size.
            if s.0 == self.seen || s.1.len() != FRAME_BYTES {
                return;
            }
            self.seen = s.0;
            egui::ColorImage::from_rgb([W, H], &s.1)
        };
        match &mut self.tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("screen", img, egui::TextureOptions::LINEAR)),
        }
    }

    fn side_panel(&mut self, ctx: &egui::Context) {
        let Self { tab, slots, shown_slots, bands, link, view: st, .. } = self;
        let side = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 12));
        egui::SidePanel::left("knobs").resizable(false).exact_width(480.0).frame(side).show(ctx, |ui| {
            let old = *lock(&link.shared.params);
            let mut p = old;
            for (slot, state) in slots.iter_mut().enumerate() {
                let bpm = st.bpm.map(|b| b * 2f32.powi(p.tempo_octave as i32));
                OverlayPanel { slot, p: &mut p, info: st.overlays[slot].as_ref(), state, link, sound: st.has_sound, bpm }.poll(ctx);
            }
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 16.0) / 3.0;
                tab_button(ui, tab, Tab::Image, "Image", w);
                tab_button(ui, tab, Tab::Camera, "Caméra", w);
                tab_button(ui, tab, Tab::Sound, "Son", w);
            });
            ui.add_space(10.0);
            egui::TopBottomPanel::bottom("panel_footer").frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 8))).show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Tout à 0").on_hover_text("Remet tous les boutons au départ (passage propre).").clicked() {
                        p = Params::default();
                    }
                    if ui.button("Sauver…").clicked() {
                        save_settings(&p, link);
                    }
                    if ui.button("Charger…").clicked() {
                        if let Some(q) = load_settings(link) {
                            p = q;
                        }
                    }
                });
                ui.label(RichText::new("Double-clic sur un curseur : le remettre à zéro. En pause, un réglage relance 8 images de chauffe.").small().color(WEAK));
            });
            egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match *tab {
                    Tab::Image => tabs::image_tab(ui, &mut ImageTab { p: &mut p, st, slots, shown: shown_slots, link }),
                    Tab::Camera => tabs::camera_tab(ui, &mut p, st),
                    Tab::Sound => sound::sound_tab(ui, &mut p, st, bands),
                });
            });
            if p != old {
                *lock(&link.shared.params) = p;
            }
        });
    }

    fn picture(&self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(ui::BG)).show(ctx, |ui| {
            if let Some(tex) = &self.tex {
                let aspect = self.view.file.and_then(|f| f.aspect()).unwrap_or(4.0 / 3.0);
                let avail = (ui.available_size() - egui::vec2(32.0, 32.0)).max(egui::Vec2::ZERO);
                let mut size = egui::vec2(avail.x, avail.x / aspect);
                if size.y > avail.y {
                    size = egui::vec2(avail.y * aspect, avail.y);
                }
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), size)));
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Ouvre une vidéo (bouton Ouvrir… en haut à gauche)").color(WEAK).size(20.0));
                });
            }
        });
    }
}

fn save_settings(p: &Params, link: &Link) {
    let Some(f) = rfd::FileDialog::new().set_file_name("reglage.json").add_filter("JSON", &["json"]).save_file() else { return };
    let saved = serde_json::to_string_pretty(p).map_err(|e| e.to_string()).and_then(|s| std::fs::write(&f, s).map_err(|e| e.to_string()));
    link.say(match saved {
        Ok(()) => format!("Réglages enregistrés : {}", f.display()),
        Err(e) => format!("Impossible d'enregistrer {} : {e}", f.display()),
    });
}

/// The chosen settings, or None (and a message) if the file can't be used.
fn load_settings(link: &Link) -> Option<Params> {
    let f = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file()?;
    let loaded = std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str::<Params>(&s).map_err(|e| e.to_string()));
    match loaded {
        Ok(q) => {
            link.say(format!("Réglages chargés : {}", f.display()));
            Some(q.clamped())
        }
        Err(e) => {
            link.say(format!("Impossible de charger {} : {e}", f.display()));
            None
        }
    }
}

fn tab_button(ui: &mut egui::Ui, tab: &mut Tab, this: Tab, label: &str, w: f32) {
    let sel = *tab == this;
    let text = RichText::new(label).size(15.0).color(if sel { egui::Color32::BLACK } else { ui::TEXT });
    let b = egui::Button::new(text).fill(if sel { ACCENT } else { egui::Color32::from_rgb(40, 44, 52) }).corner_radius(8);
    if ui.add_sized([w, 34.0], b).clicked() {
        *tab = this;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.capture_screenshot(ctx);
        self.view.refresh(&lock(&self.link.shared.status));
        self.update_texture(ctx);
        if self.view.file.is_some() && ctx.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.wants_keyboard_input() {
            self.link.send(if self.view.playing { Cmd::Pause } else { Cmd::Play });
        }
        self.transport_bar(ctx);
        self.status_bar(ctx);
        self.side_panel(ctx);
        self.picture(ctx);
        if self.job.as_mut().is_some_and(|job| job.window(ctx)) {
            self.job = None;
        }
    }
}
