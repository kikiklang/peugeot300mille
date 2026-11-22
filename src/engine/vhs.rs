//! A worn VHS deck between the box and the TV, working on the composite signal line by line:
//!
//! * wear: luma loses bandwidth, colour is carried low ("colour under") so it smears and lags,
//!   hue wobbles from line to line, FM noise streaks,
//! * tracking: a band of noise that drifts up the picture; inside it, sync is lost too,
//! * dropouts: oxide missing on the tape gives white streaks, or the deck repeats the line above,
//! * instability: lines wobble (timebase error), the top of each field bends (flagging) and the
//!   head switch shows a few lines above the bottom.

use super::signal::*;
use crate::params::Params;

pub struct Vhs {
    prev: Vec<f32>,
    line: Vec<f32>,
    y: Vec<f32>,
    i: Vec<f32>,
    q: Vec<f32>,
    jitter: f32,
    hue: f32,
    band: f32,
    frame: u32,
}

impl Vhs {
    pub fn new() -> Self {
        Self {
            prev: vec![BLANK; SPL],
            line: vec![0.0; SPL],
            y: vec![0.0; SPL],
            i: vec![0.0; SPL],
            q: vec![0.0; SPL],
            jitter: 0.0,
            hue: 0.0,
            band: 0.3,
            frame: 0,
        }
    }

    pub fn active(p: &Params) -> bool {
        p.vhs_wear > 0.0 || p.vhs_tracking > 0.0 || p.vhs_dropouts > 0.0 || p.vhs_jitter > 0.0
    }

    /// `abs0` is the absolute index of the frame's first sample (keeps the subcarrier phase).
    pub fn process(&mut self, sig: &mut [f32], abs0: i64, p: &Params) {
        self.frame = self.frame.wrapping_add(1);
        let wear = p.vhs_wear;
        let trk = p.vhs_tracking;
        let drop = p.vhs_dropouts;
        let inst = p.vhs_jitter;
        let mut rng = Rng(0x2545_f491 ^ self.frame.wrapping_mul(0x9e37_79b9) | 1);

        let a_y = rc(3.4e6 * (1.8 / 3.4f64).powf(wear as f64)); // luma bandwidth
        let a_c = rc(1.0e6 * (0.3 / 1.0f64).powf(wear as f64)); // colour-under bandwidth
        let y_noise = 0.004 + 0.05 * wear;
        // Tracking band drifts slowly; its height grows with the error.
        self.band = (self.band + 0.004 + 0.01 * trk).fract();
        let band_h = 5.0 + 32.0 * trk;
        let band_c = self.band * LINES as f32;

        for l in 0..LINES {
            let a = l * SPL;
            let src = &sig[a..a + SPL];
            let phase = ((abs0 + a as i64) & 3) as usize;
            let field = (a >= FIELD) as usize;
            let in_field = (a as f32 - (field * FIELD) as f32) / SPL as f32; // lines since field start

            // --- Y / C split and colour under.
            for k in 0..SPL {
                let s = |j: isize| src[(k as isize + j).clamp(0, SPL as isize - 1) as usize];
                let y = 0.25 * s(-2) + 0.5 * s(0) + 0.25 * s(2);
                let c = src[k] - y;
                let ph = (phase + k) & 3;
                self.y[k] = y;
                self.i[k] = 2.0 * c * COS4[ph];
                self.q[k] = 2.0 * c * SIN4[ph];
            }
            // Luma: band-limited both ways, plus streaky FM noise.
            let mut s = self.y[0];
            for v in self.y.iter_mut() {
                s += (*v - s) * a_y;
                *v = s;
            }
            let mut s = self.y[SPL - 1];
            let mut n = 0.0f32;
            for v in self.y.iter_mut().rev() {
                s += (*v - s) * a_y;
                n += (rng.tri() - n) * 0.35;
                *v = s + n * y_noise;
            }
            // Colour: one-way low-pass, so it smears and lags behind the picture.
            self.hue += (rng.tri() * 0.25 - self.hue) * 0.5;
            let (hs, hc) = (self.hue * wear).sin_cos();
            let (mut si, mut sq) = (0.0f32, 0.0f32);
            for k in 0..SPL {
                si += (self.i[k] - si) * a_c;
                sq += (self.q[k] - sq) * a_c;
                let (ii, qq) = (si * hc - sq * hs, si * hs + sq * hc);
                let ph = (phase + k) & 3;
                self.line[k] = self.y[k] + (ii * COS4[ph] + qq * SIN4[ph]) * (1.0 + rng.tri() * 0.15 * wear);
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

            // --- Tracking noise band (wraps round the frame).
            let mut d = (l as f32 - band_c).abs();
            d = d.min(LINES as f32 - d);
            if trk > 0.0 && d < band_h {
                let strength = 0.8 * trk.powf(1.8) * (1.0 - d / band_h);
                let mut k = 0;
                while k < SPL {
                    let run = 4 + (rng.unit() * 60.0) as usize;
                    if rng.unit() < strength {
                        let lvl = BLACK + 0.2 + rng.unit() * 0.6;
                        for v in self.line[k..(k + run).min(SPL)].iter_mut() {
                            *v = lvl + rng.tri() * 0.15;
                        }
                    }
                    k += run;
                }
            }
            if near_switch && trk > 0.3 {
                for v in self.line[..SPL / 3].iter_mut() {
                    *v = BLACK + 0.3 + rng.tri() * 0.3 * trk;
                }
            }

            // --- Dropouts: a white streak, or the line above repeated by the compensator.
            if drop > 0.0 && rng.unit() < drop * 0.12 {
                let start = ACT0 + (rng.unit() * (W as f32 - 20.0)) as usize;
                let len = 10 + (rng.unit() * rng.unit() * 360.0 * (0.3 + drop)) as usize;
                let end = (start + len).min(ACT0 + W);
                if rng.unit() < 0.55 {
                    for v in self.line[start..end].iter_mut() {
                        *v = 0.85 + rng.tri() * 0.12;
                    }
                } else {
                    let (dst, src) = (&mut self.line[start..end], &self.prev[start..end]);
                    dst.copy_from_slice(src);
                }
            }

            sig[a..a + SPL].copy_from_slice(&self.line);
            self.prev.copy_from_slice(&self.line);
        }
    }
}

impl Default for Vhs {
    fn default() -> Self {
        Self::new()
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
