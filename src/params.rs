use crate::engine::overlay::N_OVERLAYS;
use crate::knob::{Knob, DRIVEN, N_DRIVEN};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;

/// A knob driven by the sound: `source` indexes [`crate::audio::SOURCES`] (0 = none),
/// `depth` in -1..1 is how far the sound pushes the knob.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Mod {
    pub source: u8,
    pub depth: f32,
}

/// A frequency band of the soundtrack that can drive knobs: `mode` 0 = level, 1 = attacks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Band {
    pub lo: f32,
    pub hi: f32,
    pub mode: u8,
    /// Sensitivity in dB (-12..12): how hard the band drives the knobs.
    pub sens: f32,
}

impl Band {
    pub fn centre(&self) -> f32 {
        (self.lo * self.hi).sqrt()
    }
    /// Width in octaves.
    pub fn width(&self) -> f32 {
        (self.hi / self.lo).log2()
    }
    pub fn set(&mut self, centre: f32, width: f32) {
        let w = width.clamp(0.1, 6.0);
        let c = centre.clamp(20.0, 20000.0);
        self.lo = (c / 2f32.powf(w / 2.0)).max(20.0);
        self.hi = (c * 2f32.powf(w / 2.0)).min(20000.0);
    }
}

impl Default for Band {
    fn default() -> Self {
        DEFAULT_BANDS[0]
    }
}

/// Starting bands, aimed at a typical master: kick, snare, voice, hi-hats.
pub const DEFAULT_BANDS: [Band; 4] = [
    Band { lo: 40.0, hi: 110.0, mode: 1, sens: 0.0 },
    Band { lo: 150.0, hi: 300.0, mode: 1, sens: 0.0 },
    Band { lo: 300.0, hi: 3000.0, mode: 0, sens: 0.0 },
    Band { lo: 7000.0, hi: 14000.0, mode: 0, sens: 0.0 },
];
pub const BAND_HINTS: [&str; 4] = ["kick", "caisse claire", "voix", "hi-hat"];

/// One video loop laid over the picture (the file itself is chosen separately).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    pub opacity: f32,
    /// Height as a fraction of the picture.
    pub size: f32,
    /// Centre, -1..1.
    pub x: f32,
    pub y: f32,
    /// Playback speed (negative = backwards).
    pub speed: f32,
    /// Index into overlay::MODES.
    pub mode: u8,
    /// 0 = in the circuit (affected by every effect), 1 = on top of the finished picture.
    pub place: u8,
    /// Length of the loop in beats of the track, which it then follows (0 = free).
    pub beats: u8,
    /// Mirrored left/right.
    pub flip_h: bool,
    /// Upside down. With `flip_h`, a half turn.
    pub flip_v: bool,
}

impl Default for Overlay {
    fn default() -> Self {
        Self { opacity: 1.0, size: 0.5, x: 0.0, y: 0.0, speed: 1.0, mode: 0, place: 0, beats: 0, flip_h: false, flip_v: false }
    }
}

