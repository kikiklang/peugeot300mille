use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
}

impl Default for Overlay {
    fn default() -> Self {
        Self { opacity: 1.0, size: 0.5, x: 0.0, y: 0.0, speed: 1.0, mode: 0, place: 0, beats: 0 }
    }
}

/// Number of knobs the sound can drive.
pub const N_KNOBS: usize = 35;

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
    pub vhs_dropouts: f32,
    pub vhs_jitter: f32,
    pub vhs_streaks: f32,
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
    /// How each knob follows the sound, in [`KNOBS`] order.
    #[serde(deserialize_with = "lenient_mods", serialize_with = "mods_as_list")]
    pub mods: [Mod; N_KNOBS],
    /// The three overlay slots.
    pub overlays: [Overlay; 3],
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
            vhs_dropouts: 0.0,
            vhs_jitter: 0.0,
            vhs_streaks: 0.0,
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
            mods: [Mod::default(); N_KNOBS],
            overlays: [Overlay::default(); 3],
            sound_amount: 1.0,
            bands: DEFAULT_BANDS,
            tempo_octave: 0,
            tempo_phase: 0.0,
            tempo_restart: -1.0,
        }
    }
}

fn mods_as_list<S: Serializer>(m: &[Mod; N_KNOBS], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(m.iter())
}

/// Older saved settings have fewer entries: missing ones are "not driven".
fn lenient_mods<'de, D: Deserializer<'de>>(d: D) -> Result<[Mod; N_KNOBS], D::Error> {
    let v: Vec<Mod> = Vec::deserialize(d)?;
    let mut out = [Mod::default(); N_KNOBS];
    for (o, m) in out.iter_mut().zip(v) {
        *o = m;
    }
    Ok(out)
}

/// Knobs the sound can drive, in modulation order, with their range. New knobs go at the end so
/// saved settings keep their links.
pub const KNOBS: [(&str, f32, f32); N_KNOBS] = [
    ("enhancer_gain", 0.0, 1.0),
    ("enhancer_tone", 0.0, 1.0),
    ("feedback_gain", 0.0, 1.0),
    ("feedback_tone", 0.0, 1.0),
    ("sync", 0.0, 1.0),
    ("mix", 0.0, 1.0),
    ("blur", 0.0, 1.0),
    ("color", 0.0, 1.0),
    ("noise", 0.0, 1.0),
    ("vhs_wear", 0.0, 1.0),
    ("vhs_tracking", 0.0, 1.0),
    ("vhs_dropouts", 0.0, 1.0),
    ("vhs_jitter", 0.0, 1.0),
    ("cam_pan", -1.0, 1.0),
    ("cam_tilt", -1.0, 1.0),
    ("cam_zoom", -1.0, 1.0),
    ("cam_rotate", -1.0, 1.0),
    ("cam_drift", 0.0, 1.0),
    ("cam_shake", 0.0, 1.0),
    ("cam_hue", -1.0, 1.0),
    ("cam_negative", 0.0, 1.0),
    ("tube_lag", 0.0, 1.0),
    ("tube_bloom", 0.0, 1.0),
    ("tube_burn", 0.0, 1.0),
    ("lens_fisheye", -1.0, 1.0),
    ("lens_vignette", 0.0, 1.0),
    ("lens_ca", 0.0, 1.0),
    ("lens_moire", 0.0, 1.0),
    ("vhs_streaks", 0.0, 1.0),
    ("ov1_opacity", 0.0, 1.0),
    ("ov1_size", 0.0, 1.0),
    ("ov2_opacity", 0.0, 1.0),
    ("ov2_size", 0.0, 1.0),
    ("ov3_opacity", 0.0, 1.0),
    ("ov3_size", 0.0, 1.0),
];

pub fn knob_index(name: &str) -> usize {
    KNOBS.iter().position(|k| k.0 == name).unwrap_or_else(|| panic!("unknown knob {name}"))
}

/// Mirrors in front of the camera lens.
pub const MIRRORS: [&str; 6] = ["Aucun", "Miroir gauche/droite", "Miroir haut/bas", "Quatre miroirs", "Kaléidoscope 6", "Kaléidoscope 8"];

