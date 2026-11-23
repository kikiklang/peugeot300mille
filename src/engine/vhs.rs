//! A worn VHS deck between the box and the TV, working on the composite signal line by line:
//!
//! * wear: luma loses bandwidth, colour is carried low ("colour under") so it smears and lags,
//!   hue wobbles from line to line, FM noise streaks,
//! * tracking: a band of noise that drifts up the picture; inside it, sync is lost too,
//! * instability: lines wobble (timebase error), the top of each field bends (flagging) and the
//!   head switch shows a few lines above the bottom.

use super::signal::*;
use crate::params::Params;
use rayon::prelude::*;

/// Below this knob value the deck fades in, instead of its filters switching on at once.
const FADE_IN: f32 = 0.05;

/// The knobs worked out for the current frame.
#[derive(Clone, Copy, Default)]
struct Setup {
    wear: f32,
    trk: f32,
    inst: f32,
    /// How much of the deck is heard: rises from 0 so that no knob jumps when it leaves zero.
    wet: f32,
    a_y: f32,
    a_c: f32,
    y_noise: f32,
    band_h: f32,
    band_c: f32,
}

pub struct Vhs {
    line: Vec<f32>,
    y: Vec<f32>,
    /// Scratch for the second sideways shift.
    scratch: Vec<f32>,
    /// The whole frame's filtered luma and colour-under, before any noise.
    luma: Vec<f32>,
    chroma_i: Vec<f32>,
    chroma_q: Vec<f32>,
    jitter: f32,
    slip: f32,
    hue: f32,
    band: f32,
    frame: u32,
    rng: Rng,
    setup: Setup,
}

impl Vhs {
    pub fn new() -> Self {
        Self {
            line: vec![0.0; SPL],
            y: vec![0.0; SPL],
            scratch: vec![0.0; SPL],
            luma: Vec::new(),
            chroma_i: Vec::new(),
            chroma_q: Vec::new(),
            jitter: 0.0,
            slip: 0.0,
            hue: 0.0,
            band: 0.3,
            frame: 0,
            rng: Rng(1),
            setup: Setup::default(),
        }
    }

    pub fn active(p: &Params) -> bool {
        p.vhs_wear > 0.0 || p.vhs_tracking > 0.0 || p.vhs_jitter > 0.0
    }

    /// Readies a frame, to be fed in order through [`Vhs::run`].
    pub fn begin(&mut self, p: &Params) {
        self.frame = self.frame.wrapping_add(1);
        let wear = p.vhs_wear;
        let trk = p.vhs_tracking;
        let inst = p.vhs_jitter;
        self.rng = Rng(0x2545_f491 ^ self.frame.wrapping_mul(0x9e37_79b9) | 1);

        let a_y = rc(3.4e6 * (1.8 / 3.4f64).powf(wear as f64)); // luma bandwidth
        let a_c = rc(1.0e6 * (0.3 / 1.0f64).powf(wear as f64)); // colour-under bandwidth
        let y_noise = 0.004 + 0.05 * wear;
        // Tracking band drifts slowly; its height grows with the error.
        self.band = (self.band + 0.004 + 0.01 * trk).fract();
        let band_h = 5.0 + 32.0 * trk;
        let band_c = self.band * LINES as f32;
        let wet = (wear.max(trk).max(inst) / FADE_IN).min(1.0);
        self.setup = Setup { wear, trk, inst, wet, a_y, a_c, y_noise, band_h, band_c };
    }