/// The knobs of the box. Every control starts at 0, which is a clean pass through the circuit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    // Input processor (before the circuit).
    pub brightness: f32,
    pub contrast: f32,
    pub gamma: f32,
    pub saturation: f32,
    /// Degrees.
    pub hue: f32,
    /// How much of the input LUT is applied (0..1).
    pub lut_in_amount: f32,
    // Amp.
    pub enhancer_gain: f32,
    pub enhancer_tone: f32,
    pub feedback_gain: f32,
    pub feedback_tone: f32,
    pub sync: f32,
    // VHS deck.
    pub vhs_wear: f32,
    pub vhs_tracking: f32,
    pub vhs_jitter: f32,
    // Camera loop.
    pub mix: f32,
    pub blur: f32,
    pub color: f32,
    pub noise: f32,
    /// Index into [`CAMERAS`].
    pub camera: u8,
    // Camera framing (on top of the chosen position).
    pub cam_pan: f32,
    pub cam_tilt: f32,
    pub cam_zoom: f32,
    pub cam_rotate: f32,
    pub cam_drift: f32,
    pub cam_shake: f32,
    // Camera colour, per pass round the loop.
    pub cam_hue: f32,
    pub cam_negative: f32,
    // Camera tube.
    pub tube_lag: f32,
    pub tube_bloom: f32,
    pub tube_burn: f32,
    // Lens.
    pub lens_fisheye: f32,
    pub lens_vignette: f32,
    pub lens_ca: f32,
    pub lens_moire: f32,
    /// Mirrors in front of the lens: index into [`MIRRORS`].
    pub mirror: u8,
    // Output.
    /// How much of the output LUT is applied (0..1).
    pub lut_out_amount: f32,
    /// How each knob follows the sound, in [`DRIVEN`] order. Saved by knob name.
    #[serde(deserialize_with = "saved_mods", serialize_with = "mods_by_name")]
    pub mods: [Mod; N_DRIVEN],
    /// The overlay slots.
    #[serde(deserialize_with = "fill_slots")]
    pub overlays: [Overlay; N_OVERLAYS],
    /// Overall strength of the sound's influence (0..2).
    pub sound_amount: f32,
    /// The four user frequency bands.
    pub bands: [Band; 4],
    /// Tempo correction in octaves: -1 = half time, +1 = double time.
    pub tempo_octave: i8,
    /// Shift of the beat grid, in beats.
    pub tempo_phase: f32,
    /// Where "Align start" was clicked, in seconds on the timeline (negative = never).
    pub tempo_restart: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 0.0,
            gamma: 0.0,
            saturation: 0.0,
            hue: 0.0,
            lut_in_amount: 1.0,
            enhancer_gain: 0.0,
            enhancer_tone: 0.0,
            feedback_gain: 0.0,
            feedback_tone: 0.0,
            sync: 0.0,
            vhs_wear: 0.0,
            vhs_tracking: 0.0,
            vhs_jitter: 0.0,
            mix: 0.0,
            blur: 0.0,
            color: 0.0,
            noise: 0.0,
            camera: 0,
            cam_pan: 0.0,
            cam_tilt: 0.0,
            cam_zoom: 0.0,
            cam_rotate: 0.0,
            cam_drift: 0.0,
            cam_shake: 0.0,
            cam_hue: 0.0,
            cam_negative: 0.0,
            tube_lag: 0.0,
            tube_bloom: 0.0,
            tube_burn: 0.0,
            lens_fisheye: 0.0,
            lens_vignette: 0.0,
            lens_ca: 0.0,
            lens_moire: 0.0,
            mirror: 0,
            lut_out_amount: 1.0,
            mods: [Mod::default(); N_DRIVEN],
            overlays: [Overlay::default(); N_OVERLAYS],
            sound_amount: 1.0,
            bands: DEFAULT_BANDS,
            tempo_octave: 0,
            tempo_phase: 0.0,
            tempo_restart: -1.0,
        }
    }
}

/// Up to v0.8 the links were saved as a plain list in this order. Frozen: it is what reads those
/// files back, including the links of knobs that no longer exist.
const LINKS_V08: [&str; 35] = [
    "enhancer_gain", "enhancer_tone", "feedback_gain", "feedback_tone", "sync", "mix", "blur", "color", "noise",
    "vhs_wear", "vhs_tracking", "vhs_dropouts", "vhs_jitter", "cam_pan", "cam_tilt", "cam_zoom", "cam_rotate",
    "cam_drift", "cam_shake", "cam_hue", "cam_negative", "tube_lag", "tube_bloom", "tube_burn", "lens_fisheye",
    "lens_vignette", "lens_ca", "lens_moire", "vhs_streaks", "ov1_opacity", "ov1_size", "ov2_opacity", "ov2_size",
    "ov3_opacity", "ov3_size",
];

/// A list saved with another number of slots: the missing ones are empty, extra ones are dropped.
pub fn fill_slots<'de, D, T, const N: usize>(d: D) -> Result<[T; N], D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let mut saved = Vec::<T>::deserialize(d)?.into_iter();
    Ok(std::array::from_fn(|_| saved.next().unwrap_or_default()))
}

fn mods_by_name<S: Serializer>(mods: &[Mod; N_DRIVEN], s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(DRIVEN.iter().zip(mods).filter(|(_, m)| **m != Mod::default()).map(|(k, m)| (k.name(), m)))
}

