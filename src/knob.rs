//! Every slider of the box, as a type: a wrong name no longer compiles, and each knob's saved
//! name, range and place in [`Params`] are written once.

use crate::params::Params;

macro_rules! knobs {
    ($($variant:ident = $name:literal, $lo:expr, $hi:expr, |$p:ident| $field:expr;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Knob {
            $($variant,)*
        }

        impl Knob {
            pub const ALL: &'static [Knob] = &[$(Knob::$variant,)*];

            /// The name used in saved settings: it must never change.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Knob::$variant => $name,)*
                }
            }

            pub const fn range(self) -> (f32, f32) {
                match self {
                    $(Knob::$variant => ($lo, $hi),)*
                }
            }

            pub fn get_mut(self, params: &mut Params) -> &mut f32 {
                match self {
                    $(Knob::$variant => {
                        let $p = params;
                        &mut $field
                    })*
                }
            }
        }
    };
}

knobs! {
    Brightness = "brightness", -1.0, 1.0, |p| p.brightness;
    Contrast = "contrast", -1.0, 1.0, |p| p.contrast;
    Gamma = "gamma", -1.0, 1.0, |p| p.gamma;
    Saturation = "saturation", -1.0, 1.0, |p| p.saturation;
    Hue = "hue", -180.0, 180.0, |p| p.hue;
    LutIn = "lut_in_amount", 0.0, 1.0, |p| p.lut_in_amount;
    EnhancerGain = "enhancer_gain", 0.0, 1.0, |p| p.enhancer_gain;
    EnhancerTone = "enhancer_tone", 0.0, 1.0, |p| p.enhancer_tone;
    FeedbackGain = "feedback_gain", 0.0, 1.0, |p| p.feedback_gain;
    FeedbackTone = "feedback_tone", 0.0, 1.0, |p| p.feedback_tone;
    Sync = "sync", 0.0, 1.0, |p| p.sync;
    VhsWear = "vhs_wear", 0.0, 1.0, |p| p.vhs_wear;
    VhsTracking = "vhs_tracking", 0.0, 1.0, |p| p.vhs_tracking;
    VhsJitter = "vhs_jitter", 0.0, 1.0, |p| p.vhs_jitter;
    Mix = "mix", 0.0, 1.0, |p| p.mix;
    Blur = "blur", 0.0, 1.0, |p| p.blur;
    Color = "color", 0.0, 1.0, |p| p.color;
    Noise = "noise", 0.0, 1.0, |p| p.noise;
    CamPan = "cam_pan", -1.0, 1.0, |p| p.cam_pan;
    CamTilt = "cam_tilt", -1.0, 1.0, |p| p.cam_tilt;
    CamZoom = "cam_zoom", -1.0, 1.0, |p| p.cam_zoom;
    CamRotate = "cam_rotate", -1.0, 1.0, |p| p.cam_rotate;
    CamDrift = "cam_drift", 0.0, 1.0, |p| p.cam_drift;
    CamShake = "cam_shake", 0.0, 1.0, |p| p.cam_shake;
    CamHue = "cam_hue", -1.0, 1.0, |p| p.cam_hue;
    CamNegative = "cam_negative", 0.0, 1.0, |p| p.cam_negative;
    TubeLag = "tube_lag", 0.0, 1.0, |p| p.tube_lag;
    TubeBloom = "tube_bloom", 0.0, 1.0, |p| p.tube_bloom;
    TubeBurn = "tube_burn", 0.0, 1.0, |p| p.tube_burn;
    LensFisheye = "lens_fisheye", -1.0, 1.0, |p| p.lens_fisheye;
    LensVignette = "lens_vignette", 0.0, 1.0, |p| p.lens_vignette;
    LensCa = "lens_ca", 0.0, 1.0, |p| p.lens_ca;
    LensMoire = "lens_moire", 0.0, 1.0, |p| p.lens_moire;
    LutOut = "lut_out_amount", 0.0, 1.0, |p| p.lut_out_amount;
    Ov1Opacity = "ov1_opacity", 0.0, 1.0, |p| p.overlays[0].opacity;
    Ov1Size = "ov1_size", 0.0, 1.0, |p| p.overlays[0].size;
    Ov1X = "ov1_x", -1.0, 1.0, |p| p.overlays[0].x;
    Ov1Y = "ov1_y", -1.0, 1.0, |p| p.overlays[0].y;
    Ov1Speed = "ov1_speed", -4.0, 4.0, |p| p.overlays[0].speed;
    Ov2Opacity = "ov2_opacity", 0.0, 1.0, |p| p.overlays[1].opacity;
    Ov2Size = "ov2_size", 0.0, 1.0, |p| p.overlays[1].size;
    Ov2X = "ov2_x", -1.0, 1.0, |p| p.overlays[1].x;
    Ov2Y = "ov2_y", -1.0, 1.0, |p| p.overlays[1].y;
    Ov2Speed = "ov2_speed", -4.0, 4.0, |p| p.overlays[1].speed;
    Ov3Opacity = "ov3_opacity", 0.0, 1.0, |p| p.overlays[2].opacity;
    Ov3Size = "ov3_size", 0.0, 1.0, |p| p.overlays[2].size;
    Ov3X = "ov3_x", -1.0, 1.0, |p| p.overlays[2].x;
    Ov3Y = "ov3_y", -1.0, 1.0, |p| p.overlays[2].y;
    Ov3Speed = "ov3_speed", -4.0, 4.0, |p| p.overlays[2].speed;
}

/// Number of knobs the sound can drive.
pub const N_DRIVEN: usize = 33;

/// The knobs the sound can drive, in the order of `Params::mods`.
pub const DRIVEN: [Knob; N_DRIVEN] = {
    use Knob::*;
    [
        EnhancerGain, EnhancerTone, FeedbackGain, FeedbackTone, Sync, Mix, Blur, Color, Noise, VhsWear, VhsTracking,
        VhsJitter, CamPan, CamTilt, CamZoom, CamRotate, CamDrift, CamShake, CamHue, CamNegative, TubeLag, TubeBloom,
        TubeBurn, LensFisheye, LensVignette, LensCa, LensMoire, Ov1Opacity, Ov1Size, Ov2Opacity, Ov2Size, Ov3Opacity,
        Ov3Size,
    ]
};

impl Knob {
    pub fn from_name(name: &str) -> Option<Knob> {
        Self::ALL.iter().copied().find(|k| k.name() == name)
    }

    /// Its place in `Params::mods`, if the sound can drive it.
    pub fn driven_slot(self) -> Option<usize> {
        DRIVEN.iter().position(|&k| k == self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found_back() {
        for &k in Knob::ALL {
            assert_eq!(Knob::from_name(k.name()), Some(k));
        }
    }

    #[test]
    fn every_knob_reaches_its_own_value() {
        for &k in Knob::ALL {
            let mut p = Params::default();
            *k.get_mut(&mut p) = 0.123;
            for &other in Knob::ALL {
                let expected = if other == k { 0.123 } else { *other.get_mut(&mut Params::default()) };
                assert_eq!(*other.get_mut(&mut p), expected, "{} touched {}", k.name(), other.name());
            }
        }
    }

    #[test]
    fn driven_knobs_are_listed_once() {
        for (i, k) in DRIVEN.iter().enumerate() {
            assert_eq!(k.driven_slot(), Some(i));
        }
    }
}