/// Camera sub-groups for the UI: (title, [(field, label, help)]).
pub const CAM_GROUPS: [(&str, &[(&str, &str, &str)]); 4] = [
    (
        "Cadrage",
        &[
            ("cam_pan", "Pan", "Tourne la caméra à gauche / à droite (s'ajoute à la position choisie)."),
            ("cam_tilt", "Tilt", "Lève / baisse la caméra."),
            ("cam_zoom", "Zoom", "Positif = zoom avant : le tunnel s'ouvre ; négatif = il se creuse."),
            ("cam_rotate", "Rotation", "Fait tourner la caméra : la spirale tourne à chaque passage."),
            ("cam_drift", "Dérive", "Le pied n'est pas fixe : la caméra glisse lentement toute seule."),
            ("cam_shake", "Secousses", "Caméra à l'épaule. Branchée sur les attaques, elle encaisse les coups."),
        ],
    ),
    (
        "Couleur par passage",
        &[
            ("cam_hue", "Teinte / passage", "La teinte tourne un peu à chaque tour de boucle : tunnels arc-en-ciel."),
            ("cam_negative", "Négatif", "Inverseur de polarité : chaque passage inverse la lumière (les couleurs restent)."),
        ],
    ),
    (
        "Tube",
        &[
            ("tube_lag", "Traînées", "Rémanence du tube : les lumières laissent des queues de comète."),
            ("tube_bloom", "Halo", "Les hautes lumières bavent autour d'elles."),
            ("tube_burn", "Marquage", "Ce qui reste longtemps à l'image s'imprime en fantôme."),
        ],
    ),
    (
        "Optique",
        &[
            ("lens_fisheye", "Fisheye", "Positif = grand-angle bombé, négatif = coussinet."),
            ("lens_vignette", "Vignettage", "Les bords et les coins s'assombrissent."),
            ("lens_ca", "Aberration", "Franges colorées sur les bords (en plus de l'aberration d'origine)."),
            ("lens_moire", "Moiré", "La caméra voit les lignes et la grille de l'écran : franges de moiré."),
        ],
    ),
];