/// Links by name, or the positional list of older files. A knob that is gone loses its link.
fn saved_mods<'de, D: Deserializer<'de>>(d: D) -> Result<[Mod; N_DRIVEN], D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        ByName(HashMap<String, Mod>),
        ByPosition(Vec<Mod>),
    }
    let links: Vec<(String, Mod)> = match Saved::deserialize(d)? {
        Saved::ByName(map) => map.into_iter().collect(),
        Saved::ByPosition(list) => LINKS_V08.iter().map(|n| n.to_string()).zip(list).collect(),
    };
    let mut out = [Mod::default(); N_DRIVEN];
    for (name, m) in links {
        if let Some(slot) = Knob::from_name(&name).and_then(Knob::driven_slot) {
            out[slot] = m;
        }
    }
    Ok(out)
}

/// Mirrors in front of the camera lens.
pub const MIRRORS: [&str; 6] = ["Aucun", "Miroir gauche/droite", "Miroir haut/bas", "Quatre miroirs", "Kaléidoscope 6", "Kaléidoscope 8"];

/// A slider in the panel: (knob, label, help).
pub type KnobRow = (Knob, &'static str, &'static str);

/// Camera sub-groups for the UI: (title, rows).
pub const CAM_GROUPS: [(&str, &[KnobRow]); 4] = [
    (
        "Cadrage",
        &[
            (Knob::CamPan, "Pan", "Tourne la caméra à gauche / à droite (s'ajoute à la position choisie)."),
            (Knob::CamTilt, "Tilt", "Lève / baisse la caméra."),
            (Knob::CamZoom, "Zoom", "Positif = zoom avant : le tunnel s'ouvre ; négatif = il se creuse."),
            (Knob::CamRotate, "Rotation", "Fait tourner la caméra : la spirale tourne à chaque passage."),
            (Knob::CamDrift, "Dérive", "Le pied n'est pas fixe : la caméra glisse lentement toute seule."),
            (Knob::CamShake, "Secousses", "Caméra à l'épaule. Branchée sur les attaques, elle encaisse les coups."),
        ],
    ),
    (
        "Couleur par passage",
        &[
            (Knob::CamHue, "Teinte / passage", "La teinte tourne un peu à chaque tour de boucle : tunnels arc-en-ciel."),
            (Knob::CamNegative, "Négatif", "Inverseur de polarité : chaque passage inverse la lumière (les couleurs restent)."),
        ],
    ),
    (
        "Tube",
        &[
            (Knob::TubeLag, "Traînées", "Rémanence du tube : les lumières laissent des queues de comète."),
            (Knob::TubeBloom, "Halo", "Les hautes lumières bavent autour d'elles."),
            (Knob::TubeBurn, "Marquage", "Ce qui reste longtemps à l'image s'imprime en fantôme."),
        ],
    ),
    (
        "Optique",
        &[
            (Knob::LensFisheye, "Fisheye", "Positif = grand-angle bombé, négatif = coussinet."),
            (Knob::LensVignette, "Vignettage", "Les bords et les coins s'assombrissent."),
            (Knob::LensCa, "Aberration", "Franges colorées sur les bords (en plus de l'aberration d'origine)."),
            (Knob::LensMoire, "Moiré", "La caméra voit les lignes et la grille de l'écran : franges de moiré."),
        ],
    ),
];

/// Ready-made sound reactions: (name, [(knob, source, depth)]).
pub const SOUND_PRESETS: [(&str, &[(Knob, u8, f32)]); 7] = [
    ("Aucun", &[]),
    // Sources: 1 volume, 5 attacks, 6 band 1 (kick), 7 band 2 (snare), 8 band 3 (voice), 9 band 4 (hi-hat).
    ("Pulse sur le kick", &[(Knob::FeedbackGain, 6, 0.45), (Knob::EnhancerGain, 6, 0.35), (Knob::Blur, 6, 0.15)]),
    ("Tunnel qui respire", &[(Knob::Mix, 1, 0.35), (Knob::Color, 8, 0.3), (Knob::Blur, 6, 0.25)]),
    ("Télé qui décroche", &[(Knob::Sync, 7, 0.45), (Knob::Noise, 9, 0.35), (Knob::FeedbackGain, 6, 0.2)]),
    ("Cassette malmenée", &[(Knob::VhsWear, 7, 0.6), (Knob::VhsTracking, 6, 0.4), (Knob::VhsJitter, 9, 0.5)]),
    (
        "Tout réagit",
        &[(Knob::FeedbackGain, 6, 0.4), (Knob::EnhancerGain, 9, 0.4), (Knob::Sync, 7, 0.3), (Knob::Mix, 1, 0.3), (Knob::Noise, 9, 0.25), (Knob::VhsJitter, 5, 0.4)],
    ),
    // Sources: 11 pulse each beat, 13 pulse each 4 beats, 15 slow wave over 4 beats.
    ("Calé sur le tempo", &[(Knob::FeedbackGain, 11, 0.4), (Knob::EnhancerGain, 11, 0.25), (Knob::Sync, 13, 0.3), (Knob::Color, 15, 0.3)]),
];

