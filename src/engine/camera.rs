//! The camera filming the TV screen: lens blur, placement (pan, tilt, zoom, roll), lateral
//! chromatic aberration, colour gain, auto exposure and grain. It films at half resolution.

use super::signal::*;
use crate::params::{Params, CAMERAS};
use rayon::prelude::*;

const CW: usize = W / 2;
const CH: usize = H / 2;
/// Lateral chromatic aberration.
const CA: f32 = 0.015;

pub struct Camera {
    agc: f32,
    blurred: Vec<f32>,
    tmp: Vec<f32>,
    cam: Vec<f32>,
    frame: u32,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            agc: 1.0,
            blurred: vec![0.0; W * H * 3],
            tmp: vec![0.0; W * H * 3],
            cam: vec![0.0; CW * CH * 3],
            frame: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Films `screen` (RGB, 768 × 480) and mixes the camera's picture into `src` in place.
    pub fn film_and_mix(&mut self, screen: &[f32], src: &mut [f32], p: &Params) {
        self.frame = self.frame.wrapping_add(1);
        let radius = (40.0 * p.blur * p.blur).round() as usize;
        if radius > 0 {
            box_blur(screen, &mut self.tmp, &mut self.blurred, radius);
        } else {
            self.blurred.copy_from_slice(screen);
        }

        let [pan, tilt, zoom_k, roll_k] = CAMERAS[p.camera as usize].1;
        let zoom = 0.965f32.powf(-zoom_k);
        let roll = (2.5 * roll_k).to_radians();
        let (sr, cr) = roll.sin_cos();
        let pan_px = -12.0 * pan;
        let tilt_px = 12.0 * tilt;
        let sat = 10f32.powf(12.0 * p.color / 20.0);
        let manual = if p.noise > 0.0 { 10f32.powf(36.0 * p.noise / 20.0) } else { 0.0 };
        let agc_max = 10f32.powf((36.0 - 36.0 * p.noise) / 20.0 / 2.2).min(2.54);
        let bright = self.agc * manual.max(1.0).powf(0.25);
        let grain = 2.449 * manual / 255.0;
        let a_ch = 1.0 - (-0.8607f32).exp();
        let renorm = ((2.0 - a_ch) / a_ch).sqrt();
        let frame = self.frame;
        let img = &self.blurred;

        let lum: f32 = self
            .cam
            .par_chunks_mut(CW * 3)
            .enumerate()
            .map(|(y, row)| {
                let mut rng = Rng(hash(frame, y as u32));
                let (mut ni, mut nq) = (0.0f32, 0.0f32);
                let mut lum = 0.0f32;
                let py = (y as f32 + 0.5) * 2.0 - CH as f32;
                for x in 0..CW {
                    let px = (x as f32 + 0.5) * 2.0 - CW as f32;
                    let rx = (cr * px - sr * py) / zoom;
                    let ry = (sr * px + cr * py) / zoom;
                    let cx = 383.5 + pan_px;
                    let cy = 239.5 + tilt_px;
                    let g = sample(img, cx + rx, cy + ry, 1);
                    let r = sample(img, cx + rx / (1.0 + CA), cy + ry / (1.0 + CA), 0);
                    let b = sample(img, cx + rx / (1.0 - CA), cy + ry / (1.0 - CA), 2);
                    // Colour gain in YIQ.
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let ii = (0.596 * r - 0.274 * g - 0.322 * b) * sat;
                    let qq = (0.211 * r - 0.523 * g + 0.312 * b) * sat;
                    let mut rgb = [
                        (yy + 0.956 * ii + 0.621 * qq).clamp(0.0, 1.0) * bright,
                        (yy - 0.272 * ii - 0.647 * qq).clamp(0.0, 1.0) * bright,
                        (yy - 1.106 * ii + 1.703 * qq).clamp(0.0, 1.0) * bright,
                    ];
                    for c in rgb.iter_mut() {
                        *c = c.min(1.0);
                    }
                    lum += 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
                    if grain > 0.0 {
                        let (n0, n1, n2) = (rng.tri(), rng.tri(), rng.tri());
                        let ny = 0.299 * n0 + 0.587 * n1 + 0.114 * n2;
                        let ci = 0.596 * n0 - 0.274 * n1 - 0.322 * n2;
                        let cq = 0.211 * n0 - 0.523 * n1 + 0.312 * n2;
                        ni += (ci - ni) * a_ch;
                        nq += (cq - nq) * a_ch;
                        let (fi, fq) = (ni * renorm, nq * renorm);
                        rgb[0] += (ny + 0.956 * fi + 0.621 * fq) * grain;
                        rgb[1] += (ny - 0.272 * fi - 0.647 * fq) * grain;
                        rgb[2] += (ny - 1.106 * fi + 1.703 * fq) * grain;
                    }
                    row[3 * x] = rgb[0].clamp(0.0, 1.0);
                    row[3 * x + 1] = rgb[1].clamp(0.0, 1.0);
                    row[3 * x + 2] = rgb[2].clamp(0.0, 1.0);
                }
                lum
            })
            .sum();

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

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9e37_79b1) ^ b.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h | 1
}

/// Bilinear sample of one channel; black outside the screen.
#[inline]
fn sample(img: &[f32], x: f32, y: f32, c: usize) -> f32 {
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
    a * (1.0 - ty) + b * ty
}

/// Separable box blur of an RGB image.
fn box_blur(src: &[f32], tmp: &mut [f32], dst: &mut [f32], r: usize) {
    let n = (2 * r + 1) as f32;
    tmp.par_chunks_mut(W * 3).enumerate().for_each(|(y, row)| {
        let s = &src[y * W * 3..(y + 1) * W * 3];
        for c in 0..3 {
            let at = |x: isize| s[(x.clamp(0, W as isize - 1) as usize) * 3 + c];
            let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
            for x in 0..W {
                row[x * 3 + c] = acc / n;
                acc += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
            }
        }
    });
    // Vertical: a running sum per column, columns split across threads in strips.
    const STRIP: usize = 64 * 3;
    let row_of = |y: isize| y.clamp(0, H as isize - 1) as usize * W * 3;
    let strips: Vec<(usize, Vec<f32>)> = (0..W * 3)
        .step_by(STRIP)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|c0| {
            let c1 = (c0 + STRIP).min(W * 3);
            let w = c1 - c0;
            let mut out = vec![0.0f32; w * H];
            let mut acc = vec![0.0f32; w];
            for k in -(r as isize)..=r as isize {
                let o = row_of(k);
                for (a, v) in acc.iter_mut().zip(&tmp[o + c0..o + c1]) {
                    *a += v;
                }
            }
            for y in 0..H {
                for (d, a) in out[y * w..(y + 1) * w].iter_mut().zip(&acc) {
                    *d = a / n;
                }
                let (add, sub) = (row_of(y as isize + r as isize + 1), row_of(y as isize - r as isize));
                for i in 0..w {
                    acc[i] += tmp[add + c0 + i] - tmp[sub + c0 + i];
                }
            }
            (c0, out)
        })
        .collect();
    for (c0, out) in strips {
        let w = out.len() / H;
        for y in 0..H {
            dst[y * W * 3 + c0..y * W * 3 + c0 + w].copy_from_slice(&out[y * w..(y + 1) * w]);
        }
    }
}
