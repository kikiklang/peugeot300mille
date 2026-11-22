//! The desktop window.

use crate::audio::{bin_freq, freq_bin, N_BANDS, N_GLOBAL, SOURCES, SPEC_BINS};
use crate::engine::{lut::Lut, overlay::{Clip, MAX_SECONDS, MODES, PLACES}};
use crate::params::{Band, Params, AMP_KNOBS, BAND_HINTS, CAMERAS, CAM_GROUPS, CAM_KNOBS, INPUT_KNOBS, KNOBS, MIRRORS, SOUND_PRESETS, VHS_KNOBS};
use crate::player::{self, Cmd, OverlayInfo, Shared, Status};
use crate::render::{render, Knobs};
use crate::runner::Automation;
use crate::ui::{self, card, knob_grid, ACCENT, REC, WEAK};
use anyhow::{anyhow, Result};
use eframe::egui::{self, RichText};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

pub fn run(input: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 880.0]).with_min_inner_size([1000.0, 600.0]).with_title("glitchbox"),
        ..Default::default()
    };
    eframe::run_native("glitchbox", options, Box::new(move |cc| Ok(Box::new(App::new(cc, input)))))
        .map_err(|e| anyhow!("{e}"))
}

struct RenderJob {
    progress: Arc<Mutex<(i64, i64)>>,
    result: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Image,
    Camera,
    Sound,
}

struct App {
    tab: Tab,
    /// Overlay start/end being edited, before the loop is reloaded.
    ov_edit: [Option<(f64, f64)>; 3],
    sel_band: usize,
    band_drag: Option<usize>,
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    tex: Option<egui::TextureHandle>,
    seen: u64,
    seek: Option<f64>,
    job: Option<RenderJob>,
    /// Development aid: GLITCHBOX_SCREENSHOT=out.ppm captures this window and quits.
    shot: Option<(String, u32)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, input: Option<PathBuf>) -> Self {
        ui::setup(&cc.egui_ctx);
        let shared = Arc::new(Shared::new());
        let (tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        player::spawn(shared.clone(), rx, move || ctx.request_repaint());
        if let Some(p) = input {
            let _ = tx.send(Cmd::Open(p));
        }
        let tab = match std::env::var("GLITCHBOX_TAB").as_deref() {
            Ok("camera") => Tab::Camera,
            Ok("sound") => Tab::Sound,
            _ => Tab::Image,
        };
        if let Some(q) = std::env::var("GLITCHBOX_PARAMS").ok().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| serde_json::from_str::<Params>(&s).ok()) {
            *shared.params.lock().unwrap() = q.clamped();
        }
        let shot = std::env::var("GLITCHBOX_SCREENSHOT").ok().map(|p| (p, 0));
        Self { tab, ov_edit: [None; 3], sel_band: 0, band_drag: None, shared, tx, tex: None, seen: 0, seek: None, job: None, shot }
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    fn start_render(&mut self, auto: Automation) {
        let Some(out) = rfd::FileDialog::new()
            .set_title("Rendre l'enregistrement")
            .set_file_name("glitchbox.mp4")
            .add_filter("MP4", &["mp4"])
            .save_file()
        else {
            return;
        };
        let job = RenderJob {
            progress: Arc::new(Mutex::new((0, auto.frames.len() as i64))),
            result: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (progress, result, cancel) = (job.progress.clone(), job.result.clone(), job.cancel.clone());
        std::thread::spawn(move || {
            let json = out.with_extension("automation.json");
            let _ = std::fs::write(&json, serde_json::to_string_pretty(&auto).unwrap_or_default());
            let input = PathBuf::from(&auto.source);
            let load = |p: &Option<String>| p.as_ref().and_then(|p| Lut::load(std::path::Path::new(p)).ok().map(Arc::new));
            let luts = (load(&auto.lut_in), load(&auto.lut_out));
            let mut clips: [Option<Arc<Clip>>; 3] = [None, None, None];
            for (i, c) in clips.iter_mut().enumerate() {
                if let Some(p) = &auto.overlays[i] {
                    let (s, e) = auto.overlay_ranges[i].map_or((0.0, None), |(s, e)| (s, Some(e)));
                    *c = Clip::load_range(std::path::Path::new(p), s, e).ok().map(Arc::new);
                }
            }
            let r = render(&input, &out, Knobs::Recorded(&auto), auto.start, None, luts, clips, &cancel, &mut |d, n| {
                *progress.lock().unwrap() = (d, n);
            });
            *result.lock().unwrap() = Some(r.map(|_| out).map_err(|e| format!("{e:#}")));
        });
        self.job = Some(job);
    }
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0)
}