    /// A block of whole lines starting at line `l0`. `abs0` is the absolute index of the frame's
    /// first sample (keeps the subcarrier phase).
    pub fn run(&mut self, sig: &mut [f32], l0: usize, abs0: i64) {
        let Setup { wear, trk, inst, wet, a_y, a_c, y_noise, band_h, band_c } = self.setup;
        let mut rng = self.rng;
        let n_lines = sig.len() / SPL;

        // The filters use no random numbers, so every line can run at once; only the noise, whose
        // generator is shared by all the lines, has to go in order below.
        self.luma.resize(sig.len(), 0.0);
        self.chroma_i.resize(sig.len(), 0.0);
        self.chroma_q.resize(sig.len(), 0.0);
        sig.par_chunks(SPL)
            .zip(self.luma.par_chunks_mut(SPL))
            .zip(self.chroma_i.par_chunks_mut(SPL))
            .zip(self.chroma_q.par_chunks_mut(SPL))
            .enumerate()
            .for_each(|(l, (((src, y), i), q))| {
                let phase = ((abs0 + ((l0 + l) * SPL) as i64) & 3) as usize;
                filter_line(src, y, i, q, phase, a_y, a_c);
            });

        for n in 0..n_lines {
            let l = l0 + n;
            let a = l * SPL;
            let b = n * SPL; // where the line sits in this block
            let phase = ((abs0 + a as i64) & 3) as usize;
            let field = (a >= FIELD) as usize;
            let in_field = (a as f32 - (field * FIELD) as f32) / SPL as f32; // lines since field start

            let (luma, ci, cq) = (&self.luma[b..b + SPL], &self.chroma_i[b..b + SPL], &self.chroma_q[b..b + SPL]);
            // Streaky FM noise on the luma, drawn from the end of the line.
            let mut n = 0.0f32;
            for (v, s) in self.y.iter_mut().zip(luma).rev() {
                n += (rng.tri() - n) * 0.35;
                *v = s + n * y_noise;
            }
            // Colour, with its hue wobble.
            self.hue += (rng.tri() * 0.25 - self.hue) * 0.5;
            let (hs, hc) = (self.hue * wear).sin_cos();
            for (k, v) in self.line.iter_mut().enumerate() {
                let (si, sq) = (ci[k], cq[k]);
                let (ii, qq) = (si * hc - sq * hs, si * hs + sq * hc);
                let ph = (phase + k) & 3;
                *v = self.y[k] + (ii * COS4[ph] + qq * SIN4[ph]) * (1.0 + rng.tri() * 0.15 * wear);
            }

            // --- Timebase: per-line wobble, flagging at the top of each field, head switch.
            self.jitter += (rng.tri() - self.jitter) * 0.08;
            let mut dx = inst * 4.0 * self.jitter;
            let wob = ((self.frame as f32) * 0.37).sin() * 0.5 + 0.5;
            if in_field < 30.0 {
                dx += inst * 24.0 * (1.0 - in_field / 30.0).powi(2) * (0.4 + 0.6 * wob);
            }
            let switch_l = 262.5 - 6.0;
            let near_switch = in_field > switch_l && in_field < switch_l + 4.0;
            if near_switch && (inst > 0.0 || wear > 0.0) {
                dx += (inst * 30.0 + wear * 8.0) * (1.0 - (in_field - switch_l) / 4.0);
            }
            if dx.abs() > 0.01 {
                shift(&mut self.line, &mut self.y, dx);
            }

            // --- Tracking band (wraps round the frame): the heads leave the track, so lines slip
            // sideways, colour is lost and sync weakens (the TV tears there).
            let mut d = (l as f32 - band_c).abs();
            d = d.min(LINES as f32 - d);
            if trk > 0.0 && d < band_h {
                let strength = (trk.powf(0.9) * (1.3 - d / band_h)).min(1.0);
                // Colour gone: back to luma only.
                for (v, y) in self.line.iter_mut().zip(&self.y) {
                    *v += (*y - *v) * strength;
                }
                // Sideways slip.
                self.slip += (rng.tri() * 110.0 - self.slip) * 0.35;
                shift(&mut self.line, &mut self.scratch, self.slip * strength);
                // Weak sync and a washed, lifted picture.
                // Sync mostly gone where the heads are off the track.
                let lose_sync = rng.unit() < strength * 0.9;
                for v in self.line.iter_mut() {
                    if *v < BLANK {
                        *v += (BLANK - *v) * if lose_sync { 1.0 } else { strength * 0.6 };
                    } else {
                        *v += (BLACK + 0.25 - *v) * strength * 0.35;
                    }
                }
            }
            if wet >= 1.0 {
                sig[b..b + SPL].copy_from_slice(&self.line);
            } else {
                for (v, l) in sig[b..b + SPL].iter_mut().zip(&self.line) {
                    *v += (l - *v) * wet;
                }
            }
        }
        self.rng = rng;
    }
}

/// Y / C split of one line. Luma is band-limited both ways; colour is carried low ("colour under"):
/// a one-way low-pass, so it smears and lags behind the picture.
fn filter_line(src: &[f32], y: &mut [f32], i: &mut [f32], q: &mut [f32], phase: usize, a_y: f32, a_c: f32) {
    for k in 0..SPL {
        let s = |j: isize| src[(k as isize + j).clamp(0, SPL as isize - 1) as usize];
        let l = 0.25 * s(-2) + 0.5 * s(0) + 0.25 * s(2);
        let c = src[k] - l;
        let ph = (phase + k) & 3;
        y[k] = l;
        i[k] = 2.0 * c * COS4[ph];
        q[k] = 2.0 * c * SIN4[ph];
    }
    let mut s = y[0];
    for v in y.iter_mut() {
        s += (*v - s) * a_y;
        *v = s;
    }
    let mut s = y[SPL - 1];
    for v in y.iter_mut().rev() {
        s += (*v - s) * a_y;
        *v = s;
    }
    let (mut si, mut sq) = (0.0f32, 0.0f32);
    for (vi, vq) in i.iter_mut().zip(q.iter_mut()) {
        si += (*vi - si) * a_c;
        sq += (*vq - sq) * a_c;
        (*vi, *vq) = (si, sq);
    }
}

/// Delays a line by `dx` samples (linear interpolation), using `tmp` as scratch.
fn shift(line: &mut [f32], tmp: &mut [f32], dx: f32) {
    tmp.copy_from_slice(line);
    let n = line.len() as isize;
    let di = dx.floor() as isize;
    let f = dx - di as f32;
    for (k, v) in line.iter_mut().enumerate() {
        let j = k as isize - di;
        let a = tmp[(j).clamp(0, n - 1) as usize];
        let b = tmp[(j - 1).clamp(0, n - 1) as usize];
        *v = a + (b - a) * f;
    }
}