/// Where the camera stands: name and (pan, tilt, zoom, roll), each in -1..1.
pub const CAMERAS: [(&str, [f32; 4]); 9] = [
    ("Center", [0.0, 0.0, 0.0, 0.0]),
    ("Pan Right", [1.0, 0.0, 0.0, 0.0]),
    ("Pan Left", [-1.0, 0.0, 0.0, 0.0]),
    ("Tilt Up", [0.0, 1.0, 0.0, 0.0]),
    ("Tilt Down", [0.0, -1.0, 0.0, 0.0]),
    ("Zoom In + Roll Right", [0.0, 0.0, 1.0, 1.0]),
    ("Zoom In + Roll Left", [0.0, 0.0, 1.0, -1.0]),
    ("Zoom Out + Roll Right", [0.0, 0.0, -1.0, 1.0]),
    ("Zoom Out + Roll Left", [0.0, 0.0, -1.0, -1.0]),
];

/// Input processor sliders.
pub const INPUT_KNOBS: [KnobRow; 5] = [
    (Knob::Brightness, "Luminosité", "Remonte ou baisse toute l'image."),
    (Knob::Contrast, "Contraste", "Écarte ou rapproche les noirs et les blancs."),
    (Knob::Gamma, "Gamma", "Éclaircit (positif) ou assombrit (négatif) les tons moyens, sans toucher aux extrêmes."),
    (Knob::Saturation, "Saturation", "-1 = noir et blanc, +1 = couleurs doublées."),
    (Knob::Hue, "Teinte", "Fait tourner toutes les couleurs (degrés)."),
];

pub const AMP_KNOBS: [KnobRow; 5] = [
    (Knob::EnhancerGain, "Enhancer Gain", "How strongly the enhancer boosts edges. Up high, edges ring and oscillate."),
    (Knob::EnhancerTone, "Enhancer Tone", "How wide the edges it boosts are."),
    (Knob::FeedbackGain, "Feedback Gain", "How much of the output goes back to the input. Never fully off."),
    (Knob::FeedbackTone, "Feedback Tone", "How wide the changes fed back are. Up high, brightness itself is fed back."),
    (Knob::Sync, "Sync", "Sync mixer: locked → rolling → lifting to white → white with grain → snow → sinking to black. 0 = out."),
];

pub const VHS_KNOBS: [KnobRow; 3] = [
    (Knob::VhsWear, "Usure", "Bande fatiguée : image moins nette, couleur qui bave et traîne derrière, teinte qui frémit, souffle."),
    (Knob::VhsTracking, "Tracking", "Réglage de piste raté : une bande où l'image glisse, perd sa couleur et se déchire."),
    (Knob::VhsJitter, "Instabilité", "Lignes qui tremblent, haut de l'image qui ondule, commutation des têtes visible en bas."),
];

pub const CAM_KNOBS: [KnobRow; 4] = [
    (Knob::Mix, "Mix", "How much of the camera's picture of the screen is mixed back. 0 = camera out."),
    (Knob::Blur, "Blur", "The lens's blur: fine detail dies and only broad light travels round."),
    (Knob::Color, "Color", "How much colour the camera gives back (0 normal, 1 = +12 dB)."),
    (Knob::Noise, "Noise", "The camera's gain turned up by hand, and the grain that comes with it."),
];

impl Params {
    /// Applies a sound-reaction preset (keeps the knobs' own values).
    pub fn set_sound_preset(&mut self, i: usize) {
        self.mods = [Mod::default(); N_DRIVEN];
        for &(knob, source, depth) in SOUND_PRESETS[i].1 {
            if let Some(slot) = knob.driven_slot() {
                self.mods[slot] = Mod { source, depth };
            }
        }
    }

