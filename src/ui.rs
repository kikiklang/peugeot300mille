//! Look and widgets of the control panel: theme, fonts, cards, knob rows, sound links.

use crate::audio::{FIRST_TEMPO_SOURCE, MENU_SOURCES, SOURCES};
use crate::params::{knob_index, Params, INPUT_KNOBS, KNOBS};
use eframe::egui::{self, Color32, FontFamily, FontId, RichText, TextStyle};
use std::sync::Arc;

mod range_bar;

pub use range_bar::range_bar;

/// True on the very frame a button goes down, for actions aimed at a beat. Not `hovered()`: during
/// playback it can be false on that frame.
pub fn pressed_now(ui: &egui::Ui, button: &egui::Response) -> bool {
    let at = ui.input(|i| i.pointer.primary_pressed().then(|| i.pointer.interact_pos()).flatten());
    at.is_some_and(|at| button.interact_rect.contains(at) && ui.ctx().layer_id_at(at).is_none_or(|l| l == ui.layer_id()))
}

pub const ACCENT: Color32 = Color32::from_rgb(255, 138, 61);
pub const BG: Color32 = Color32::from_rgb(14, 15, 18);
pub const PANEL: Color32 = Color32::from_rgb(22, 24, 29);
pub const CARD: Color32 = Color32::from_rgb(31, 34, 41);
pub const TEXT: Color32 = Color32::from_rgb(226, 228, 233);
pub const WEAK: Color32 = Color32::from_rgb(138, 144, 156);
pub const REC: Color32 = Color32::from_rgb(235, 64, 64);

/// Fonts and colours, once at start-up.
pub fn setup(ctx: &egui::Context) {
    fonts(ctx);
    ctx.style_mut(|s| {
        let semi = FontFamily::Name("semibold".into());
        s.text_styles = [
            (TextStyle::Heading, FontId::new(15.5, semi)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        let sp = &mut s.spacing;
        sp.item_spacing = egui::vec2(8.0, 8.0);
        sp.button_padding = egui::vec2(10.0, 5.0);
        sp.interact_size.y = 26.0;
        sp.slider_width = 170.0;
        sp.slider_rail_height = 5.0;
        sp.combo_width = 150.0;
        sp.menu_margin = egui::Margin::same(10);

        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = PANEL;
        v.window_fill = CARD;
        v.extreme_bg_color = BG;
        v.faint_bg_color = CARD;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.85);
        v.selection.stroke = egui::Stroke::new(1.0, Color32::BLACK);
        v.slider_trailing_fill = true;
        v.hyperlink_color = ACCENT;
        let r = egui::CornerRadius::same(6);
        for (w, fill) in [
            (&mut v.widgets.noninteractive, CARD),
            (&mut v.widgets.inactive, Color32::from_rgb(45, 49, 58)),
            (&mut v.widgets.hovered, Color32::from_rgb(58, 63, 74)),
            (&mut v.widgets.active, Color32::from_rgb(70, 76, 88)),
            (&mut v.widgets.open, Color32::from_rgb(58, 63, 74)),
        ] {
            w.corner_radius = r;
            w.weak_bg_fill = fill;
            w.bg_fill = fill;
        }
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(40, 43, 51));
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.menu_corner_radius = egui::CornerRadius::same(8);
    });
}

/// IBM Plex if installed (Sans for text, SemiBold for titles, Mono for figures); egui's own
/// fonts otherwise.
fn fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let find = |patterns: &[&str], token: &str| -> Option<Vec<u8>> {
        for pat in patterns {
            let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pat]).output().ok()?;
            let path = String::from_utf8_lossy(&out.stdout).to_string();
            if path.contains(token) {
                if let Ok(b) = std::fs::read(&path) {
                    return Some(b);
                }
            }
        }
        None
    };
    let mut add = |name: &str, bytes: Option<Vec<u8>>, family: FontFamily| {
        if let Some(b) = bytes {
            defs.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
            defs.families.entry(family).or_default().insert(0, name.into());
        }
    };
    add("plex", find(&["IBM Plex Sans:style=Regular"], "PlexSans-Regular"), FontFamily::Proportional);
    add("plex-mono", find(&["IBM Plex Mono:style=Regular"], "PlexMono-Regular"), FontFamily::Monospace);
    let proportional = defs.families[&FontFamily::Proportional].clone();
    let semi = find(&["IBM Plex Sans SemiBold", "IBM Plex Sans:weight=600"], "PlexSans-SemiBold")
        .or_else(|| find(&["IBM Plex Sans Medium", "IBM Plex Sans:weight=500"], "PlexSans-Medium"));
    let mut fam = proportional;
    if let Some(b) = semi {
        defs.font_data.insert("plex-semi".into(), Arc::new(egui::FontData::from_owned(b)));
        fam.insert(0, "plex-semi".into());
    }
    defs.families.insert(FontFamily::Name("semibold".into()), fam);
    ctx.set_fonts(defs);
}