/// LUT file chooser with its dose.
fn lut_ui(ui: &mut egui::Ui, output: bool, name: &Option<String>, amount: &mut f32, tx: &Sender<Cmd>, shared: &Shared) {
    ui.horizontal(|ui| {
        if ui.button(if name.is_some() { "Changer…" } else { "Charger une LUT…" }).on_hover_text("Fichier .cube (color.io, Resolve, Kdenlive…)").clicked() {
            if let Some(f) = rfd::FileDialog::new().add_filter("LUT .cube", &["cube", "CUBE"]).pick_file() {
                match Lut::load(&f) {
                    Ok(l) => {
                        let _ = tx.send(Cmd::SetLut { output, lut: Some(Arc::new(l)) });
                    }
                    Err(e) => shared.status.lock().unwrap().message = format!("{e:#}"),
                }
            }
        }
        match name {
            Some(n) => {
                ui.label(RichText::new(n).color(ACCENT));
                if ui.small_button("Retirer").clicked() {
                    let _ = tx.send(Cmd::SetLut { output, lut: None });
                }
            }
            None => {
                ui.label(RichText::new("aucune").color(WEAK));
            }
        }
    });
    if name.is_some() {
        ui.add(egui::Slider::new(amount, 0.0..=1.0).fixed_decimals(2).text("Dosage"));
    }
}

