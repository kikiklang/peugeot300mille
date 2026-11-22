//! The television: it has to find the picture in the signal, the way a real set does.
//!
//! * adaptive sync separator (tracks the sync tip and the back porch, slices at the middle with
//!   hysteresis, classifies pulses by width),
//! * horizontal flywheel PLL (proportional-integral, limited hold range),
//! * vertical integrator with a hold window and a free-running vertical oscillator (so a picture
//!   without vertical sync rolls),
//! * colour: burst demodulation, a colour PLL with limited pull-in, ACC and a colour killer,
//! * each line is resampled at its sub-sample position (Catmull-Rom) into the 768 × 480 raster.

use super::signal::*;

/// Horizontal PLL gains (per line).
const KP: f64 = 0.30;
const KI: f64 = 0.02;
/// Horizontal hold range around the free-running period (samples).
const H_HOLD: f64 = 14.0;
/// Free-running horizontal period detune.
const H_DETUNE: f64 = 0.0;
/// Vertical integrator rate, and the vertical oscillator's free-running field length.
const KV: f32 = 1.0 / 400.0;
const V_FREE: f64 = FIELD as f64 * 1.03;
/// Vertical hold: a vertical sync is only accepted this far into a field.
const V_HOLD: f64 = FIELD as f64 * 0.85;
/// Colour PLL gain and pull-in limit per line (rad).
const KC: f64 = 0.25;
const C_PULL: f64 = 0.1198;

pub struct Tv {
    buf: Vec<f32>,
    /// Absolute sample index of buf[0].
    base: i64,
    /// Next absolute sample to slice.
    scan: i64,
    tip: f32,
    porch: f32,
    in_sync: bool,
    pulse_start: i64,
    hedges: Vec<i64>,
    vint: f32,
    v_armed: bool,
    vevents: Vec<i64>,
    line_start: f64,
    period: f64,
    pub h_lock: f32,
    t_v: f64,
    r0: f64,
    cphase: f64,
    bamp: f32,
    acc: f32,
    clock: f32,
    color_on: bool,
    pub raster: Vec<f32>,
    // per-line scratch
    yr: Vec<f32>,
    ir: Vec<f32>,
    qr: Vec<f32>,
}

impl Tv {
    pub fn new(tpl: &Template) -> Self {
        let mut tv = Self::blank(0.0);
        tv.r0 = calibrate(tpl);
        tv
    }

    fn blank(r0: f64) -> Self {
        Self {
            buf: Vec::with_capacity(FRAME * 2),
            base: 0,
            scan: 0,
            tip: 0.0,
            porch: BLANK,
            in_sync: false,
            pulse_start: 0,
            hedges: Vec::new(),
            vint: 0.0,
            v_armed: false,
            vevents: Vec::new(),
            line_start: 0.0,
            period: SPL as f64,
            h_lock: 0.0,
            t_v: -(FIELD as f64),
            r0,
            cphase: 0.0,
            bamp: 0.0,
            acc: 1.0,
            clock: 0.0,
            color_on: false,
            raster: vec![0.0; W * H * 3],
            yr: vec![0.0; W + 16],
            ir: vec![0.0; W + 16],
            qr: vec![0.0; W + 16],
        }
    }

    pub fn reset(&mut self) {
        let r0 = self.r0;
        *self = Self::blank(r0);
    }

    /// Feeds one frame of signal whose first sample has absolute index `abs0`.
    pub fn push(&mut self, frame: &[f32], abs0: i64) {
        if self.buf.is_empty() {
            self.base = abs0;
            self.scan = abs0;
            self.line_start = abs0 as f64;
        }
        debug_assert_eq!(self.base + self.buf.len() as i64, abs0);
        self.buf.extend_from_slice(frame);
        self.slice();
        self.lines();
        // Drop what no line will look at again.
        let keep_from = (self.line_start as i64 - 2 * SPL as i64).max(self.base);
        let drop = (keep_from - self.base) as usize;
        if drop > 0 {
            self.buf.drain(..drop);
            self.base += drop as i64;
        }
        let cut = self.base;
        self.hedges.retain(|&e| e >= cut);
    }

    #[inline]
    fn at(&self, a: i64) -> f32 {
        let i = a - self.base;
        if i < 0 || i as usize >= self.buf.len() {
            BLANK
        } else {
            self.buf[i as usize]
        }
    }

