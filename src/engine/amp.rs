//! The video amp: an edge enhancer whose output is fed back into its own input, sample by sample.
//!
//! The structure follows the analysed box: sync-tip clamp, band-limited input, an edge (high-pass)
//! boost of 3.9× to 51.8×, soft asymmetric saturation towards the rails, and a non-linear feedback
//! path whose strength depends on how busy the signal is (a transistor's emitter resistance moves
//! with its current). The component values are ours: tune them in [`AmpTuning`].

use super::signal::*;
use crate::params::Params;

/// Knob-independent constants of the circuit, kept in one place for tuning.
pub struct AmpTuning {
    /// Input bandwidth (Hz).
    pub input_fc: f64,
    /// Enhancer edge filter cutoff at tone 0 and tone 1 (Hz).
    pub edge_fc: (f64, f64),
    /// HF gain range of the enhancer.
    pub gain_range: (f32, f32),
    /// Fraction of the HF gain actually applied to the edge signal.
    pub edge_scale: f32,
    /// Feedback path cutoff at tone 0 and tone 1 (Hz).
    pub fb_fc: (f64, f64),
    /// Feedback delay through the loop (samples). 4 samples = one subcarrier period.
    pub fb_delay: usize,
    /// Positive and negative headroom above / below blanking.
    pub rail_pos: f32,
    pub rail_neg: f32,
    /// Mains hum amplitude at feedback 0 and feedback 1.
    pub hum: (f32, f32),
}

pub const TUNING: AmpTuning = AmpTuning {
    input_fc: 5.5e6,
    edge_fc: (3.5e6, 0.25e6),
    gain_range: (3.9, 51.8),
    edge_scale: 0.55,
    fb_fc: (2.0e6, 0.03e6),
    fb_delay: 4,
    rail_pos: 0.85,
    rail_neg: 0.36,
    hum: (0.002, 0.010),
};

const FB_LEN: usize = 16;

#[derive(Clone, Copy)]
pub struct Amp {
    clamp: f32,
    lp_in: f32,
    hist: [f32; 5],
    lp_edge: f32,
    env: f32,
    lp_fb: f32,
    fb: [f32; FB_LEN],
    fb_i: usize,
    t: f64,
    rng: Rng,
}

impl Amp {
    pub fn new() -> Self {
        Self {
            clamp: -BLANK,
            lp_in: 0.0,
            hist: [0.0; 5],
            lp_edge: 0.0,
            env: 0.0,
            lp_fb: 0.0,
            fb: [0.0; FB_LEN],
            fb_i: 0,
            t: 0.0,
            rng: Rng(0x1234_5678),
        }
    }

    pub fn process(&mut self, sig: &mut [f32], p: &Params) {
        let tu = &TUNING;
        let g = tu.gain_range.0 * (tu.gain_range.1 / tu.gain_range.0).powf(p.enhancer_gain);
        // 0.15 at the lowest gain (a gentle TV sharpness), about 7 at the highest.
        let k_edge = 0.15 + (g / tu.gain_range.0 - 1.0) * tu.edge_scale;
        let a_in = rc(tu.input_fc);
        let a_edge = rc(lerp_log(tu.edge_fc, p.enhancer_tone as f64));
        let a_fb = rc(lerp_log(tu.fb_fc, p.feedback_tone as f64));
        let dc_frac = smoothstep(0.65, 1.0, p.feedback_tone) * 0.35;
        // Never fully off.
        let fb_gain = 0.02 + 1.25 * p.feedback_gain.powf(1.3);
        let hum_amp = tu.hum.0 + (tu.hum.1 - tu.hum.0) * p.feedback_gain;
        let d = tu.fb_delay.min(FB_LEN - 1);
        let w60 = 2.0 * std::f64::consts::PI * 60.0 * DT;

        let mut s = *self;
        for v in sig.iter_mut() {
            // Feedback returns into the input.
            let fbv = s.fb[(s.fb_i + FB_LEN - d) % FB_LEN];
            let mut x = *v - BLANK + fbv;
            // Sync-tip clamp: follows the tip down fast, relaxes up slowly.
            let k = if x < s.clamp { 0.05 } else { 3e-5 };
            s.clamp += k * (x - s.clamp);
            x -= s.clamp + BLANK;
            // Input bandwidth.
            s.lp_in += a_in * (x - s.lp_in);
            s.hist = [s.hist[1], s.hist[2], s.hist[3], s.hist[4], s.lp_in];
            // Two samples late, so the chroma trap can look either side.
            let xi = s.hist[2];
            // Edge enhancer, on luma only: a trap at the subcarrier keeps it off the colour.
            let luma = 0.25 * s.hist[0] + 0.5 * s.hist[2] + 0.25 * s.hist[4];
            s.lp_edge += a_edge * (luma - s.lp_edge);
            let hp = luma - s.lp_edge;
            s.env += 0.002 * (hp.abs() - s.env);
            let n = s.rng.tri() * 0.0025;
            let y = soft_clip(xi + k_edge * hp + n, tu.rail_pos, tu.rail_neg);
            // Feedback path: the changes (high-passed) and, up high, the brightness itself
            // (only the picture's, never the sync's).
            s.lp_fb += a_fb * (y - s.lp_fb);
            let src = (y - s.lp_fb) + dc_frac * s.lp_fb.max(0.0);
            let busy = 0.7 + 0.6 * (s.env / (s.env + 0.04));
            s.fb[s.fb_i] = src * fb_gain * busy;
            s.fb_i = (s.fb_i + 1) % FB_LEN;
            // Mains hum drifts slowly against the 59.94 Hz field.
            s.t += 1.0;
            let hum = hum_amp * ((s.t * w60).sin() as f32 + 0.4 * (s.t * 2.0 * w60).sin() as f32);
            *v = y + hum + BLANK;
        }
        *self = s;
    }
}


impl Default for Amp {
    fn default() -> Self {
        Self::new()
    }
}

fn lerp_log((a, b): (f64, f64), t: f64) -> f64 {
    a * (b / a).powf(t.clamp(0.0, 1.0))
}

/// Linear in the middle, saturating smoothly towards the rails.
#[inline]
pub fn soft_clip(v: f32, pos: f32, neg: f32) -> f32 {
    let kp = 0.7 * pos;
    let kn = 0.8 * neg;
    if v > kp {
        kp + (pos - kp) * ((v - kp) / (pos - kp)).tanh()
    } else if v < -kn {
        -kn - (neg - kn) * ((-v - kn) / (neg - kn)).tanh()
    } else {
        v
    }
}