/// Ready-made sound reactions: (name, [(knob, source, depth)]).
pub const SOUND_PRESETS: [(&str, &[(&str, u8, f32)]); 7] = [
    ("Aucun", &[]),
    // Sources: 1 volume, 5 attacks, 6 band 1 (kick), 7 band 2 (snare), 8 band 3 (voice), 9 band 4 (hi-hat).
    ("Pulse sur le kick", &[("feedback_gain", 6, 0.45), ("enhancer_gain", 6, 0.35), ("blur", 6, 0.15)]),
    ("Tunnel qui respire", &[("mix", 1, 0.35), ("color", 8, 0.3), ("blur", 6, 0.25)]),
    ("Télé qui décroche", &[("sync", 7, 0.45), ("noise", 9, 0.35), ("feedback_gain", 6, 0.2)]),
    ("Cassette malmenée", &[("vhs_dropouts", 7, 0.6), ("vhs_tracking", 6, 0.4), ("vhs_jitter", 9, 0.5)]),
    (
        "Tout réagit",
        &[("feedback_gain", 6, 0.4), ("enhancer_gain", 9, 0.4), ("sync", 7, 0.3), ("mix", 1, 0.3), ("noise", 9, 0.25), ("vhs_dropouts", 5, 0.4)],
    ),
    // Sources: 11 pulse each beat, 13 pulse each 4 beats, 15 slow wave over 4 beats.
    ("Calé sur le tempo", &[("feedback_gain", 11, 0.4), ("enhancer_gain", 11, 0.25), ("sync", 13, 0.3), ("color", 15, 0.3)]),
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

/// Input processor sliders: (field, label, help, min, max).
pub const INPUT_KNOBS: [(&str, &str, &str, f32, f32); 5] = [
    ("brightness", "Luminosité", "Remonte ou baisse toute l'image.", -1.0, 1.0),
    ("contrast", "Contraste", "Écarte ou rapproche les noirs et les blancs.", -1.0, 1.0),
    ("gamma", "Gamma", "Éclaircit (positif) ou assombrit (négatif) les tons moyens, sans toucher aux extrêmes.", -1.0, 1.0),
    ("saturation", "Saturation", "-1 = noir et blanc, +1 = couleurs doublées.", -1.0, 1.0),
    ("hue", "Teinte", "Fait tourner toutes les couleurs (degrés).", -180.0, 180.0),
];

/// (field name, label, help) for the 0..1 sliders, in UI order.
pub const AMP_KNOBS: [(&str, &str, &str); 5] = [
    ("enhancer_gain", "Enhancer Gain", "How strongly the enhancer boosts edges. Up high, edges ring and oscillate."),
    ("enhancer_tone", "Enhancer Tone", "How wide the edges it boosts are."),
    ("feedback_gain", "Feedback Gain", "How much of the output goes back to the input. Never fully off."),
    ("feedback_tone", "Feedback Tone", "How wide the changes fed back are. Up high, brightness itself is fed back."),
    ("sync", "Sync", "Sync mixer: locked → rolling → lifting to white → white with grain → snow → sinking to black. 0 = out."),
];

pub const VHS_KNOBS: [(&str, &str, &str); 5] = [
    ("vhs_wear", "Usure", "Bande fatiguée : image moins nette, couleur qui bave et traîne derrière, teinte qui frémit, souffle."),
    ("vhs_tracking", "Tracking", "Réglage de piste raté : une bande où l'image glisse, perd sa couleur et se déchire."),
    ("vhs_streaks", "Traits", "Les traits blancs qui défilent dans la bande de tracking."),
    ("vhs_dropouts", "Drop-outs", "Manques d'oxyde : traits blancs, ou ligne du dessus répétée."),
    ("vhs_jitter", "Instabilité", "Lignes qui tremblent, haut de l'image qui ondule, commutation des têtes visible en bas."),
];

pub const CAM_KNOBS: [(&str, &str, &str); 4] = [
    ("mix", "Mix", "How much of the camera's picture of the screen is mixed back. 0 = camera out."),
    ("blur", "Blur", "The lens's blur: fine detail dies and only broad light travels round."),
    ("color", "Color", "How much colour the camera gives back (0 normal, 1 = +12 dB)."),
    ("noise", "Noise", "The camera's gain turned up by hand, and the grain that comes with it."),
];

impl Params {
    pub fn knob_mut(&mut self, name: &str) -> &mut f32 {
        match name {
            "brightness" => &mut self.brightness,
            "contrast" => &mut self.contrast,
            "gamma" => &mut self.gamma,
            "saturation" => &mut self.saturation,
            "hue" => &mut self.hue,
            "lut_in_amount" => &mut self.lut_in_amount,
            "enhancer_gain" => &mut self.enhancer_gain,
            "enhancer_tone" => &mut self.enhancer_tone,
            "feedback_gain" => &mut self.feedback_gain,
            "feedback_tone" => &mut self.feedback_tone,
            "sync" => &mut self.sync,
            "vhs_wear" => &mut self.vhs_wear,
            "vhs_tracking" => &mut self.vhs_tracking,
            "vhs_dropouts" => &mut self.vhs_dropouts,
            "vhs_jitter" => &mut self.vhs_jitter,
            "vhs_streaks" => &mut self.vhs_streaks,
            "mix" => &mut self.mix,
            "blur" => &mut self.blur,
            "color" => &mut self.color,
            "noise" => &mut self.noise,
            "lut_out_amount" => &mut self.lut_out_amount,
            "cam_pan" => &mut self.cam_pan,
            "cam_tilt" => &mut self.cam_tilt,
            "cam_zoom" => &mut self.cam_zoom,
            "cam_rotate" => &mut self.cam_rotate,
            "cam_drift" => &mut self.cam_drift,
            "cam_shake" => &mut self.cam_shake,
            "cam_hue" => &mut self.cam_hue,
            "cam_negative" => &mut self.cam_negative,
            "tube_lag" => &mut self.tube_lag,
            "tube_bloom" => &mut self.tube_bloom,
            "tube_burn" => &mut self.tube_burn,
            "lens_fisheye" => &mut self.lens_fisheye,
            "lens_vignette" => &mut self.lens_vignette,
            "lens_ca" => &mut self.lens_ca,
            "lens_moire" => &mut self.lens_moire,
            n if n.starts_with("ov") && n.len() > 4 => {
                let i = (n.as_bytes()[2] - b'1') as usize;
                let o = &mut self.overlays[i.min(2)];
                match &n[4..] {
                    "opacity" => &mut o.opacity,
                    "size" => &mut o.size,
                    "x" => &mut o.x,
                    "y" => &mut o.y,
                    "speed" => &mut o.speed,
                    _ => panic!("unknown knob {name}"),
                }
            }
            _ => panic!("unknown knob {name}"),
        }
    }

    /// Applies a sound-reaction preset (keeps the knobs' own values).
    pub fn set_sound_preset(&mut self, i: usize) {
        self.mods = [Mod::default(); N_KNOBS];
        for &(name, source, depth) in SOUND_PRESETS[i].1 {
            self.mods[knob_index(name)] = Mod { source, depth };
        }
    }

    /// The knobs as the sound pushes them this frame. `feat` = volume, bass, mid, treble, hits.
    pub fn modulated(&self, feat: &[f32; crate::audio::N_FEAT]) -> Params {
        let mut p = *self;
        for (k, m) in self.mods.iter().enumerate() {
            if m.source > 0 && m.depth != 0.0 {
                let f = feat[(m.source as usize - 1).min(feat.len() - 1)];
                let (name, lo, hi) = KNOBS[k];
                *p.knob_mut(name) += m.depth * f * self.sound_amount * (hi - lo);
            }
        }
        p.clamped()
    }

    pub fn clamped(mut self) -> Self {
        self.replace_nan();
        for (name, lo, hi) in KNOBS {
            let v = self.knob_mut(name);
            *v = v.clamp(lo, hi);
        }
        self.mirror = self.mirror.min(MIRRORS.len() as u8 - 1);
        for o in self.overlays.iter_mut() {
            o.opacity = o.opacity.clamp(0.0, 1.0);
            o.size = o.size.clamp(0.0, 1.0);
            o.x = o.x.clamp(-1.0, 1.0);
            o.y = o.y.clamp(-1.0, 1.0);
            o.speed = o.speed.clamp(-4.0, 4.0);
            o.mode = o.mode.min(crate::engine::overlay::MODES.len() as u8 - 1);
            o.place = o.place.min(1);
            if !crate::engine::overlay::LOOP_BEATS.contains(&o.beats) {
                o.beats = 0;
            }
        }
        for (name, _, _, lo, hi) in INPUT_KNOBS {
            let v = self.knob_mut(name);
            *v = v.clamp(lo, hi);
        }
        self.lut_in_amount = self.lut_in_amount.clamp(0.0, 1.0);
        self.lut_out_amount = self.lut_out_amount.clamp(0.0, 1.0);
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
        let d = Params::default();
        let names = KNOBS.iter().map(|k| k.0).chain(INPUT_KNOBS.iter().map(|k| k.0)).chain(["lut_in_amount", "lut_out_amount"]);
        for name in names {
            let fallback = *{ d }.knob_mut(name);
            let v = self.knob_mut(name);
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
        for (o, od) in self.overlays.iter_mut().zip(&d.overlays) {
            fix(&mut o.x, od.x);
            fix(&mut o.y, od.y);
            fix(&mut o.speed, od.speed);
        }
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
        assert_eq!(p.mods[N_KNOBS - 1], Mod::default());
        assert_eq!(p.overlays, [Overlay::default(); 3]);
        assert_eq!(p.bands[0].sens, 0.0);
        assert_eq!(p.lut_in_amount, 1.0);
        // Longer lists than this version knows are cut, not refused.
        let long = format!(r#"{{"mods":[{}]}}"#, vec![r#"{"source":2,"depth":0.1}"#; N_KNOBS + 5].join(","));
        assert_eq!(serde_json::from_str::<Params>(&long).unwrap().mods[N_KNOBS - 1].source, 2);
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
        for bad in ["", "{", "null", r#"{"mix":"x"}"#, r#"{"mods":3}"#, r#"{"camera":-1}"#, r#"{"overlays":[{}]}"#] {
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