/// A rounded block with a title and a one-line explanation.
pub fn card(ui: &mut egui::Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(CARD)
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).heading());
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).small().color(WEAK));
            }
            ui.add_space(4.0);
            add(ui);
        });
    ui.add_space(10.0);
}

fn range_of(name: &str) -> (f32, f32) {
    if let Some(k) = KNOBS.iter().find(|k| k.0 == name) {
        return (k.1, k.2);
    }
    if name.starts_with("ov") {
        if name.ends_with("_x") || name.ends_with("_y") {
            return (-1.0, 1.0);
        }
        if name.ends_with("_speed") {
            return (-4.0, 4.0);
        }
    }
    if let Some(k) = INPUT_KNOBS.iter().find(|k| k.0 == name) {
        return (k.3, k.4);
    }
    (0.0, 1.0)
}

fn default_of(name: &str) -> f32 {
    *Params::default().knob_mut(name)
}

/// Aligned rows: label · slider · sound link. Double-click a slider to reset it.
pub fn knob_grid(ui: &mut egui::Ui, id: impl std::hash::Hash, p: &mut Params, rows: &[(&str, &str, &str)], sound: bool) {
    egui::Grid::new(id).num_columns(3).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
        for (name, label, help) in rows {
            knob_row(ui, p, name, label, help, sound);
            ui.end_row();
        }
    });
}

pub fn knob_row(ui: &mut egui::Ui, p: &mut Params, name: &str, label: &str, help: &str, sound: bool) {
    ui.label(label).on_hover_text(help);
    let (lo, hi) = range_of(name);
    let decimals = if hi - lo > 10.0 { 0 } else { 2 };
    // Two-way knobs (centre = 0) are not filled from the left.
    let r = ui.add(egui::Slider::new(p.knob_mut(name), lo..=hi).fixed_decimals(decimals).trailing_fill(lo >= 0.0)).on_hover_ui(|ui| {
        // Built only while hovered: there are dozens of knobs on screen. Same layout as on_hover_text.
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(format!("{help}\nDouble-clic : remettre à {}", default_of(name)));
    });
    if r.double_clicked() {
        *p.knob_mut(name) = default_of(name);
    }
    if KNOBS.iter().any(|k| k.0 == name) && sound {
        sound_link(ui, p, knob_index(name));
    } else {
        ui.label("");
    }
}

fn short(source: u8) -> String {
    match source {
        1 => "Vol".into(),
        5 => "Att".into(),
        6..=9 => format!("B{}", source - 5),
        10 => "T½".into(),
        11 => "T1".into(),
        12 => "T2".into(),
        13 => "T4".into(),
        14 => "~1".into(),
        15 => "~4".into(),
        s => SOURCES.get(s as usize).map_or_else(String::new, |n| n.chars().take(3).collect()),
    }
}

/// A small button that shows what drives the knob, and opens a menu to change it.
fn sound_link(ui: &mut egui::Ui, p: &mut Params, k: usize) {
    let m = &mut p.mods[k];
    let text = if m.source == 0 {
        RichText::new("🎵").color(WEAK)
    } else {
        RichText::new(format!("🎵 {} {:+.2}", short(m.source), m.depth)).color(ACCENT)
    };
    ui.menu_button(text, |ui| {
        ui.set_min_width(200.0);
        ui.label(RichText::new("Piloté par").small().color(WEAK));
        for i in MENU_SOURCES {
            if i == FIRST_TEMPO_SOURCE {
                ui.separator();
            }
            ui.selectable_value(&mut m.source, i, if i == 0 { "Rien" } else { SOURCES[i as usize] });
        }
        ui.separator();
        ui.add_enabled(m.source > 0, egui::Slider::new(&mut m.depth, -1.0..=1.0).fixed_decimals(2).text("Profondeur"))
            .on_hover_text("De combien le son pousse le bouton. Négatif : le son le fait baisser.");
        if m.source > 0 && m.depth == 0.0 {
            m.depth = 0.5;
        }
    })
    .response
    .on_hover_text("Lien avec le son");
}

/// Short name for a knob, for the summary of links.
pub fn knob_label(name: &str) -> String {
    use crate::params::{AMP_KNOBS, CAM_GROUPS, CAM_KNOBS, VHS_KNOBS};
    AMP_KNOBS
        .iter()
        .chain(VHS_KNOBS.iter())
        .chain(CAM_KNOBS.iter())
        .chain(CAM_GROUPS.iter().flat_map(|g| g.1.iter()))
        .find(|k| k.0 == name)
        .map_or_else(
            || match name.strip_prefix("ov") {
                Some(r) => format!("Calque {} · {}", r.get(..1).unwrap_or("?"), if r.ends_with("opacity") { "Opacité" } else { "Taille" }),
                None => name.to_string(),
            },
            |k| k.1.to_string(),
        )
}
