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
use rayon::prelude::*;

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

/// Everything needed to draw one line once the serial sync and colour tracking has passed it.
#[derive(Clone, Copy)]
struct LineJob {
    /// Absolute sample of the first column to demodulate, and the sub-sample position.
    i0: i64,
    frac: f32,
    cth: f32,
    sth: f32,
    porch: f32,
    gain_c: f32,
}

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
    /// The line to draw on each row in this frame (a later line on the same row replaces it).
    jobs: Vec<Option<LineJob>>,
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
            jobs: vec![None; H],
        }
    }

    pub fn reset(&mut self) {
        let r0 = self.r0;
        *self = Self::blank(r0);
    }

    /// Feeds one frame of signal whose first sample has absolute index `abs0`.
    pub fn push(&mut self, frame: &[f32], abs0: i64) {
        self.begin(abs0);
        self.feed(frame);
        self.end();
    }

    /// Starts a frame whose first sample has absolute index `abs0`.
    pub fn begin(&mut self, abs0: i64) {
        if self.buf.is_empty() {
            self.base = abs0;
            self.scan = abs0;
            self.line_start = abs0 as f64;
        }
        debug_assert_eq!(self.base + self.buf.len() as i64, abs0);
    }

    /// The next block of the frame: only the sync separator runs as the signal arrives.
    pub fn feed(&mut self, block: &[f32]) {
        self.buf.extend_from_slice(block);
        self.slice();
    }

    /// The frame is complete: tracks and draws its lines, then forgets what no line will look at
    /// again. The tracking has to wait for this point: it reads the sync tip as the separator
    /// left it at the end of the frame, and the separator reads the porch the tracking updates.
    pub fn end(&mut self) {
        self.lines();
        self.draw_lines();
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
        sample_at(&self.buf, self.base, a)
    }

    fn slice(&mut self) {
        let end = self.base + self.buf.len() as i64;
        // On the stack for the loop: this runs once per sample of the signal.
        let (mut tip, porch, mut in_sync, mut pulse_start) = (self.tip, self.porch, self.in_sync, self.pulse_start);
        let (mut vint, mut v_armed) = (self.vint, self.v_armed);
        let first = (self.scan - self.base) as usize;
        for (a, &s) in (self.scan..end).zip(&self.buf[first..]) {
            if s < tip {
                tip += (s - tip) * 0.02;
            } else {
                tip += (porch - tip) * 0.0002;
            }
            let span = (porch - tip).max(0.02);
            let mid = tip + 0.5 * span;
            let hyst = 0.05 * span;
            let now = if in_sync { s < mid + hyst } else { s < mid - hyst };
            if now && !in_sync {
                pulse_start = a;
            } else if !now && in_sync {
                let w = (a - pulse_start) as f64;
                if (33.5..=107.2).contains(&w) {
                    self.hedges.push(pulse_start);
                }
            }
            in_sync = now;
            vint += ((now as u8 as f32) - vint) * KV;
            if !v_armed && vint > 0.5 {
                v_armed = true;
                self.vevents.push(a);
            } else if v_armed && vint < 0.2 {
                v_armed = false;
            }
        }
        (self.tip, self.in_sync, self.pulse_start, self.vint, self.v_armed) = (tip, in_sync, pulse_start, vint, v_armed);
        self.scan = end;
    }

    /// The sync edge closest to `t`, within a quarter line (the earlier one on a tie).
    fn edge_near(&self, t: f64) -> Option<f64> {
        let first = self.hedges.partition_point(|&e| e as f64 - t < -227.5);
        let mut best: Option<(f64, f64)> = None;
        for &e in &self.hedges[first..] {
            let d = e as f64 - t;
            if d > 227.5 {
                break;
            }
            if best.is_none_or(|(bd, _)| d.abs() < bd) {
                best = Some((d.abs(), e as f64));
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
        // Only the tracking above has to run line after line: the picture itself is drawn
        // afterwards, on all cores, by `draw_lines`.
        let start = ls + ACT0 as f64;
        self.jobs[row as usize] = Some(LineJob {
            i0: start.floor() as i64 - 4,
            frac: (start - start.floor()) as f32,
            cth: self.cphase.cos() as f32,
            sth: self.cphase.sin() as f32,
            porch: self.porch,
            gain_c: if self.color_on { self.acc / CGAIN } else { 0.0 },
        });
    }

    /// Draws the lines of this frame into the raster.
    fn draw_lines(&mut self) {
        let (buf, base) = (&self.buf[..], self.base);
        self.raster.par_chunks_mut(W * 3).zip(self.jobs.par_iter_mut()).for_each(|(out, job)| {
            if let Some(job) = job.take() {
                draw_line(buf, base, &job, out);
            }
        });
    }
}

/// The signal at absolute sample `a`; blanking outside what is buffered.
#[inline]
fn sample_at(buf: &[f32], base: i64, a: i64) -> f32 {
    let i = a - base;
    if i < 0 || i as usize >= buf.len() {
        BLANK
    } else {
        buf[i as usize]
    }
}

/// Y/C separation and demodulation at integer samples, then resampling into one raster row.
fn draw_line(buf: &[f32], base: i64, job: &LineJob, out: &mut [f32]) {
    let &LineJob { i0, frac, cth, sth, porch, gain_c } = job;
    let mut yr = [0.0f32; W + 16];
    let mut ir = [0.0f32; W + 16];
    let mut qr = [0.0f32; W + 16];
    let mut ibox = [0.0f32; 4];
    let mut qbox = [0.0f32; 4];
    for j in 0..W + 12 {
        let a = i0 + j as i64;
        let s = sample_at(buf, base, a);
        let y = 0.25 * sample_at(buf, base, a - 2) + 0.5 * s + 0.25 * sample_at(buf, base, a + 2);
        let c = s - y;
        let ph = (a & 3) as usize;
        let co = COS4[ph] * cth - SIN4[ph] * sth;
        let si = SIN4[ph] * cth + COS4[ph] * sth;
        ibox[j & 3] = 2.0 * c * co;
        qbox[j & 3] = 2.0 * c * si;
        yr[j] = y;
        ir[j] = 0.25 * (ibox[0] + ibox[1] + ibox[2] + ibox[3]);
        qr[j] = 0.25 * (qbox[0] + qbox[1] + qbox[2] + qbox[3]);
    }
    for (k, px) in out.chunks_exact_mut(3).enumerate() {
        // Sample k sits at j = k + 4 + frac. The 4-box shifts chroma by 1.5 samples.
        let jy = k + 4;
        let yv = catmull(&yr, jy, frac);
        let iv = catmull(&ir, jy + 1, frac + 0.5);
        let qv = catmull(&qr, jy + 1, frac + 0.5);
        let y = (yv - porch - (BLACK - BLANK)) / YGAIN;
        px.copy_from_slice(&rgb_of(y, iv * gain_c, qv * gain_c));
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
