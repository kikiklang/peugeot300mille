//! The camera filming the TV screen.
//!
//! Per pass round the loop: lens blur, mirrors, placement (position + continuous pan / tilt /
//! zoom / rotation, drift, shake), fisheye, chromatic aberration, the screen's lines and grille
//! seen by the camera (moiré), vignetting, colour gain and hue turn, negative, auto exposure,
//! grain, and the tube's lag, bloom and burn-in. It films at half resolution. Every addition is
//! off at 0.

use super::signal::*;
use crate::params::{Params, CAMERAS};
use rayon::prelude::*;

const CW: usize = W / 2;
const CH: usize = H / 2;
/// The original lateral chromatic aberration; `lens_ca` adds to it.
const CA: f32 = 0.015;
/// Columns (×3 channels) per task in the vertical pass of the blur.
const STRIP: usize = 64 * 3;

pub struct Camera {
    agc: f32,
    blurred: Vec<f32>,
    tmp: Vec<f32>,
    cam: Vec<f32>,
    prev: Vec<f32>,
    burn: Vec<f32>,
    bloom_a: Vec<f32>,
    bloom_b: Vec<f32>,
    /// Working memory of the blur's vertical pass.
    strips: Vec<Vec<f32>>,
    /// Where each camera pixel looks through a kaleidoscope, and for which mirror (0 = not built).
    fold_map: Vec<(f32, f32)>,
    fold_mirror: u8,
    frame: u32,
    /// Drift: current offset and where it is heading (pan px, tilt px, zoom, rotation °).
    drift: [f32; 4],
    drift_to: [f32; 4],
    rng: Rng,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            agc: 1.0,
            blurred: vec![0.0; W * H * 3],
            tmp: vec![0.0; W * H * 3],
            cam: vec![0.0; CW * CH * 3],
            prev: vec![0.0; CW * CH * 3],
            burn: vec![0.0; CW * CH * 3],
            bloom_a: vec![0.0; CW * CH * 3],
            bloom_b: vec![0.0; CW * CH * 3],
            strips: Vec::new(),
            fold_map: Vec::new(),
            fold_mirror: 0,
            frame: 0,
            drift: [0.0; 4],
            drift_to: [0.0; 4],
            rng: Rng(0x5eed_1234),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Films `screen` (RGB, 768 × 480) and mixes the camera's picture into `src` in place.
    pub fn film_and_mix(&mut self, screen: &[f32], src: &mut [f32], p: &Params) {
        self.frame = self.frame.wrapping_add(1);
        let radius = (40.0 * p.blur * p.blur).round() as usize;
        let img: &[f32] = if radius > 0 {
            box_blur(screen, &mut self.tmp, &mut self.blurred, &mut self.strips, radius, W, H);
            &self.blurred
        } else {
            screen
        };

        // --- Where the camera points.
        let [pan, tilt, zoom_k, roll_k] = CAMERAS[p.camera as usize].1;
        // Drift: a slow wander towards a new target every couple of seconds.
        if p.cam_drift > 0.0 {
            if self.frame % 60 == 1 {
                let a = p.cam_drift;
                self.drift_to = [
                    self.rng.tri() * 40.0 * a,
                    self.rng.tri() * 30.0 * a,
                    self.rng.tri() * 0.06 * a,
                    self.rng.tri() * 4.0 * a,
                ];
            }
            for k in 0..4 {
                self.drift[k] += (self.drift_to[k] - self.drift[k]) * 0.03;
            }
        } else {
            self.drift = [0.0; 4];
        }
        let shake = if p.cam_shake > 0.0 {
            let a = p.cam_shake;
            [self.rng.tri() * 14.0 * a, self.rng.tri() * 10.0 * a, self.rng.tri() * 0.02 * a, self.rng.tri() * 1.5 * a]
        } else {
            [0.0; 4]
        };
        let zoom = 0.965f32.powf(-zoom_k) * 1.18f32.powf(p.cam_zoom) * (1.0 + self.drift[2] + shake[2]);
        let roll = (2.5 * roll_k + 12.0 * p.cam_rotate + self.drift[3] + shake[3]).to_radians();
        let (sr, cr) = roll.sin_cos();
        let pan_px = -12.0 * pan - 60.0 * p.cam_pan + self.drift[0] + shake[0];
        let tilt_px = 12.0 * tilt + 45.0 * p.cam_tilt + self.drift[1] + shake[1];
        let fish = 0.55 * p.lens_fisheye;
        let ca = CA + 0.08 * p.lens_ca;
        let moire = p.lens_moire;
        let vign = p.lens_vignette;
        let mirror = p.mirror;
        // A kaleidoscope costs an atan2, a sine and a cosine per pixel, but only depends on the
        // pixel: worked out once for as long as the mirror stays.
        if mirror >= 4 && self.fold_mirror != mirror {
            self.fold_map.clear();
            self.fold_map.par_extend((0..CW * CH).into_par_iter().map(|i| {
                let (px, py) = centred(i % CW, i / CW);
                fold(px, py, mirror)
            }));
            self.fold_mirror = mirror;
        }
        let fold_map = &self.fold_map;

        // --- Colour.
        let sat = 10f32.powf(12.0 * p.color / 20.0);
        let (hs, hc) = (45.0 * p.cam_hue).to_radians().sin_cos();
        let neg = p.cam_negative;
        let manual = if p.noise > 0.0 { 10f32.powf(36.0 * p.noise / 20.0) } else { 0.0 };
        let agc_max = 10f32.powf((36.0 - 36.0 * p.noise) / 20.0 / 2.2).min(2.54);
        let bright = self.agc * manual.max(1.0).powf(0.25);
        let grain = 2.449 * manual / 255.0;
        let a_ch = 1.0 - (-0.8607f32).exp();
        let renorm = ((2.0 - a_ch) / a_ch).sqrt();
        let frame = self.frame;
        let half_diag2 = (CW * CW + CH * CH) as f32;

        let lum: f32 = self.cam.par_chunks_mut(CW * 3).enumerate().map(|(y, row)| {
            let mut rng = Rng(hash(frame, y as u32));
            let (mut ni, mut nq) = (0.0f32, 0.0f32);
            let mut lum = 0.0f32;
            for x in 0..CW {
                let (mut px, mut py) = centred(x, y);
                let vig = if vign > 0.0 { 1.0 - vign * 0.85 * ((px * px + py * py) / (4.0 * half_diag2)).min(1.0) } else { 1.0 };
                (px, py) = if mirror >= 4 { fold_map[y * CW + x] } else { fold(px, py, mirror) };
                if fish != 0.0 {
                    let r2 = (px * px + py * py) / (CW * CW) as f32;
                    let k = 1.0 + fish * r2;
                    px *= k;
                    py *= k;
                }
                let rx = (cr * px - sr * py) / zoom;
                let ry = (sr * px + cr * py) / zoom;
                let cx = 383.5 + pan_px;
                let cy = 239.5 + tilt_px;
                let g = sample(img, cx + rx, cy + ry, 1, moire);
                let r = sample(img, cx + rx / (1.0 + ca), cy + ry / (1.0 + ca), 0, moire);
                let b = sample(img, cx + rx / (1.0 - ca), cy + ry / (1.0 - ca), 2, moire);
                // Colour gain and hue turn in YIQ.
                let [mut yy, i0, q0] = yiq_of([r, g, b]);
                // Negative: the brightness is inverted, the colours are kept.
                if neg > 0.0 {
                    yy += neg * (1.0 - 2.0 * yy);
                }
                let ii = (i0 * hc - q0 * hs) * sat;
                let qq = (i0 * hs + q0 * hc) * sat;
                let mut rgb = rgb_of(yy, ii, qq).map(|c| (c.clamp(0.0, 1.0) * vig * bright).min(1.0));
                // Exposure is measured before the grain.
                lum += 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
                if grain > 0.0 {
                    let (n0, n1, n2) = (rng.tri(), rng.tri(), rng.tri());
                    let [ny, ci, cq] = yiq_of([n0, n1, n2]);
                    ni += (ci - ni) * a_ch;
                    nq += (cq - nq) * a_ch;
                    let (fi, fq) = (ni * renorm, nq * renorm);
                    let n = rgb_of(ny, fi, fq);
                    for (c, n) in rgb.iter_mut().zip(n) {
                        *c += n * grain;
                    }
                }
                row[3 * x] = rgb[0].clamp(0.0, 1.0);
                row[3 * x + 1] = rgb[1].clamp(0.0, 1.0);
                row[3 * x + 2] = rgb[2].clamp(0.0, 1.0);
            }
            lum
        }).sum();

        // --- The tube: lag (bright things fade slowly), bloom, burn-in.
        if p.tube_lag > 0.0 {
            let k = 0.85 * p.tube_lag;
            self.cam.par_chunks_mut(CW * 3).zip(self.prev.par_chunks(CW * 3)).for_each(|(cs, ps)| {
                for (c, pv) in cs.iter_mut().zip(ps) {
                    *c += (pv - *c).max(0.0) * k;
                }
            });
        }
        if p.tube_burn > 0.0 {
            let k = 0.35 * p.tube_burn;
            self.cam.par_chunks_mut(CW * 3).zip(self.burn.par_chunks_mut(CW * 3)).for_each(|(cs, bs)| {
                for (c, b) in cs.iter_mut().zip(bs) {
                    *b += (*c - *b) * 0.004;
                    *c = *c * (1.0 - k) + (*c).max(*b * 1.6) * k;
                }
            });
        }
        if p.tube_bloom > 0.0 {
            self.bloom_a.par_chunks_mut(CW * 3).zip(self.cam.par_chunks(CW * 3)).for_each(|(ds, cs)| {
                for (d, c) in ds.iter_mut().zip(cs) {
                    *d = (c - 0.72).max(0.0) * 3.0;
                }
            });
            let small = CW * CH * 3;
            box_blur(&self.bloom_a, &mut self.bloom_b, &mut self.tmp[..small], &mut self.strips, 7, CW, CH);
            box_blur(&self.tmp[..small], &mut self.bloom_a, &mut self.bloom_b, &mut self.strips, 7, CW, CH);
            // Screen-blend the halo so it never piles light on light.
            let k = 0.9 * p.tube_bloom;
            self.cam.par_chunks_mut(CW * 3).zip(self.bloom_b.par_chunks(CW * 3)).for_each(|(cs, bs)| {
                for (c, b) in cs.iter_mut().zip(bs) {
                    let h = (b * k).min(1.0);
                    *c = 1.0 - (1.0 - *c) * (1.0 - h);
                }
            });
        }
        self.prev.par_chunks_mut(CW * 3).zip(self.cam.par_chunks(CW * 3)).for_each(|(d, s)| d.copy_from_slice(s));

        // Auto exposure: aims for a mean luma of 0.45.
        let mean = (lum / (CW * CH) as f32).max(0.001);
        self.agc = (self.agc * (0.45 / mean).powf(1.0 / 15.0)).clamp(1.0, agc_max.max(1.0));

        // Back up to full size and mix into the source.
        let m = 0.8 * p.mix;
        let cam = &self.cam;
        src.par_chunks_mut(W * 3).enumerate().for_each(|(y, row)| {
            let fy = ((y as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, (CH - 1) as f32);
            let y0 = fy as usize;
            let y1 = (y0 + 1).min(CH - 1);
            let ty = fy - y0 as f32;
            for x in 0..W {
                let fx = ((x as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, (CW - 1) as f32);
                let x0 = fx as usize;
                let x1 = (x0 + 1).min(CW - 1);
                let tx = fx - x0 as f32;
                for c in 0..3 {
                    let a = cam[(y0 * CW + x0) * 3 + c] * (1.0 - tx) + cam[(y0 * CW + x1) * 3 + c] * tx;
                    let b = cam[(y1 * CW + x0) * 3 + c] * (1.0 - tx) + cam[(y1 * CW + x1) * 3 + c] * tx;
                    let v = a * (1.0 - ty) + b * ty;
                    let s = &mut row[3 * x + c];
                    *s = v * m + *s * (1.0 - m);
                }
            }
        });
    }
}

/// A camera pixel's position relative to the centre of the picture, in screen pixels.
#[inline]
fn centred(x: usize, y: usize) -> (f32, f32) {
    ((x as f32 + 0.5) * 2.0 - CW as f32, (y as f32 + 0.5) * 2.0 - CH as f32)
}

/// Mirrors in front of the lens, in camera-centred coordinates.
#[inline]
fn fold(px: f32, py: f32, mirror: u8) -> (f32, f32) {
    match mirror {
        1 => (-px.abs(), py),
        2 => (px, -py.abs()),
        3 => (-px.abs(), -py.abs()),
        4 | 5 => {
            let n = if mirror == 4 { 6.0 } else { 8.0 };
            let seg = std::f32::consts::TAU / n;
            let r = (px * px + py * py).sqrt();
            let mut a = py.atan2(px).rem_euclid(seg);
            if a > seg / 2.0 {
                a = seg - a;
            }
            (r * a.cos(), r * a.sin())
        }
        _ => (px, py),
    }
}

fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9e37_79b1) ^ b.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h | 1
}

