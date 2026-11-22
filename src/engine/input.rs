//! The input processor, before the circuit: exposure (brightness, contrast, gamma), an optional
//! 3D LUT, then saturation and hue. All at 0 (and no LUT) = untouched.

use super::lut::Lut;
use super::signal::{rgb_of, yiq_of};
use crate::params::Params;
use rayon::prelude::*;

pub fn is_neutral(p: &Params, lut: Option<&Lut>) -> bool {
    p.brightness == 0.0 && p.contrast == 0.0 && p.gamma == 0.0 && p.saturation == 0.0 && p.hue == 0.0
        && (lut.is_none() || p.lut_in_amount == 0.0)
}

/// Same look applied to the output picture: LUT only.
pub fn apply_lut(rgb: &mut [f32], lut: &Lut, amount: f32) {
    rgb.par_chunks_mut(3 * 64).for_each(|px| {
        for c in px.chunks_exact_mut(3) {
            let o = lut.apply([c[0], c[1], c[2]]);
            for k in 0..3 {
                c[k] += (o[k] - c[k]) * amount;
            }
        }
    });
}

/// An 8-bit level as the engine reads it.
#[inline]
pub fn level(v: usize) -> f32 {
    v as f32 * (1.0 / 255.0)
}

/// `rgb` holds 0..1 values, 3 per pixel.
pub fn process(rgb: &mut [f32], p: &Params, lut: Option<&Lut>) {
    if is_neutral(p, lut) {
        return;
    }
    let gamma = 2f32.powf(-p.gamma); // gamma +1 → brighter mids
    let contrast = 1.0 + p.contrast;
    let bright = 0.4 * p.brightness;
    let sat = 1.0 + p.saturation;
    let (hs, hc) = p.hue.to_radians().sin_cos();
    let lut_amt = p.lut_in_amount.clamp(0.0, 1.0);
    // The source is 8-bit, so `powf` only ever sees 256 values.
    let curve: [f32; 256] = std::array::from_fn(|i| level(i).clamp(0.0, 1.0).powf(gamma));
    let apply_gamma = |x: f32| {
        let i = (x * 255.0 + 0.5) as usize & 255;
        // Exact match only: anything else (a value not from 8 bits) takes the real curve.
        if level(i).clamp(0.0, 1.0).to_bits() == x.to_bits() { curve[i] } else { x.powf(gamma) }
    };
    rgb.par_chunks_mut(3 * 64).for_each(|px| {
        for c in px.chunks_exact_mut(3) {
            let mut v = [0.0f32; 3];
            for k in 0..3 {
                let x = apply_gamma(c[k].clamp(0.0, 1.0));
                v[k] = ((x - 0.5) * contrast + 0.5 + bright).clamp(0.0, 1.0);
            }
            if let Some(l) = lut {
                if lut_amt > 0.0 {
                    let o = l.apply(v);
                    for k in 0..3 {
                        v[k] += (o[k] - v[k]) * lut_amt;
                    }
                }
            }
            if sat != 1.0 || p.hue != 0.0 {
                let [y, i0, q0] = yiq_of(v);
                let i = (i0 * hc - q0 * hs) * sat;
                let q = (i0 * hs + q0 * hc) * sat;
                v = rgb_of(y, i, q);
            }
            for k in 0..3 {
                c[k] = v[k].clamp(0.0, 1.0);
            }
        }
    });
}