    fn slice(&mut self) {
        let end = self.base + self.buf.len() as i64;
        let mut a = self.scan;
        while a < end {
            let s = self.buf[(a - self.base) as usize];
            if s < self.tip {
                self.tip += (s - self.tip) * 0.02;
            } else {
                self.tip += (self.porch - self.tip) * 0.0002;
            }
            let span = (self.porch - self.tip).max(0.02);
            let mid = self.tip + 0.5 * span;
            let hyst = 0.05 * span;
            let now = if self.in_sync { s < mid + hyst } else { s < mid - hyst };
            if now && !self.in_sync {
                self.pulse_start = a;
            } else if !now && self.in_sync {
                let w = (a - self.pulse_start) as f64;
                if (33.5..=107.2).contains(&w) {
                    self.hedges.push(self.pulse_start);
                }
            }
            self.in_sync = now;
            self.vint += ((now as u8 as f32) - self.vint) * KV;
            if !self.v_armed && self.vint > 0.5 {
                self.v_armed = true;
                self.vevents.push(a);
            } else if self.v_armed && self.vint < 0.2 {
                self.v_armed = false;
            }
            a += 1;
        }
        self.scan = end;
    }

    fn edge_near(&self, t: f64) -> Option<f64> {
        let mut best: Option<(f64, f64)> = None;
        for &e in &self.hedges {
            let d = e as f64 - t;
            if d.abs() <= 227.5 && best.map_or(true, |(bd, _)| d.abs() < bd) {
                best = Some((d.abs(), e as f64));
            }
            if d > 227.5 {
                break;
            }
        }
        best.map(|(_, e)| e)
    }

    fn lines(&mut self) {
        let end = (self.base + self.buf.len() as i64) as f64;
        while self.line_start + 2.0 * SPL as f64 + 240.0 < end {
            let ls = self.line_start;
            // Vertical: accept a vertical sync once far enough into the field, else free-run.
            let mut i = 0;
            while i < self.vevents.len() {
                let ev = self.vevents[i] as f64;
                if ev > ls {
                    break;
                }
                if ev - self.t_v > V_HOLD {
                    self.t_v = ev;
                }
                i += 1;
            }
            self.vevents.drain(..i);
            while ls - self.t_v > V_FREE {
                self.t_v += V_FREE;
            }

            self.decode_line(ls);

            // Horizontal flywheel towards the next line's sync.
            let pred = ls + self.period;
            let free = SPL as f64 * (1.0 - H_DETUNE);
            match self.edge_near(pred + HS0 as f64) {
                Some(e) => {
                    let err = e - (pred + HS0 as f64);
                    let locked = err.abs() < 8.0;
                    self.h_lock += ((locked as u8 as f32) - self.h_lock) * 0.05;
                    self.line_start = pred + KP * err;
                    self.period = (self.period + KI * err).clamp(free - H_HOLD, free + H_HOLD);
                }
                None => {
                    self.h_lock -= self.h_lock * 0.05;
                    self.line_start = pred;
                    self.period += (free - self.period) * 0.002;
                }
            }
        }
    }