/// Loads (part of) a loop in the background, then hands it to the player.
fn load_overlay(slot: usize, path: PathBuf, range: Option<(f64, f64)>, tx: &Sender<Cmd>, shared: &Arc<Shared>) {
    shared.status.lock().unwrap().message = "Chargement de la boucle…".into();
    let (tx, shared) = (tx.clone(), shared.clone());
    std::thread::spawn(move || {
        let (s, e) = range.map_or((0.0, None), |(s, e)| (s, Some(e)));
        match Clip::load_range(&path, s, e) {
            Ok(c) => {
                shared.status.lock().unwrap().message = format!("Boucle chargée : {} ({:.1} s)", c.name, c.end - c.start);
                let _ = tx.send(Cmd::SetOverlay { slot, clip: Some(Arc::new(c)) });
            }
            Err(e) => shared.status.lock().unwrap().message = format!("{e:#}"),
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn overlay_ui(
    ui: &mut egui::Ui,
    slot: usize,
    p: &mut Params,
    info: &Option<OverlayInfo>,
    edit: &mut Option<(f64, f64)>,
    tx: &Sender<Cmd>,
    shared: &Arc<Shared>,
    sound: bool,
) {
    let name = info.as_ref().map(|i| i.name.clone());
    let title = match &name {
        Some(n) => RichText::new(format!("Calque {} — {n}", slot + 1)).color(ACCENT),
        None => RichText::new(format!("Calque {} — vide", slot + 1)).color(WEAK),
    };
    egui::CollapsingHeader::new(title).id_salt(("overlay", slot)).default_open(name.is_some()).show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.button(if name.is_some() { "Changer…" } else { "Charger une boucle…" }).clicked() {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("Vidéo / image", &["mp4", "mov", "mkv", "webm", "avi", "gif", "png", "m4v"])
                    .pick_file()
                {
                    *edit = None;
                    load_overlay(slot, f, None, tx, shared);
                }
            }
            if name.is_some() && ui.small_button("Retirer").clicked() {
                let _ = tx.send(Cmd::SetOverlay { slot, clip: None });
            }
        });
        let Some(info) = info else { return };
        // Which part of the file loops.
        let (mut a, mut b) = edit.unwrap_or((info.start, info.end));
        let mut release = false;
        egui::Grid::new(("ovrange", slot)).num_columns(2).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
            // Both sliders share the same fixed scale (the whole file); the other end only moves
            // when it has to (end before start, or a loop longer than MAX_SECONDS).
            let dur = info.duration.max(0.2);
            ui.label("Début");
            let r = ui.add(egui::Slider::new(&mut a, 0.0..=dur - 0.1).suffix(" s").fixed_decimals(2))
                .on_hover_text("Où la boucle commence dans le fichier.");
            release |= r.drag_stopped() || r.lost_focus();
            if r.changed() {
                if b < a + 0.1 {
                    b = (a + 0.1).min(dur);
                }
                if b > a + MAX_SECONDS {
                    b = a + MAX_SECONDS;
                }
            }
            ui.end_row();
            ui.label("Fin");
            let r = ui.add(egui::Slider::new(&mut b, 0.1..=dur).suffix(" s").fixed_decimals(2))
                .on_hover_text(format!("Où la boucle s'arrête. {} s au plus après le début.", MAX_SECONDS));
            release |= r.drag_stopped() || r.lost_focus();
            if r.changed() {
                if a > b - 0.1 {
                    a = (b - 0.1).max(0.0);
                }
                if a < b - MAX_SECONDS {
                    a = b - MAX_SECONDS;
                }
            }
            ui.end_row();
        });
        ui.label(RichText::new(format!("Boucle de {:.2} s, fichier de {:.1} s", b - a, info.duration)).small().color(WEAK));
        if (a - info.start).abs() > 1e-3 || (b - info.end).abs() > 1e-3 {
            *edit = Some((a, b));
            if release {
                load_overlay(slot, info.path.clone(), Some((a, b)), tx, shared);
            }
        } else {
            *edit = None;
        }
        let o = &mut p.overlays[slot];
        egui::Grid::new(("ovsel", slot)).num_columns(2).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
            ui.label("Placement");
            egui::ComboBox::from_id_salt(("ovplace", slot)).selected_text(PLACES[o.place as usize]).show_ui(ui, |ui| {
                ui.selectable_value(&mut o.place, 0, PLACES[0]).on_hover_text("Avant le circuit : ampli, VHS, télé et caméra l'abîment aussi.");
                ui.selectable_value(&mut o.place, 1, PLACES[1]).on_hover_text("Par-dessus l'image finie, intact.");
            });
            ui.end_row();
            ui.label("Mode");
            egui::ComboBox::from_id_salt(("ovmode", slot)).selected_text(MODES[o.mode as usize]).show_ui(ui, |ui| {
                for (i, m) in MODES.iter().enumerate() {
                    ui.selectable_value(&mut o.mode, i as u8, *m);
                }
            })
            .response
            .on_hover_text("Incrustation du noir : le fond noir d'une boucle VJ disparaît.");
            ui.end_row();
        });
        let n = slot + 1;
        let rows: [(String, &str, &str); 5] = [
            (format!("ov{n}_opacity"), "Opacité", "Transparence du calque."),
            (format!("ov{n}_size"), "Taille", "Hauteur du calque par rapport à l'image."),
            (format!("ov{n}_x"), "Position X", "Gauche (-1) à droite (+1)."),
            (format!("ov{n}_y"), "Position Y", "Bas (-1) à haut (+1)."),
            (format!("ov{n}_speed"), "Vitesse", "Vitesse de lecture de la boucle (négatif = à l'envers)."),
        ];
        egui::Grid::new(("ovknobs", slot)).num_columns(3).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
            for (name, label, help) in &rows {
                ui::knob_row(ui, p, name, label, help, sound);
                ui.end_row();
            }
        });
    });
}

fn tab_button(ui: &mut egui::Ui, tab: &mut Tab, this: Tab, label: &str, w: f32) {
    let sel = *tab == this;
    let text = RichText::new(label).size(15.0).color(if sel { egui::Color32::BLACK } else { ui::TEXT });
    let b = egui::Button::new(text).fill(if sel { ACCENT } else { egui::Color32::from_rgb(40, 44, 52) }).corner_radius(8);
    if ui.add_sized([w, 34.0], b).clicked() {
        *tab = this;
    }
}

