//! The "Son" tab: how the soundtrack drives the knobs, and the EQ-style band editor.

use super::transport::fmt_time;
use super::view::View;
use crate::audio::{bin_freq, freq_bin, N_BANDS, N_GLOBAL, SOURCES, SPEC_BINS};
use crate::params::{Band, Params, BAND_HINTS, KNOBS, SOUND_PRESETS};
use crate::ui::{self, card, ACCENT, WEAK};
use eframe::egui::{self, RichText};

/// Which point of the spectrum is selected, and which one is being dragged.
#[derive(Default)]
pub struct BandPoints {
    selected: usize,
    dragged: Option<usize>,
}

const POINT_LABELS: [&str; N_BANDS] = ["1", "2", "3", "4"];

/// The "Son" tab. `bands` is the selection and drag of the spectrum's points.
pub fn sound_tab(ui: &mut egui::Ui, p: &mut Params, st: &View, bands: &mut BandPoints) {
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
    card(ui, "Tempo", "Branche un bouton sur « Tempo » ou « Vague » avec 🎵 pour le caler sur les temps du morceau.", |ui| {
        let Some(bpm) = st.bpm else {
            ui.label(RichText::new("Aucun tempo détecté dans ce son.").color(WEAK));
            return;
        };
        egui::Grid::new("tempo").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Tempo");
            ui.horizontal(|ui| {
                let shown = bpm * 2f32.powi(p.tempo_octave as i32);
                ui.label(RichText::new(format!("{shown:.1} BPM")).strong().color(ACCENT));
                if ui.add_enabled(p.tempo_octave > -2, egui::Button::new("÷2")).on_hover_text("Le détecteur a compté deux fois trop de temps.").clicked() {
                    p.tempo_octave -= 1;
                }
                if ui.add_enabled(p.tempo_octave < 2, egui::Button::new("×2")).on_hover_text("Le détecteur a compté deux fois trop peu de temps.").clicked() {
                    p.tempo_octave += 1;
                }
            });
            ui.end_row();
            ui.label("Calage");
            ui.horizontal(|ui| {
                // No tooltip: it could open over the button and take the click aimed at the beat.
                let align = ui.button("Align start");
                #[cfg(test)]
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("align"), align.rect));
                if ui::pressed_now(ui, &align) {
                    p.tempo_restart = st.time as f32;
                    p.tempo_phase = 0.0;
                }
                if p.tempo_restart >= 0.0 {
                    ui.label(RichText::new(format!("calé à {}", fmt_time(p.tempo_restart as f64))).small().color(WEAK));
                }
            });
            ui.end_row();
            ui.label("Décalage");
            ui.add(egui::Slider::new(&mut p.tempo_phase, -2.0..=2.0).step_by(0.01).fixed_decimals(2).suffix(" temps"))
                .on_hover_text("Avance ou recule la grille. ±1 change le temps sur lequel tombent les pulsations de 2 et 4 temps.");
            ui.end_row();
            for (label, i) in [("Chaque temps", 11usize), ("Chaque mesure", 13)] {
                ui.label(label);
                ui.add(egui::ProgressBar::new(st.feat[i - 1]).desired_width(250.0).desired_height(10.0).fill(ACCENT));
                ui.end_row();
            }
        });
    });
    card(ui, "Bandes de fréquence", "Place les points comme sur un EQ : ← → fréquence, ↑ ↓ sensibilité, molette = largeur.", |ui| {
        spectrum_ui(ui, &st.spectrum, &mut p.bands, bands);
        ui.add_space(4.0);
        egui::Grid::new("bands").num_columns(6).spacing([6.0, 6.0]).show(ui, |ui| {
            for i in 0..N_BANDS {
                ui.radio_value(&mut bands.selected, i, RichText::new(format!("Bande {}", i + 1)).color(BAND_COLORS[i]))
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

const BAND_COLORS: [egui::Color32; N_BANDS] = [
    egui::Color32::from_rgb(255, 95, 70),
    egui::Color32::from_rgb(255, 200, 50),
    egui::Color32::from_rgb(90, 200, 255),
    egui::Color32::from_rgb(200, 130, 255),
];

/// Live spectrum with the four bands as EQ-style points: drag a point (left/right = frequency,
/// up/down = sensitivity), mouse wheel on it = width, click on the background = move the selected
/// point there.
fn spectrum_ui(ui: &mut egui::Ui, spec: &[f32], bands: &mut [Band; N_BANDS], points: &mut BandPoints) {
    use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 130.0), Sense::click_and_drag());
    // The mappings below divide by the plot's size.
    if rect.width() < 2.0 {
        return;
    }
    let BandPoints { selected: sel, dragged: drag } = points;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, ui::BG);
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
        painter.text(p, Align2::CENTER_CENTER, POINT_LABELS[i], FontId::proportional(10.0), if i == *sel { Color32::BLACK } else { c });
    }

    // Interaction.
    if resp.drag_started() {
        let grabbed = hover.unwrap_or(*sel);
        *drag = Some(grabbed);
        *sel = grabbed;
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

#[cfg(test)]
mod align_start {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, Rect};

    #[test]
    fn a_click_aligns_on_the_press() {
        let ctx = egui::Context::default();
        let st = View { has_sound: true, bpm: Some(120.0), time: 42.5, ..View::default() };
        let (mut p, mut bands) = (Params { tempo_phase: 0.5, ..Params::default() }, BandPoints::default());
        let mut frame = |events: Vec<Event>, p: &mut Params| {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(520.0, 1200.0))), events, ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| sound_tab(ui, p, &st, &mut bands));
            });
            ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("align"))).expect("the button")
        };
        frame(vec![], &mut p);
        let c = frame(vec![], &mut p).center();
        frame(vec![Event::PointerMoved(c)], &mut p);
        assert_eq!(p.tempo_restart, -1.0);
        frame(vec![Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }], &mut p);
        assert_eq!((p.tempo_restart, p.tempo_phase), (42.5, 0.0));
    }
}