    fn decode_line(&mut self, ls: f64) {
        // Burst and back porch.
        let b0 = (ls + BURST0 as f64).round() as i64;
        let (mut bc, mut bs, mut sum) = (0.0f32, 0.0f32, 0.0f32);
        for a in b0..b0 + BURSTN as i64 {
            let s = self.at(a);
            let ph = (a & 3) as usize;
            bc += s * COS4[ph];
            bs += s * SIN4[ph];
            sum += s;
        }
        let mean = sum / BURSTN as f32;
        // The porch can only be measured where it is above the sync tip.
        if mean > self.tip + 0.05 {
            self.porch += (mean - self.porch) * 0.5;
        }
        let amp = 2.0 * (bc * bc + bs * bs).sqrt() / BURSTN as f32;
        let meas = (bs as f64).atan2(bc as f64);
        // Burst is -cos: a signal delayed by δ measures δ - π; demodulating needs θ = -δ.
        let target = wrap(std::f64::consts::PI - meas);
        let err = wrap(target - self.cphase);
        self.bamp += (amp - self.bamp) * 0.1;
        if self.bamp > 0.02 {
            self.cphase = wrap(self.cphase + (KC * err).clamp(-C_PULL, C_PULL));
        }
        self.clock += ((err.cos() as f32) - self.clock) * 0.05;
        let acc_t = (BURST_AMP / self.bamp.max(1e-3)).clamp(0.25, 4.0);
        self.acc += (acc_t - self.acc) * 0.05;
        if !self.color_on {
            if self.bamp > 0.04 && self.clock > 0.6 {
                self.color_on = true;
            }
        } else if self.bamp < 0.025 || self.clock < 0.3 {
            self.color_on = false;
        }

        // Where this line lands on the screen.
        let rowf = 2.0 * (ls - self.t_v) / SPL as f64 - self.r0;
        let row = rowf.round();
        if !(0.0..H as f64).contains(&row) {
            return;
        }
        let row = row as usize;

        // Y/C separation and demodulation at integer samples, then resample.
        let start = ls + ACT0 as f64;
        let i0 = start.floor() as i64 - 4;
        let frac = (start - start.floor()) as f32;
        let (cth, sth) = (self.cphase.cos() as f32, self.cphase.sin() as f32);
        let n = W + 12;
        let mut ibox = [0.0f32; 4];
        let mut qbox = [0.0f32; 4];
        for j in 0..n {
            let a = i0 + j as i64;
            let s = self.at(a);
            let y = 0.25 * self.at(a - 2) + 0.5 * s + 0.25 * self.at(a + 2);
            let c = s - y;
            let ph = (a & 3) as usize;
            let co = COS4[ph] * cth - SIN4[ph] * sth;
            let si = SIN4[ph] * cth + COS4[ph] * sth;
            ibox[j & 3] = 2.0 * c * co;
            qbox[j & 3] = 2.0 * c * si;
            self.yr[j] = y;
            self.ir[j] = 0.25 * (ibox[0] + ibox[1] + ibox[2] + ibox[3]);
            self.qr[j] = 0.25 * (qbox[0] + qbox[1] + qbox[2] + qbox[3]);
        }
        let porch = self.porch;
        let gain_c = if self.color_on { self.acc / CGAIN } else { 0.0 };
        let out = &mut self.raster[row * W * 3..(row + 1) * W * 3];
        for k in 0..W {
            // Sample k sits at j = k + 4 + frac. The 4-box shifts chroma by 1.5 samples.
            let jy = k + 4;
            let yv = catmull(&self.yr, jy, frac);
            let iv = catmull(&self.ir, jy + 1, frac + 0.5);
            let qv = catmull(&self.qr, jy + 1, frac + 0.5);
            let y = (yv - porch - (BLACK - BLANK)) / YGAIN;
            let i = iv * gain_c;
            let q = qv * gain_c;
            out[3 * k] = y + 0.956 * i + 0.621 * q;
            out[3 * k + 1] = y - 0.272 * i - 0.647 * q;
            out[3 * k + 2] = y - 1.106 * i + 1.703 * q;
        }
    }
}

#[inline]
fn wrap(a: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    (a + std::f64::consts::PI).rem_euclid(tau) - std::f64::consts::PI
}

/// Catmull-Rom interpolation of v at index i + t (t may exceed 1).
#[inline]
fn catmull(v: &[f32], i: usize, t: f32) -> f32 {
    let (i, t) = if t >= 1.0 { (i + 1, t - 1.0) } else { (i, t) };
    let p0 = v[i.saturating_sub(1)];
    let p1 = v[i];
    let p2 = v[(i + 1).min(v.len() - 1)];
    let p3 = v[(i + 2).min(v.len() - 1)];
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * (2.0 * p1 + (p2 - p0) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2 + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3)
}

/// Measures where a clean picture lands, so that it fills rows 0..479 exactly: a white band on
/// source rows 200 and 201 is sent, and we read back which rows it was drawn on.
fn calibrate(tpl: &Template) -> f64 {
    let mut tv = Tv::blank(0.0);
    let y: Vec<f32> = (0..W * H).map(|p| if (200..202).contains(&(p / W)) { 1.0 } else { 0.0 }).collect();
    let zero = vec![0.0f32; W * H];
    let mut frame = vec![0.0f32; FRAME];
    for f in 0..4i64 {
        let abs0 = f * FRAME as i64;
        super::encoder::encode(tpl, &y, &zero, &zero, &mut frame, (abs0 & 3) as usize);
        tv.push(&frame, abs0);
    }
    let (mut sum, mut n) = (0.0f64, 0.0f64);
    for r in 0..H {
        let mut v = 0.0f32;
        for x in 300..468 {
            v += tv.raster[(r * W + x) * 3 + 1];
        }
        if v / 168.0 > 0.5 {
            sum += r as f64;
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        (sum / n - 200.5).round()
    }
}