    /// The knobs as the sound pushes them this frame. `feat` = volume, bass, mid, treble, hits.
    pub fn modulated(&self, feat: &[f32; crate::audio::N_FEAT]) -> Params {
        let mut p = *self;
        for (knob, m) in DRIVEN.iter().zip(&self.mods) {
            if m.source > 0 && m.depth != 0.0 {
                let f = feat[(m.source as usize - 1).min(feat.len() - 1)];
                let (lo, hi) = knob.range();
                *knob.get_mut(&mut p) += m.depth * f * self.sound_amount * (hi - lo);
            }
        }
        p.clamped()
    }

    pub fn clamped(mut self) -> Self {
        self.replace_nan();
        for knob in Knob::ALL {
            let (lo, hi) = knob.range();
            let v = knob.get_mut(&mut self);
            *v = v.clamp(lo, hi);
        }
        self.mirror = self.mirror.min(MIRRORS.len() as u8 - 1);
        for o in self.overlays.iter_mut() {
            o.mode = o.mode.min(crate::engine::overlay::MODES.len() as u8 - 1);
            o.place = o.place.min(1);
            if !crate::engine::overlay::LOOP_BEATS.contains(&o.beats) {
                o.beats = 0;
            }
        }
        self.camera = self.camera.min(CAMERAS.len() as u8 - 1);
        self.sound_amount = self.sound_amount.clamp(0.0, 2.0);
        for b in self.bands.iter_mut() {
            b.lo = b.lo.clamp(20.0, 20000.0);
            b.hi = b.hi.clamp(b.lo, 20000.0);
            b.mode = b.mode.min(1);
            b.sens = b.sens.clamp(-12.0, 12.0);
        }
        self.tempo_octave = self.tempo_octave.clamp(-2, 2);
        self.tempo_phase = self.tempo_phase.clamp(-2.0, 2.0);
        self.tempo_restart = if self.tempo_restart >= 0.0 { self.tempo_restart.min(86_400.0) } else { -1.0 };
        for m in self.mods.iter_mut() {
            m.source = m.source.min(crate::audio::N_FEAT as u8);
            m.depth = m.depth.clamp(-1.0, 1.0);
        }
        self
    }

