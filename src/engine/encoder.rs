//! RGB → YIQ → band-limited → composite NTSC.

use super::signal::*;
use rayon::prelude::*;

/// RGB planes (0..1) → Y, I, Q planes, band-limited like a studio encoder.
pub fn rgb_to_yiq(rgb: &[f32], y: &mut [f32], i: &mut [f32], q: &mut [f32]) {
    y.par_chunks_mut(W)
        .zip(i.par_chunks_mut(W))
        .zip(q.par_chunks_mut(W))
        .enumerate()
        .for_each(|(row, ((yr, ir), qr))| {
            let src = &rgb[row * W * 3..(row + 1) * W * 3];
            for x in 0..W {
                let (r, g, b) = (src[3 * x], src[3 * x + 1], src[3 * x + 2]);
                yr[x] = 0.299 * r + 0.587 * g + 0.114 * b;
                ir[x] = 0.5959 * r - 0.2746 * g - 0.3213 * b;
                qr[x] = 0.2115 * r - 0.5227 * g + 0.3112 * b;
            }
            zero_phase_lowpass(yr, 0.6483, 1);
            zero_phase_lowpass(ir, 0.3632, 2);
            zero_phase_lowpass(qr, 0.2084, 2);
        });
}

/// One-pole low-pass run forward then backward along a row (no phase shift).
pub fn zero_phase_lowpass(row: &mut [f32], a: f32, passes: usize) {
    for _ in 0..passes {
        let mut s = row[0];
        for v in row.iter_mut() {
            s += (*v - s) * a;
            *v = s;
        }
        let mut s = *row.last().unwrap();
        for v in row.iter_mut().rev() {
            s += (*v - s) * a;
            *v = s;
        }
    }
}

/// Writes one composite frame. `phase0` is the absolute sample index of the frame's first sample,
/// modulo 4, so the subcarrier stays continuous across frames.
pub fn encode(tpl: &Template, y: &[f32], i: &[f32], q: &[f32], out: &mut [f32], phase0: usize) {
    out.copy_from_slice(&tpl.base);
    for &(a, env) in &tpl.burst {
        let a = a as usize;
        out[a] += BURST_AMP * env * -COS4[(a + phase0) & 3];
    }
    out.par_chunks_mut(SPL).enumerate().for_each(|(l, line)| {
        let row = tpl.line_row[l];
        if row < 0 {
            return;
        }
        let p0 = row as usize * W;
        let a0 = l * SPL + ACT0;
        for k in 0..W {
            let ph = (a0 + k + phase0) & 3;
            let p = p0 + k;
            line[ACT0 + k] = BLACK + YGAIN * y[p] + CGAIN * (i[p] * COS4[ph] + q[p] * SIN4[ph]);
        }
    });
}
