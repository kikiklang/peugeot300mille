//! The desktop window.

use crate::audio::{bin_freq, MENU_SOURCES, freq_bin, N_BANDS, N_GLOBAL, SOURCES, SPEC_BINS};
use crate::engine::lut::Lut;
use crate::params::{Band, BAND_HINTS, Params, AMP_KNOBS, CAMERAS, CAM_KNOBS, INPUT_KNOBS, KNOBS, SOUND_PRESETS, VHS_KNOBS};
use crate::player::{self, Cmd, Shared};
use crate::render::{render, Knobs};
use crate::runner::Automation;
use anyhow::{anyhow, Result};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

pub fn run(input: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1320.0, 820.0]).with_title("glitchbox"),
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

struct App {
    sel_band: usize,
    band_drag: Option<usize>,
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    tex: Option<egui::TextureHandle>,
    seen: u64,
    seek: Option<f64>,
    job: Option<RenderJob>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, input: Option<PathBuf>) -> Self {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        player::spawn(shared.clone(), rx, move || ctx.request_repaint());
        if let Some(p) = input {
            let _ = tx.send(Cmd::Open(p));
        }
        Self { sel_band: 0, band_drag: None, shared, tx, tex: None, seen: 0, seek: None, job: None }
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
            let r = render(&input, &out, Knobs::Recorded(&auto), auto.start, None, luts, &cancel, &mut |d, n| {
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

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

        if ctx.input(|i| i.key_pressed(egui::Key::Space)) && loaded {
            self.send(if st.playing { Cmd::Pause } else { Cmd::Play });
        }

        egui::TopBottomPanel::top("transport").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("📂 Ouvrir…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Vidéo", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mpg", "mts"])
                        .pick_file()
                    {
                        self.send(Cmd::Open(p));
                    }
                }
                ui.add_enabled_ui(loaded, |ui| {
                    if ui.button(if st.playing { "⏸ Pause" } else { "▶ Lecture" }).clicked() {
                        self.send(if st.playing { Cmd::Pause } else { Cmd::Play });
                    }
                    let dur = st.file.as_ref().map_or(1.0, |f| f.duration.max(0.01));
                    let mut t = self.seek.unwrap_or(st.time);
                    let resp = ui.add(
                        egui::Slider::new(&mut t, 0.0..=dur)
                            .show_value(false)
                            .trailing_fill(true),
                    );
                    if resp.dragged() || resp.changed() {
                        self.seek = Some(t);
                    }
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        self.send(Cmd::Seek(t));
                        self.seek = None;
                    }
                    ui.monospace(format!("{} / {}", fmt_time(t), fmt_time(dur)));
                    ui.separator();

                    let live = st.live_rec.is_some();
                    let live_txt = if live { "⏹ Arrêter l'enregistrement" } else { "⏺ Enregistrer en direct…" };
                    if ui
                        .add_enabled(!st.auto_rec, egui::Button::new(live_txt))
                        .on_hover_text("Écrit ce qui est joué dans un fichier vidéo, avec le son d'origine.")
                        .clicked()
                    {
                        if live {
                            self.send(Cmd::RecLive(None));
                        } else if let Some(p) = rfd::FileDialog::new()
                            .set_file_name("glitchbox-live.mp4")
                            .add_filter("MP4", &["mp4"])
                            .save_file()
                        {
                            self.send(Cmd::RecLive(Some(p)));
                        }
                    }
                    let auto_txt = if st.auto_rec {
                        format!("⏹ Stop ({} images)", st.auto_frames)
                    } else {
                        "⏺ Enregistrer les réglages".to_string()
                    };
                    if ui
                        .add_enabled(!live, egui::Button::new(auto_txt))
                        .on_hover_text("Joue la vidéo et retient chaque mouvement de bouton, pour un rendu propre ensuite.")
                        .clicked()
                    {
                        self.send(Cmd::RecAuto(!st.auto_rec));
                    }
                    let can_render = st.automation.is_some() && self.job.is_none();
                    if ui
                        .add_enabled(can_render, egui::Button::new("🎬 Rendre l'enregistrement…"))
                        .on_hover_text("Rendu hors ligne, image par image, identique à ce que vous avez joué.")
                        .clicked()
                    {
                        if let Some(a) = st.automation.clone() {
                            self.start_render(a);
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if st.live_rec.is_some() {
                    ui.colored_label(egui::Color32::RED, "● REC");
                }
                if st.auto_rec {
                    ui.colored_label(egui::Color32::from_rgb(255, 160, 0), "● RÉGLAGES");
                }
                if loaded {
                    ui.monospace(format!("{:5.1} i/s", st.fps));
                    ui.monospace(format!("verrou H {:3.0} %", st.h_lock * 100.0));
                    ui.separator();
                }
                ui.label(&st.message);
            });
        });

        let tx = self.tx.clone();
        let shared = self.shared.clone();
        let (sel, drag) = (&mut self.sel_band, &mut self.band_drag);
        egui::SidePanel::left("knobs").resizable(false).min_width(400.0).show(ctx, |ui| {
            let old = *shared.params.lock().unwrap();
            let mut p = old;
            let sound = st.has_sound;
            let lut_row = |ui: &mut egui::Ui, output: bool, name: &Option<String>, amount: &mut f32| {
                ui.horizontal(|ui| {
                    let label = if output { "LUT de sortie…" } else { "LUT d'entrée…" };
                    if ui.button(label).on_hover_text("Fichier .cube (color.io, Resolve, Kdenlive…)").clicked() {
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
                            ui.label(n);
                            if ui.small_button("Retirer").on_hover_text("Retirer la LUT").clicked() {
                                let _ = tx.send(Cmd::SetLut { output, lut: None });
                            }
                        }
                        None => {
                            ui.weak("aucune");
                        }
                    }
                });
                if name.is_some() {
                    ui.add(egui::Slider::new(amount, 0.0..=1.0).text("Dosage"));
                }
            };
            egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(6.0);

            // Sound reaction.
            ui.heading("🎵 Réaction au son");
            if sound {
                ui.horizontal(|ui| {
                    for i in [1usize, 5] {
                        ui.vertical(|ui| {
                            ui.add(egui::ProgressBar::new(st.feat[i - 1]).desired_width(150.0).desired_height(8.0));
                            ui.small(SOURCES[i]);
                        });
                    }
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("sound_preset")
                        .selected_text("Preset…")
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for (i, (n, _)) in SOUND_PRESETS.iter().enumerate() {
                                if ui.selectable_label(false, *n).clicked() {
                                    p.set_sound_preset(i);
                                }
                            }
                        })
                        .response
                        .on_hover_text("Remplit les liens son → boutons. Les valeurs des boutons ne changent pas.");
                    ui.add(egui::Slider::new(&mut p.sound_amount, 0.0..=2.0).text("Intensité"))
                        .on_hover_text("Force globale de la réaction au son.");
                });
                ui.add_space(4.0);
                ui.label("Bandes de fréquence");
                spectrum_ui(ui, &st.spectrum, &mut p.bands, sel, drag);
                for i in 0..N_BANDS {
                    ui.horizontal(|ui| {
                        ui.radio_value(sel, i, egui::RichText::new(format!("Bande {}", i + 1)).color(BAND_COLORS[i]))
                            .on_hover_text(format!("Réglage de départ : {}", BAND_HINTS[i]));
                        let b = &mut p.bands[i];
                        let (mut c, mut w) = (b.centre(), b.width());
                        let cs = c as f64 * 0.01;
                        let r1 = ui.add(egui::DragValue::new(&mut c).range(20.0..=20000.0).speed(cs).suffix(" Hz").fixed_decimals(0))
                            .on_hover_text("Fréquence centrale");
                        let r2 = ui.add(egui::DragValue::new(&mut w).range(0.1..=6.0).speed(0.02).suffix(" oct").fixed_decimals(1))
                            .on_hover_text("Largeur en octaves");
                        if r1.changed() || r2.changed() {
                            b.set(c, w);
                        }
                        ui.add(egui::DragValue::new(&mut b.sens).range(-12.0..=12.0).speed(0.1).suffix(" dB").fixed_decimals(1))
                            .on_hover_text("Sensibilité : plus haut = la bande pousse plus fort les boutons");
                        egui::ComboBox::from_id_salt(("bandmode", i))
                            .selected_text(["Niveau", "Attaques"][b.mode as usize])
                            .width(70.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut b.mode, 0, "Niveau").on_hover_text("Suit l'énergie de la bande.");
                                ui.selectable_value(&mut b.mode, 1, "Attaques").on_hover_text("Réagit aux coups dans la bande.");
                            });
                        ui.add(egui::ProgressBar::new(st.feat[N_GLOBAL + i]).desired_width(46.0).desired_height(8.0));
                    });
                }
                ui.small("Pour chaque bouton : la source qui le pousse (dont les bandes), et de combien (négatif = le son le fait baisser).");
            } else {
                ui.small("Cette vidéo n'a pas de son.");
            }
            ui.add_space(10.0);

            let knob_row = |ui: &mut egui::Ui, p: &mut Params, k: usize, label: &str, help: &str| {
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(p.knob_mut(KNOBS[k]), 0.0..=1.0).text(label)).on_hover_text(help);
                    if sound {
                        let m = &mut p.mods[k];
                        egui::ComboBox::from_id_salt(("src", k))
                            .selected_text(SOURCES[m.source as usize])
                            .width(76.0)
                            .show_ui(ui, |ui| {
                                for i in MENU_SOURCES {
                                    ui.selectable_value(&mut m.source, i, SOURCES[i as usize]);
                                }
                            });
                        ui.add_enabled(
                            m.source > 0,
                            egui::DragValue::new(&mut m.depth).range(-1.0..=1.0).speed(0.01).fixed_decimals(2),
                        )
                        .on_hover_text("Profondeur : de combien le son pousse ce bouton.");
                    }
                });
            };
            egui::CollapsingHeader::new(egui::RichText::new("🎨 Entrée (avant le circuit)").heading())
                .default_open(true)
                .show(ui, |ui| {
                    for (name, label, help, lo, hi) in INPUT_KNOBS {
                        ui.add(egui::Slider::new(p.knob_mut(name), lo..=hi).text(label)).on_hover_text(help);
                    }
                    lut_row(ui, false, &st.lut_in, &mut p.lut_in_amount);
                });
            ui.add_space(6.0);
            let section = |ui: &mut egui::Ui, p: &mut Params, title: &str, knobs: &[(&str, &str, &str)]| {
                egui::CollapsingHeader::new(egui::RichText::new(title).heading()).default_open(true).show(ui, |ui| {
                    for (name, label, help) in knobs {
                        let k = KNOBS.iter().position(|n| n == name).unwrap();
                        knob_row(ui, p, k, label, help);
                    }
                });
            };
            section(ui, &mut p, "Amp", &AMP_KNOBS);
            ui.add_space(6.0);
            section(ui, &mut p, "📼 Magnétoscope VHS", &VHS_KNOBS);
            ui.add_space(6.0);
            section(ui, &mut p, "Video Feedback", &CAM_KNOBS);
            egui::ComboBox::from_label("Camera")
                .selected_text(CAMERAS[p.camera as usize].0)
                .show_ui(ui, |ui| {
                    for (i, (n, _)) in CAMERAS.iter().enumerate() {
                        ui.selectable_value(&mut p.camera, i as u8, *n);
                    }
                })
                .response
                .on_hover_text("Où se tient la caméra. Ne compte que si Mix > 0.");
            ui.add_space(6.0);
            egui::CollapsingHeader::new(egui::RichText::new("🎞 Sortie").heading()).default_open(true).show(ui, |ui| {
                lut_row(ui, true, &st.lut_out, &mut p.lut_out_amount);
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Tout à 0").clicked() {
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
            ui.add_space(10.0);
            ui.small("Espace : lecture / pause.\nUn circuit avec mémoire : chaque image dépend des précédentes. En pause, un réglage relance 8 images de chauffe.");
            });
            if p != old {
                *shared.params.lock().unwrap() = p;
            }
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ctx, |ui| {
            if let Some(tex) = &self.tex {
                let aspect = st.file.as_ref().map_or(4.0 / 3.0, |f| f.width as f32 / f.height.max(1) as f32);
                let avail = ui.available_size();
                let mut size = egui::vec2(avail.x, avail.x / aspect);
                if size.y > avail.y {
                    size = egui::vec2(avail.y * aspect, avail.y);
                }
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), size)));
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Ouvrez une vidéo (📂 Ouvrir…)").color(egui::Color32::GRAY).size(20.0));
                });
            }
        });

        // Render progress.
        let mut close = false;
        if let Some(job) = &self.job {
            let (d, n) = *job.progress.lock().unwrap();
            let res = job.result.lock().unwrap().clone();
            egui::Window::new("Rendu").collapsible(false).resizable(false).show(ctx, |ui| match res {
                None => {
                    ui.add(egui::ProgressBar::new(d as f32 / n.max(1) as f32).text(format!("{d} / {n} images")));
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
                    ui.colored_label(egui::Color32::RED, e);
                    close = ui.button("OK").clicked();
                }
            });
        }
        if close {
            self.job = None;
        }
    }
}

const BAND_COLORS: [egui::Color32; N_BANDS] = [
    egui::Color32::from_rgb(255, 95, 70),
    egui::Color32::from_rgb(255, 200, 50),
    egui::Color32::from_rgb(90, 200, 255),
    egui::Color32::from_rgb(200, 130, 255),
];

/// Live spectrum with the four bands as EQ-style points: drag a point (left/right = frequency,
/// up/down = sensitivity), mouse wheel on it = width, click on the background = move the selected
/// point there.
fn spectrum_ui(ui: &mut egui::Ui, spec: &[f32], bands: &mut [Band; N_BANDS], sel: &mut usize, drag: &mut Option<usize>) {
    use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 130.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(16));
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