/// Bilinear sample of one channel; black outside the screen. With `moire` > 0 the camera also
/// sees the screen's scan lines and its RGB grille, which beat against its own pixels.
#[inline]
fn sample(img: &[f32], x: f32, y: f32, c: usize, moire: f32) -> f32 {
    if x < 0.0 || y < 0.0 || x >= (W - 1) as f32 || y >= (H - 1) as f32 {
        return 0.0;
    }
    let x0 = x as usize;
    let y0 = y as usize;
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let i = (y0 * W + x0) * 3 + c;
    let a = img[i] * (1.0 - tx) + img[i + 3] * tx;
    let b = img[i + W * 3] * (1.0 - tx) + img[i + W * 3 + 3] * tx;
    let v = a * (1.0 - ty) + b * ty;
    if moire > 0.0 {
        // Scan lines: dark gaps between lines; grille: each column lets one colour through.
        let line = 0.5 + 0.5 * (std::f32::consts::TAU * y).cos();
        let col = (x * (1.0 / 1.5)).rem_euclid(3.0) as usize;
        let grille = if col == c { 1.0 } else { 0.35 };
        let w = (1.0 - moire * 0.7 * (1.0 - line)) * (1.0 - moire * (1.0 - grille));
        // Keep the average brightness.
        v * w / (1.0 - moire * 0.35) / (1.0 - moire * 0.433)
    } else {
        v
    }
}