fn image_tab(ui: &mut egui::Ui, p: &mut Params, st: &Status, ov_edit: &mut [Option<(f64, f64)>; 3], tx: &Sender<Cmd>, shared: &Arc<Shared>) {
    let sound = st.has_sound;
    card(ui, "Entrée", "Avant le circuit : ce qui entre colore toute la boucle.", |ui| {
        egui::Grid::new("input").num_columns(3).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
            for (name, label, help, _, _) in INPUT_KNOBS {
                ui::knob_row(ui, p, name, label, help, false);
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        lut_ui(ui, false, &st.lut_in, &mut p.lut_in_amount, tx, shared);
    });
    card(ui, "Calques vidéo", "Des boucles vidéo par-dessus l'image. « Dans le circuit » : les effets les abîment aussi.", |ui| {
        for slot in 0..3 {
            overlay_ui(ui, slot, p, &st.overlays[slot], &mut ov_edit[slot], tx, shared, sound);
        }
    });
    card(ui, "Amp", "Le boîtier : un enhancer dont la sortie revient à l'entrée, et le mélangeur de synchro.", |ui| {
        knob_grid(ui, "amp", p, &AMP_KNOBS, sound);
    });
    card(ui, "Magnétoscope VHS", "Entre le boîtier et la télé, sur le vrai signal.", |ui| {
        knob_grid(ui, "vhs", p, &VHS_KNOBS, sound);
    });
    card(ui, "Sortie", "Une LUT sur l'image finale (affichée et enregistrée).", |ui| {
        lut_ui(ui, true, &st.lut_out, &mut p.lut_out_amount, tx, shared);
    });
}

fn camera_tab(ui: &mut egui::Ui, p: &mut Params, st: &Status) {
    let sound = st.has_sound;
    card(ui, "Boucle caméra", "Une caméra filme la télé et renvoie son image dans la source. Tout le reste de cet onglet ne compte que si Mix > 0.", |ui| {
        knob_grid(ui, "cam", p, &CAM_KNOBS, sound);
        ui.horizontal(|ui| {
            ui.label("Position");
            egui::ComboBox::from_id_salt("campos").selected_text(CAMERAS[p.camera as usize].0).show_ui(ui, |ui| {
                for (i, (n, _)) in CAMERAS.iter().enumerate() {
                    ui.selectable_value(&mut p.camera, i as u8, *n);
                }
            });
        });
    });
    let subs = [
        "Bouger la caméra. Le zoom et la rotation font les tunnels et les spirales.",
        "À chaque tour de boucle : la teinte tourne (contours arc-en-ciel), le négatif inverse la lumière. Ça s'accumule, lance la lecture.",
        "Le tube de la caméra : rémanence, halo, image imprimée.",
        "L'objectif, et ce qu'il voit de l'écran.",
    ];
    // Everything below only acts through the camera loop.
    let on = p.mix > 0.0;
    if !on {
        egui::Frame::NONE.fill(ACCENT.gamma_multiply(0.15)).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Mix est à 0 : la caméra est coupée, les réglages ci-dessous n'ont aucun effet. Monte Mix pour les activer.").color(ACCENT));
        });
        ui.add_space(10.0);
    }
    for ((title, knobs), sub) in CAM_GROUPS.iter().zip(subs) {
        card(ui, title, sub, |ui| {
            ui.add_enabled_ui(on, |ui| knob_grid(ui, title, p, knobs, sound));
        });
    }
    card(ui, "Miroirs", "Des miroirs devant l'objectif : symétries et kaléidoscopes.", |ui| {
        ui.add_enabled_ui(on, |ui| {
            egui::ComboBox::from_id_salt("mirror").selected_text(MIRRORS[p.mirror as usize]).show_ui(ui, |ui| {
                for (i, n) in MIRRORS.iter().enumerate() {
                    ui.selectable_value(&mut p.mirror, i as u8, *n);
                }
            });
        });
    });
}