    /// `clamp` lets NaN through, and one NaN in the feedback loop would stay in the picture for
    /// good: each one goes back to its default value.
    fn replace_nan(&mut self) {
        let mut d = Params::default();
        for knob in Knob::ALL {
            let fallback = *knob.get_mut(&mut d);
            let v = knob.get_mut(self);
            if v.is_nan() {
                *v = fallback;
            }
        }
        let fix = |v: &mut f32, fallback: f32| {
            if v.is_nan() {
                *v = fallback;
            }
        };
        fix(&mut self.sound_amount, d.sound_amount);
        fix(&mut self.tempo_phase, d.tempo_phase);
        for (b, bd) in self.bands.iter_mut().zip(&d.bands) {
            fix(&mut b.lo, bd.lo);
            fix(&mut b.hi, bd.hi);
            fix(&mut b.sens, bd.sens);
        }
        for m in self.mods.iter_mut() {
            fix(&mut m.depth, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_older_versions_still_load() {
        // Before the overlays and the tempo, with fewer modulation slots and no band `sens`.
        let old = r#"{"mix":0.4,"camera":2,"mods":[{"source":1,"depth":0.5}],"bands":[
            {"lo":40,"hi":110,"mode":1},{"lo":150,"hi":300,"mode":1},{"lo":300,"hi":3000},{"lo":7000,"hi":14000}]}"#;
        let p: Params = serde_json::from_str(old).unwrap();
        assert_eq!(p.mix, 0.4);
        assert_eq!(p.mods[0], Mod { source: 1, depth: 0.5 });
        assert_eq!(p.mods[N_DRIVEN - 1], Mod::default());
        assert_eq!(p.overlays, [Overlay::default(); N_OVERLAYS]);
        assert_eq!(p.bands[0].sens, 0.0);
        assert_eq!(p.lut_in_amount, 1.0);
        // Longer lists than this version knows are cut, not refused.
        let long = format!(r#"{{"mods":[{}]}}"#, vec![r#"{"source":2,"depth":0.1}"#; N_DRIVEN + 5].join(","));
        let slot = Knob::Ov3Size.driven_slot().unwrap();
        assert_eq!(serde_json::from_str::<Params>(&long).unwrap().mods[slot].source, 2);
    }

    #[test]
    fn links_of_v08_files_stay_on_their_knob() {
        let mut list = vec![r#"{"source":0,"depth":0.0}"#.to_string(); 35];
        for (i, source) in [(10, 3), (11, 4), (12, 5), (28, 6), (29, 7)] {
            list[i] = format!(r#"{{"source":{source},"depth":0.5}}"#);
        }
        let p: Params = serde_json::from_str(&format!(r#"{{"mods":[{}]}}"#, list.join(","))).unwrap();
        let source = |k: Knob| p.mods[k.driven_slot().unwrap()].source;
        assert_eq!(source(Knob::VhsTracking), 3);
        assert_eq!(source(Knob::VhsJitter), 5, "must not slide when Drop-outs goes");
        assert_eq!(source(Knob::Ov1Opacity), 7, "must not slide when Traits goes");
        assert_eq!(p.mods.iter().filter(|m| m.source != 0).count(), 3, "links of removed knobs are dropped");
    }

    #[test]
    fn settings_with_three_layers_still_load() {
        let p: Params = serde_json::from_str(r#"{"overlays":[{"opacity":0.1},{"opacity":0.2},{"opacity":0.3}]}"#).unwrap();
        assert_eq!((p.overlays[0].opacity, p.overlays[2].opacity), (0.1, 0.3));
        assert_eq!(p.overlays[3], Overlay::default());
    }

    #[test]
    fn links_are_saved_by_name() {
        let mut p = Params::default();
        p.mods[Knob::Sync.driven_slot().unwrap()] = Mod { source: 11, depth: 0.5 };
        let json: serde_json::Value = serde_json::to_value(p).unwrap();
        assert_eq!(json["mods"]["sync"]["source"], 11);
        assert_eq!(json["mods"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let mut p = Params { mix: 0.25, camera: 3, tempo_octave: -1, ..Params::default() };
        p.mods[7] = Mod { source: 11, depth: -0.5 };
        p.overlays[2].beats = 4;
        let back: Params = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn malformed_settings_are_refused_without_a_panic() {
        for bad in ["", "{", "null", r#"{"mix":"x"}"#, r#"{"mods":3}"#, r#"{"camera":-1}"#, r#"{"overlays":3}"#] {
            assert!(serde_json::from_str::<Params>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn clamping_makes_any_values_safe() {
        let mut p = Params { camera: 200, mirror: 200, mix: f32::NAN, hue: f32::INFINITY, sound_amount: f32::NAN, tempo_octave: 100, ..Params::default() };
        p.overlays[0].mode = 200;
        p.overlays[0].opacity = f32::NAN;
        p.overlays[1].speed = f32::NAN;
        p.bands[0] = Band { lo: f32::NAN, hi: -5.0, mode: 9, sens: f32::NAN };
        p.mods[0] = Mod { source: 200, depth: f32::NAN };
        let c = p.clamped();
        assert!((c.camera as usize) < CAMERAS.len() && (c.mirror as usize) < MIRRORS.len());
        assert_eq!((c.mix, c.hue, c.sound_amount, c.tempo_octave), (0.0, 180.0, 1.0, 2));
        assert_eq!((c.overlays[0].opacity, c.overlays[1].speed), (1.0, 1.0));
        assert!(c.bands[0].lo <= c.bands[0].hi && c.bands[0].sens == 0.0);
        assert_eq!(c.mods[0], Mod { source: crate::audio::N_FEAT as u8, depth: 0.0 });
        // The sound can't bring a NaN back in.
        let m = c.modulated(&[f32::NAN; crate::audio::N_FEAT]);
        assert!(!m.enhancer_gain.is_nan());
    }

    #[test]
    fn clamping_leaves_good_values_alone() {
        let p = Params { mix: 0.3, hue: -90.0, cam_pan: -0.5, ..Params::default() };
        assert_eq!(p.clamped(), p);
    }
}