/// Separable box blur of an RGB image. `strips` is working memory kept between calls.
fn box_blur(src: &[f32], tmp: &mut [f32], dst: &mut [f32], strips: &mut Vec<Vec<f32>>, r: usize, bw: usize, bh: usize) {
    let n = (2 * r + 1) as f32;
    tmp.par_chunks_mut(bw * 3).enumerate().for_each(|(y, row)| {
        let s = &src[y * bw * 3..(y + 1) * bw * 3];
        for c in 0..3 {
            let at = |x: isize| s[(x.clamp(0, bw as isize - 1) as usize) * 3 + c];
            let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
            for x in 0..bw {
                row[x * 3 + c] = acc / n;
                acc += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
            }
        }
    });
    // Vertical: a running sum per column, columns split across threads in strips.
    let tmp = &*tmp;
    let row_of = |y: isize| y.clamp(0, bh as isize - 1) as usize * bw * 3;
    let n_strips = (bw * 3).div_ceil(STRIP);
    if strips.len() < n_strips {
        strips.resize_with(n_strips, Vec::new);
    }
    let strips = &mut strips[..n_strips];
    strips.par_iter_mut().enumerate().for_each(|(s, strip)| {
        let c0 = s * STRIP;
        let w = (c0 + STRIP).min(bw * 3) - c0;
        strip.resize(w * bh + w, 0.0);
        let (out, acc) = strip.split_at_mut(w * bh);
        acc.fill(0.0);
        for k in -(r as isize)..=r as isize {
            let o = row_of(k) + c0;
            for (a, v) in acc.iter_mut().zip(&tmp[o..o + w]) {
                *a += v;
            }
        }
        for (y, line) in out.chunks_exact_mut(w).enumerate() {
            for (d, a) in line.iter_mut().zip(acc.iter()) {
                *d = a / n;
            }
            let add = row_of(y as isize + r as isize + 1) + c0;
            let sub = row_of(y as isize - r as isize) + c0;
            for ((a, p), m) in acc.iter_mut().zip(&tmp[add..add + w]).zip(&tmp[sub..sub + w]) {
                *a += p - m;
            }
        }
    });
    let strips = &*strips;
    dst.par_chunks_mut(bw * 3).enumerate().for_each(|(y, row)| {
        for (line, strip) in row.chunks_mut(STRIP).zip(strips) {
            let w = line.len();
            line.copy_from_slice(&strip[y * w..(y + 1) * w]);
        }
    });
}
