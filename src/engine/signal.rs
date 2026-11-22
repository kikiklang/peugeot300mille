//! NTSC composite signal geometry, sampled at 4×fsc (14.318 MHz).
//!
//! Levels are on a 0..1 scale where 1.0 = 140 IRE: sync tip 0, blanking 0.286, black 0.3396
//! (7.5 IRE setup), white ≈ 1.0. Because the sampling rate is exactly four times the colour
//! subcarrier, the subcarrier is the 4-step table [1, 0, -1, 0].

/// Samples per line (227.5 subcarrier cycles × 4).
pub const SPL: usize = 910;
/// Lines per frame (two interlaced fields).
pub const LINES: usize = 525;
/// Samples per frame.
pub const FRAME: usize = SPL * LINES; // 477 750
/// Samples per field (262.5 lines).
pub const FIELD: usize = FRAME / 2; // 238 875
/// Half a line.
pub const HALF: usize = SPL / 2;

/// Internal picture: one pixel per active sample.
pub const W: usize = 768;
pub const H: usize = 480;

/// First active sample of a line.
pub const ACT0: usize = 134;
/// Horizontal sync: start and width (4.7 µs).
pub const HS0: usize = 21;
pub const HSW: usize = 67;
/// Colour burst window.
pub const BURST0: usize = 99;
pub const BURSTN: usize = 36;
/// Equalizing pulse width (2.3 µs) and broad-pulse serration (4.7 µs).
pub const EQW: usize = 33;
pub const SERR: usize = 67;
/// Vertical interval: 18 half-lines (6 equalizing, 6 broad, 6 equalizing).
pub const VI_HALFLINES: usize = 18;

pub const SYNC: f32 = 0.0;
pub const BLANK: f32 = 0.286;
pub const BLACK: f32 = 0.3396;
pub const YGAIN: f32 = 0.6604;
pub const CGAIN: f32 = 0.6607;
pub const BURST_AMP: f32 = 0.142_86;

/// The TV always runs at NTSC frame rate.
pub const TV_FPS: f64 = 30000.0 / 1001.0;
/// Sample period.
pub const DT: f64 = 1.0 / 14_318_181.818_181_8;

pub const COS4: [f32; 4] = [1.0, 0.0, -1.0, 0.0];
pub const SIN4: [f32; 4] = [0.0, 1.0, 0.0, -1.0];

/// One-pole low-pass coefficient for a cutoff frequency, at the signal's sample rate.
pub fn rc(fc_hz: f64) -> f32 {
    let rc = 1.0 / (2.0 * std::f64::consts::PI * fc_hz);
    (DT / (rc + DT)) as f32
}

/// The parts of a frame that do not depend on the picture: sync, blanking, burst envelope,
/// and which picture row each line carries.
pub struct Template {
    /// Sync and blanking levels for a whole frame.
    pub base: Vec<f32>,
    /// (sample index, envelope) for every burst sample.
    pub burst: Vec<(u32, f32)>,
    /// Picture row carried by each line, or -1.
    pub line_row: [i32; LINES],
    /// True for samples inside a vertical interval.
    pub vi: Vec<bool>,
}

impl Template {
    pub fn new() -> Self {
        let mut base = vec![BLANK; FRAME];
        let mut vi = vec![false; FRAME];
        for field in 0..2 {
            let v0 = field * FIELD;
            let v1 = v0 + VI_HALFLINES * HALF;
            for s in vi.iter_mut().take(v1).skip(v0) {
                *s = true;
            }
            for hi in 0..VI_HALFLINES {
                let s0 = v0 + hi * HALF + HS0;
                let width = if (6..12).contains(&hi) { HALF - SERR } else { EQW };
                for a in s0..(s0 + width).min(FRAME) {
                    base[a] = SYNC;
                }
            }
        }
        let mut burst = Vec::new();
        for l in 0..LINES {
            let a = l * SPL;
            if vi[a + HS0] {
                continue;
            }
            for s in base.iter_mut().skip(a + HS0).take(HSW) {
                *s = SYNC;
            }
            // Burst on every line outside the first 9 lines after each vertical interval start.
            let in_field2 = a >= FIELD;
            let rel = if in_field2 { a as i64 - FIELD as i64 } else { a as i64 };
            if rel >= (9 * SPL) as i64 {
                for k in 0..BURSTN {
                    let env = ((k + 1) as f32 / 4.0).min((BURSTN - k) as f32 / 4.0).min(1.0);
                    burst.push(((a + BURST0 + k) as u32, env));
                }
            }
        }
        // Field 1 lines 21..261 → even rows, field 2 lines 284..524 → odd rows.
        let mut line_row = [-1i32; LINES];
        for l in 21..261 {
            line_row[l] = 2 * (l as i32 - 21);
        }
        for l in 284..524 {
            line_row[l] = 2 * (l as i32 - 284) + 1;
        }
        Self { base, burst, line_row, vi }
    }
}

/// xorshift32, as used for every noise source.
#[derive(Clone, Copy)]
pub struct Rng(pub u32);

impl Rng {
    #[inline]
    pub fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Triangular in [-1, 1] (sum of the two 16-bit halves).
    #[inline]
    pub fn tri(&mut self) -> f32 {
        let x = self.next();
        ((x & 0xffff) + (x >> 16)) as f32 * (1.0 / 65536.0) - 1.0
    }
    /// Uniform in [0, 1).
    #[inline]
    pub fn unit(&mut self) -> f32 {
        self.next() as f32 / 4_294_967_296.0
    }
}

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// RGB → YIQ, the matrix every colour control works in (the studio encoder has its own, finer one).
#[inline]
pub fn yiq_of([r, g, b]: [f32; 3]) -> [f32; 3] {
    [0.299 * r + 0.587 * g + 0.114 * b, 0.596 * r - 0.274 * g - 0.322 * b, 0.211 * r - 0.523 * g + 0.312 * b]
}

/// YIQ → RGB, as a TV's matrix does it.
#[inline]
pub fn rgb_of(y: f32, i: f32, q: f32) -> [f32; 3] {
    [y + 0.956 * i + 0.621 * q, y - 0.272 * i - 0.647 * q, y - 1.106 * i + 1.703 * q]
}
