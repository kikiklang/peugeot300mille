//! The "Image" and "Caméra" tabs.

use super::link::Link;
use super::overlay::{OverlayPanel, OverlaySlot};
use super::view::View;
use crate::engine::lut::Lut;
use crate::params::{Params, AMP_KNOBS, CAMERAS, CAM_GROUPS, CAM_KNOBS, INPUT_KNOBS, MIRRORS, VHS_KNOBS};
use crate::player::Cmd;
use crate::ui::{self, card, knob_grid, ACCENT, WEAK};
use eframe::egui::{self, RichText};
use std::sync::Arc;

/// What the "Image" tab works on.
pub struct ImageTab<'a> {
    pub p: &'a mut Params,
    pub st: &'a View,
    pub slots: &'a mut [OverlaySlot; 3],
    pub link: &'a Link,
}

/// LUT file chooser with its dose.
fn lut_ui(ui: &mut egui::Ui, output: bool, name: &Option<String>, amount: &mut f32, link: &Link) {
    ui.horizontal(|ui| {
        if ui.button(if name.is_some() { "Changer…" } else { "Charger une LUT…" }).on_hover_text("Fichier .cube (color.io, Resolve, Kdenlive…)").clicked() {
            if let Some(f) = rfd::FileDialog::new().add_filter("LUT .cube", &["cube", "CUBE"]).pick_file() {
                match Lut::load(&f) {
                    Ok(l) => link.send(Cmd::SetLut { output, lut: Some(Arc::new(l)) }),
                    Err(e) => link.say(format!("{e:#}")),
                }
            }
        }
        match name {
            Some(n) => {
                ui.label(RichText::new(n).color(ACCENT));
                if ui.small_button("Retirer").clicked() {
                    link.send(Cmd::SetLut { output, lut: None });
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

pub fn image_tab(ui: &mut egui::Ui, c: &mut ImageTab) {
    let ImageTab { p, st, slots, link } = c;
    let sound = st.has_sound;
    card(ui, "Entrée", "Avant le circuit : ce qui entre colore toute la boucle.", |ui| {
        egui::Grid::new("input").num_columns(3).spacing([12.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
            for (knob, label, help) in INPUT_KNOBS {
                ui::knob_row(ui, p, knob, label, help, false);
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        lut_ui(ui, false, &st.lut_in, &mut p.lut_in_amount, link);
    });
    card(ui, "Calques vidéo", "Des boucles vidéo par-dessus l'image. « Dans le circuit » : les effets les abîment aussi.", |ui| {
        for (slot, state) in slots.iter_mut().enumerate() {
            let bpm = st.bpm.map(|b| b * 2f32.powi(p.tempo_octave as i32));
            OverlayPanel { slot, p, info: st.overlays[slot].as_ref(), state, link, sound, bpm }.show(ui);
        }
    });
    card(ui, "Amp", "Le boîtier : un enhancer dont la sortie revient à l'entrée, et le mélangeur de synchro.", |ui| {
        knob_grid(ui, "amp", p, &AMP_KNOBS, sound);
    });
    card(ui, "Magnétoscope VHS", "Entre le boîtier et la télé, sur le vrai signal.", |ui| {
        knob_grid(ui, "vhs", p, &VHS_KNOBS, sound);
    });
    card(ui, "Sortie", "Une LUT sur l'image finale (affichée et enregistrée).", |ui| {
        lut_ui(ui, true, &st.lut_out, &mut p.lut_out_amount, link);
    });
}

pub fn camera_tab(ui: &mut egui::Ui, p: &mut Params, st: &View) {
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