fn sound_tab(ui: &mut egui::Ui, p: &mut Params, st: &Status, sel: &mut usize, drag: &mut Option<usize>) {
    if !st.has_sound {
        card(ui, "Son", "", |ui| {
            ui.label(RichText::new("Cette vidéo n'a pas de son.").color(WEAK));
        });
        return;
    }
    card(ui, "Réaction au son", "Le son de la vidéo pousse les boutons. Clique sur 🎵 à côté d'un bouton pour le brancher.", |ui| {
        egui::Grid::new("meters").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            for i in [1usize, 5] {
                ui.label(SOURCES[i]);
                ui.add(egui::ProgressBar::new(st.feat[i - 1]).desired_width(250.0).desired_height(10.0).fill(ACCENT));
                ui.end_row();
            }
            ui.label("Intensité");
            ui.add(egui::Slider::new(&mut p.sound_amount, 0.0..=2.0).fixed_decimals(2)).on_hover_text("Force globale de la réaction au son.");
            ui.end_row();
            ui.label("Preset");
            egui::ComboBox::from_id_salt("sound_preset").selected_text("Choisir…").show_ui(ui, |ui| {
                for (i, (n, _)) in SOUND_PRESETS.iter().enumerate() {
                    if ui.selectable_label(false, *n).clicked() {
                        p.set_sound_preset(i);
                    }
                }
            })
            .response
            .on_hover_text("Remplit les liens son → boutons. Les valeurs des boutons ne changent pas.");
            ui.end_row();
        });
    });
    card(ui, "Bandes de fréquence", "Place les points comme sur un EQ : ← → fréquence, ↑ ↓ sensibilité, molette = largeur.", |ui| {
        spectrum_ui(ui, &st.spectrum, &mut p.bands, sel, drag);
        ui.add_space(4.0);
        egui::Grid::new("bands").num_columns(6).spacing([6.0, 6.0]).show(ui, |ui| {
            for i in 0..N_BANDS {
                ui.radio_value(sel, i, RichText::new(format!("Bande {}", i + 1)).color(BAND_COLORS[i]))
                    .on_hover_text(format!("Réglage de départ : {}", BAND_HINTS[i]));
                let b = &mut p.bands[i];
                let (mut c, mut w) = (b.centre(), b.width());
                let cs = c as f64 * 0.01;
                let r1 = ui.add(egui::DragValue::new(&mut c).range(20.0..=20000.0).speed(cs).suffix(" Hz").fixed_decimals(0)).on_hover_text("Fréquence centrale");
                let r2 = ui.add(egui::DragValue::new(&mut w).range(0.1..=6.0).speed(0.02).suffix(" oct").fixed_decimals(1)).on_hover_text("Largeur en octaves");
                if r1.changed() || r2.changed() {
                    b.set(c, w);
                }
                ui.add(egui::DragValue::new(&mut b.sens).range(-12.0..=12.0).speed(0.1).suffix(" dB").fixed_decimals(1)).on_hover_text("Sensibilité");
                egui::ComboBox::from_id_salt(("bandmode", i)).selected_text(["Niveau", "Attaques"][b.mode as usize]).width(78.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut b.mode, 0, "Niveau").on_hover_text("Suit l'énergie de la bande.");
                    ui.selectable_value(&mut b.mode, 1, "Attaques").on_hover_text("Réagit aux coups dans la bande.");
                });
                ui.add(egui::ProgressBar::new(st.feat[N_GLOBAL + i]).desired_width(30.0).desired_height(8.0).fill(BAND_COLORS[i]));
                ui.end_row();
            }
        });
    });
    card(ui, "Liens actifs", "Ce qui est branché sur quoi.", |ui| {
        let mut any = false;
        egui::Grid::new("links").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
            for (k, (name, _, _)) in KNOBS.iter().enumerate() {
                let m = p.mods[k];
                if m.source == 0 {
                    continue;
                }
                any = true;
                ui.label(ui::knob_label(name));
                ui.label(RichText::new(format!("← {}  {:+.2}", SOURCES[m.source as usize], m.depth)).color(ACCENT));
                if ui.small_button("Couper").clicked() {
                    p.mods[k].source = 0;
                }
                ui.end_row();
            }
        });
        if !any {
            ui.label(RichText::new("Aucun lien. Clique sur 🎵 à côté d'un bouton, ou choisis un preset.").color(WEAK));
        }
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((path, n)) = self.shot.as_mut() {
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
                let _ = std::fs::write(&*path, out);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let st = self.shared.status.lock().unwrap().clone();
        let loaded = st.file.is_some();

        // New screen from the player.
        {
            let s = self.shared.screen.lock().unwrap();
            if s.0 != self.seen && !s.1.is_empty() {
                self.seen = s.0;
                let img = egui::ColorImage::from_rgb([768, 480], &s.1);
                match &mut self.tex {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => self.tex = Some(ctx.load_texture("screen", img, egui::TextureOptions::LINEAR)),
                }
            }
        }

        if loaded && ctx.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.wants_keyboard_input() {
            self.send(if st.playing { Cmd::Pause } else { Cmd::Play });
        }

        let bar = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 10));
        egui::TopBottomPanel::top("transport").frame(bar).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Ouvrir…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Vidéo", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mpg", "mts"]).pick_file() {
                        self.send(Cmd::Open(p));
                    }
                }
                ui.add_space(6.0);
                ui.add_enabled_ui(loaded, |ui| {
                    let play = egui::Button::new(RichText::new(if st.playing { "⏸" } else { "▶" }).size(18.0).color(egui::Color32::BLACK)).fill(ACCENT).corner_radius(16);
                    if ui.add_sized([44.0, 32.0], play).on_hover_text("Lecture / pause (Espace)").clicked() {
                        self.send(if st.playing { Cmd::Pause } else { Cmd::Play });
                    }
                    let dur = st.file.as_ref().map_or(1.0, |f| f.duration.max(0.01));
                    let mut t = self.seek.unwrap_or(st.time);
                    ui.spacing_mut().slider_width = (ui.available_width() - 560.0).max(160.0);
                    let resp = ui.add(egui::Slider::new(&mut t, 0.0..=dur).show_value(false));
                    if resp.dragged() || resp.changed() {
                        self.seek = Some(t);
                    }
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        self.send(Cmd::Seek(t));
                        self.seek = None;
                    }
                    ui.label(RichText::new(format!("{} / {}", fmt_time(t), fmt_time(dur))).monospace());
                    ui.add_space(10.0);
                    let live = st.live_rec.is_some();
                    let live_txt = if live { RichText::new("⏹  Arrêter").color(REC) } else { RichText::new("⏺  En direct…") };
                    if ui.add_enabled(!st.auto_rec, egui::Button::new(live_txt)).on_hover_text("Enregistre ce qui est joué dans un fichier vidéo, avec le son d'origine.").clicked() {
                        if live {
                            self.send(Cmd::RecLive(None));
                        } else if let Some(p) = rfd::FileDialog::new().set_file_name("glitchbox-live.mp4").add_filter("MP4", &["mp4"]).save_file() {
                            self.send(Cmd::RecLive(Some(p)));
                        }
                    }
                    let auto_txt = if st.auto_rec { RichText::new(format!("⏹  Stop ({} img)", st.auto_frames)).color(ACCENT) } else { RichText::new("⏺  Réglages") };
                    if ui.add_enabled(!live, egui::Button::new(auto_txt)).on_hover_text("Joue la vidéo et retient chaque mouvement de bouton, pour un rendu propre ensuite.").clicked() {
                        self.send(Cmd::RecAuto(!st.auto_rec));
                    }
                    let can_render = st.automation.is_some() && self.job.is_none();
                    if ui.add_enabled(can_render, egui::Button::new("Rendre…")).on_hover_text("Rendu hors ligne du passage enregistré, identique à ce que vous avez joué.").clicked() {
                        if let Some(a) = st.automation.clone() {
                            self.start_render(a);
                        }
                    }
                });
            });
        });

        let status = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 6));
        egui::TopBottomPanel::bottom("status").frame(status).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if st.live_rec.is_some() {
                    ui.label(RichText::new("● REC").color(REC).strong());
                }
                if st.auto_rec {
                    ui.label(RichText::new("● RÉGLAGES").color(ACCENT).strong());
                }
                if loaded {
                    ui.label(RichText::new(format!("{:4.1} i/s", st.fps)).monospace().color(WEAK));
                    ui.label(RichText::new(format!("verrou H {:3.0} %", st.h_lock * 100.0)).monospace().color(WEAK));
                    ui.separator();
                }
                ui.label(RichText::new(&st.message).color(WEAK));
            });
        });

        let tx = self.tx.clone();
        let shared = self.shared.clone();
        let (tab, sel, drag, ov_edit) = (&mut self.tab, &mut self.sel_band, &mut self.band_drag, &mut self.ov_edit);
        let side = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 12));
        egui::SidePanel::left("knobs").resizable(false).exact_width(480.0).frame(side).show(ctx, |ui| {
            let old = *shared.params.lock().unwrap();
            let mut p = old;
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
                        if let Some(f) = rfd::FileDialog::new().set_file_name("reglage.json").add_filter("JSON", &["json"]).save_file() {
                            let _ = std::fs::write(f, serde_json::to_string_pretty(&p).unwrap_or_default());
                        }
                    }
                    if ui.button("Charger…").clicked() {
                        if let Some(f) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                            if let Some(q) = std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str::<Params>(&s).ok()) {
                                p = q.clamped();
                            }
                        }
                    }
                });
                ui.label(RichText::new("Double-clic sur un curseur : le remettre à zéro. En pause, un réglage relance 8 images de chauffe.").small().color(WEAK));
            });
            egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match *tab {
                    Tab::Image => image_tab(ui, &mut p, &st, ov_edit, &tx, &shared),
                    Tab::Camera => camera_tab(ui, &mut p, &st),
                    Tab::Sound => sound_tab(ui, &mut p, &st, sel, drag),
                });
            });
            if p != old {
                *shared.params.lock().unwrap() = p;
            }
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(ui::BG)).show(ctx, |ui| {
            if let Some(tex) = &self.tex {
                let aspect = st.file.as_ref().map_or(4.0 / 3.0, |f| f.width as f32 / f.height.max(1) as f32);
                let avail = ui.available_size() - egui::vec2(32.0, 32.0);
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

        // Render progress.
        let mut close = false;
        if let Some(job) = &self.job {
            let (d, n) = *job.progress.lock().unwrap();
            let res = job.result.lock().unwrap().clone();
            egui::Window::new("Rendu").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| match res {
                None => {
                    ui.add(egui::ProgressBar::new(d as f32 / n.max(1) as f32).text(format!("{d} / {n} images")).fill(ACCENT).desired_width(320.0));
                    if ui.button("Annuler").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
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
        }
        if close {
            self.job = None;
        }
    }
}

pub const BAND_COLORS: [egui::Color32; N_BANDS] = [
    egui::Color32::from_rgb(255, 95, 70),
    egui::Color32::from_rgb(255, 200, 50),
    egui::Color32::from_rgb(90, 200, 255),
    egui::Color32::from_rgb(200, 130, 255),
];

/// Live spectrum with the four bands as EQ-style points: drag a point (left/right = frequency,
/// up/down = sensitivity), mouse wheel on it = width, click on the background = move the selected
/// point there.
pub fn spectrum_ui(ui: &mut egui::Ui, spec: &[f32], bands: &mut [Band; N_BANDS], sel: &mut usize, drag: &mut Option<usize>) {
    use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 130.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, crate::ui::BG);
    let n = SPEC_BINS as f32;
    let plot = Rect::from_min_max(rect.min + vec2(0.0, 6.0), rect.max - vec2(0.0, 14.0));
    let fx = |f: f32| plot.left() + plot.width() * (freq_bin(f) + 0.5) / n;
    let xf = |x: f32| bin_freq((x - plot.left()) / plot.width() * n - 0.5).clamp(20.0, 20000.0);
    let ys = |sens: f32| plot.center().y - sens / 12.0 * (plot.height() / 2.0 - 8.0);
    let sy = |y: f32| ((plot.center().y - y) / (plot.height() / 2.0 - 8.0) * 12.0).clamp(-12.0, 12.0);

    // Grid and frequency labels.
    painter.line_segment([pos2(plot.left(), plot.center().y), pos2(plot.right(), plot.center().y)], Stroke::new(1.0, Color32::from_gray(40)));
    for (f, l) in [(50.0, "50"), (100.0, "100"), (200.0, "200"), (500.0, "500"), (1000.0, "1k"), (2000.0, "2k"), (5000.0, "5k"), (10000.0, "10k")] {
        let x = fx(f);
        painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, Color32::from_gray(32)));
        painter.text(pos2(x, rect.bottom() - 1.0), Align2::CENTER_BOTTOM, l, FontId::proportional(9.0), Color32::GRAY);
    }
    // Spectrum.
    let bw = plot.width() / n;
    for (b, &pw) in spec.iter().enumerate() {
        let db = 10.0 * (pw + 1e-12).log10();
        let h = ((db + 70.0) / 70.0).clamp(0.0, 1.0) * plot.height();
        let x = plot.left() + b as f32 * bw;
        painter.rect_filled(Rect::from_min_max(pos2(x, plot.bottom() - h), pos2(x + bw - 1.0, plot.bottom())), 0.0, Color32::from_gray(70));
    }
    // Bands: a bell from the floor to the point.
    for (i, b) in bands.iter().enumerate() {
        let c = BAND_COLORS[i];
        let (cf, w) = (b.centre(), b.width().max(0.1));
        let top = ys(b.sens);
        let pts: Vec<egui::Pos2> = (0..=160)
            .map(|k| {
                let x = plot.left() + plot.width() * k as f32 / 160.0;
                let d = (xf(x) / cf).log2() / (w / 2.0);
                let bell = (-0.5 * (d * 1.4).powi(2)).exp();
                pos2(x, plot.bottom() - bell * (plot.bottom() - top))
            })
            .collect();
        let mut fill = pts.clone();
        fill.push(pos2(plot.right(), plot.bottom()));
        fill.push(pos2(plot.left(), plot.bottom()));
        let alpha = if i == *sel { 0.28 } else { 0.10 };
        painter.add(Shape::convex_polygon(fill, c.gamma_multiply(alpha), Stroke::NONE));
        painter.add(Shape::line(pts, Stroke::new(if i == *sel { 2.0 } else { 1.2 }, c)));
    }
    // Points.
    let point = |b: &Band| pos2(fx(b.centre()), ys(b.sens));
    let hover = resp.hover_pos().and_then(|p| (0..N_BANDS).find(|&i| point(&bands[i]).distance(p) < 12.0));
    for (i, b) in bands.iter().enumerate() {
        let p = point(b);
        let c = BAND_COLORS[i];
        let r = if Some(i) == hover || i == *sel { 9.0 } else { 7.5 };
        painter.circle(p, r, if i == *sel { c } else { Color32::from_gray(20) }, Stroke::new(1.5, if i == *sel { Color32::WHITE } else { c }));
        painter.text(p, Align2::CENTER_CENTER, format!("{}", i + 1), FontId::proportional(10.0), if i == *sel { Color32::BLACK } else { c });
    }

    // Interaction.
    if resp.drag_started() {
        *drag = Some(hover.unwrap_or(*sel));
        *sel = drag.unwrap();
    }
    if let (Some(i), Some(p)) = (*drag, resp.interact_pointer_pos()) {
        if resp.dragged() {
            let w = bands[i].width();
            bands[i].set(xf(p.x), w);
            bands[i].sens = sy(p.y);
        }
    }
    if resp.drag_stopped() {
        *drag = None;
    }
    if resp.clicked() {
        match hover {
            Some(i) => *sel = i,
            None => {
                if let Some(p) = resp.interact_pointer_pos() {
                    let w = bands[*sel].width();
                    bands[*sel].set(xf(p.x), w);
                    bands[*sel].sens = sy(p.y);
                }
            }
        }
    }
    if resp.hovered() {
        let dy = ui.input(|i| i.raw_scroll_delta.y);
        if dy != 0.0 {
            let i = hover.unwrap_or(*sel);
            let (c, w) = (bands[i].centre(), bands[i].width());
            bands[i].set(c, w * 1.12f32.powf(-dy.signum()));
        }
    }
    resp.on_hover_text("Glisser un point : ← → fréquence, ↑ ↓ sensibilité.\nMolette sur un point : largeur de la bande.\nClic sur le fond : y place le point sélectionné.");
}
