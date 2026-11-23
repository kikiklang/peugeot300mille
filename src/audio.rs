//! The soundtrack: decoded once, analysed per TV frame (what drives the knobs), and played back
//! during the preview.

use crate::engine::signal::TV_FPS;
use crate::media::{local_arg, lock, StderrTail};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Sources that can drive a knob.
pub const SOURCES: [&str; 16] = [
    "—", "Volume", "Basses", "Médiums", "Aigus", "Attaques", "Bande 1", "Bande 2", "Bande 3", "Bande 4",
    "Tempo · ½ temps", "Tempo · 1 temps", "Tempo · 2 temps", "Tempo · 4 temps", "Vague · 1 temps", "Vague · 4 temps",
];
/// Sources offered in the menus: none, Volume, Attacks, Bands 1–4, Tempo. (Bass, mid and treble
/// are still measured so that older saved settings keep working, but are no longer offered.)
pub const MENU_SOURCES: [u8; 13] = [0, 1, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
/// First source that follows the tempo.
pub const FIRST_TEMPO_SOURCE: u8 = 10;

/// Measures per frame: the 5 global ones, the 4 user bands, then the 6 tempo ones
/// (source `i` is measure `i - 1`).
pub const N_FEAT: usize = 15;
pub const N_GLOBAL: usize = 5;
pub const N_BANDS: usize = 4;
pub const N_TEMPO: usize = 6;

/// Spectrum resolution: 1/12 octave from 20 Hz to 20.48 kHz.
pub const SPEC_BINS: usize = 120;
pub const SPEC_F0: f32 = 20.0;
const FFT_N: usize = 4096;

/// Centre frequency of a spectrum bin.
pub fn bin_freq(b: f32) -> f32 {
    SPEC_F0 * 2f32.powf((b + 0.5) / 12.0)
}
/// Fractional bin of a frequency.
pub fn freq_bin(f: f32) -> f32 {
    12.0 * (f.max(1.0) / SPEC_F0).log2() - 0.5
}

pub const RATE: u32 = 48_000;

/// Far beyond any real level, but small enough that squaring it can't overflow the analysis.
const MAX_SAMPLE: f32 = 16.0;

/// The decoded soundtrack, stereo interleaved, 48 kHz.
pub struct Track {
    pub pcm: Vec<f32>,
    /// The file could not be read to its end (damaged, or too long for the free memory).
    pub truncated: bool,
}

impl Track {
    pub fn decode(path: &Path) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-i"])
            .arg(local_arg(path).as_os_str())
            .args(["-vn", "-ac", "2", "-ar", &RATE.to_string(), "-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("ffmpeg introuvable : installez ffmpeg")?;
        let errors = StderrTail::capture(&mut child);
        let mut stdout = child.stdout.take().unwrap();
        let (pcm, cut_short) = read_pcm(&mut stdout, max_pcm_samples());
        drop(stdout);
        if cut_short {
            let _ = child.kill();
        }
        let status = child.wait().context("ffmpeg")?;
        let why = errors.finish();
        if pcm.is_empty() {
            if status.success() {
                bail!("aucun son dans ce fichier");
            }
            bail!("son illisible : {why}");
        }
        // A damaged file often still has hours of good sound: keep it, and say so.
        Ok(Self { pcm, truncated: cut_short || !status.success() })
    }
}

/// Stereo samples from a raw f32 stream, converted as they come so that a long file never sits
/// in memory twice. Stops at `limit` samples or on a read error; the flag tells it did.
fn read_pcm(r: &mut impl Read, limit: usize) -> (Vec<f32>, bool) {
    let (mut pcm, mut buf, mut held) = (Vec::new(), vec![0u8; 1 << 16], 0);
    let cut_short = loop {
        match r.read(&mut buf[held..]) {
            Ok(0) => break false,
            Ok(n) => {
                held += n;
                let whole = held / 4;
                let take = whole.min(limit.saturating_sub(pcm.len()));
                if pcm.try_reserve(take).is_err() {
                    break true;
                }
                pcm.extend(buf[..take * 4].chunks_exact(4).map(|b| clean_sample(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))));
                if take < whole {
                    break true;
                }
                buf.copy_within(whole * 4..held, 0);
                held -= whole * 4;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break true,
        }
    };
    pcm.truncate(pcm.len() & !1);
    pcm.shrink_to_fit();
    (pcm, cut_short)
}

/// Float files can hold NaN or huge values: one of them would stick in the filters for good.
fn clean_sample(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-MAX_SAMPLE, MAX_SAMPLE)
    } else {
        0.0
    }
}

/// How many samples can be kept: 3/4 of the free memory, since running out of it gets the whole
/// app killed by the system (a 2-hour file takes 2.8 GB).
fn max_pcm_samples() -> usize {
    let info = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    mem_available(&info).map_or(usize::MAX, |bytes| bytes / 4 / 4 * 3)
}

fn mem_available(meminfo: &str) -> Option<usize> {
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: usize = line.split_whitespace().nth(1)?.parse().ok()?;
    kb.checked_mul(1024)
}

/// TV frames covered by this many stereo samples.
fn tv_frames(n_samples: usize) -> usize {
    (n_samples as f64 / (RATE as f64 / TV_FPS)).ceil() as usize
}

/// A windowed FFT of the mono mix, shared by the spectrum and the onset analysis.
struct Transform {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
}

impl Transform {
    fn new(n: usize) -> Self {
        let window = (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).collect();
        Self { fft: FftPlanner::<f32>::new().plan_fft_forward(n), window }
    }

    /// One thread's working memory: the FFT would otherwise allocate its scratch at every call.
    fn buffers(&self) -> (Vec<Complex<f32>>, Vec<Complex<f32>>) {
        let zero = Complex::new(0.0, 0.0);
        (vec![zero; self.window.len()], vec![zero; self.fft.get_inplace_scratch_len()])
    }

    /// The spectrum around sample `centre`; what falls outside the track counts as silence.
    fn run(&self, pcm: &[f32], centre: isize, (buf, scratch): &mut (Vec<Complex<f32>>, Vec<Complex<f32>>)) {
        let n = self.window.len() as isize;
        let start = centre - n / 2;
        let (a, b) = ((-start).clamp(0, n) as usize, ((pcm.len() / 2) as isize - start).clamp(0, n) as usize);
        let zero = Complex::new(0.0, 0.0);
        buf[..a].fill(zero);
        if a < b {
            let from = (start + a as isize) as usize;
            let frames = pcm[2 * from..2 * (from + b - a)].chunks_exact(2);
            for ((c, w), s) in buf[a..b].iter_mut().zip(&self.window[a..b]).zip(frames) {
                *c = Complex::new(0.5 * (s[0] + s[1]) * w, 0.0);
            }
        }
        buf[b..].fill(zero);
        self.fft.process_with_scratch(buf, scratch);
    }
}

/// Per TV frame: volume, bass, mid, treble, hits — each 0..1.
pub struct Features {
    pub rows: Vec<[f32; N_GLOBAL]>,
}

/// Envelope follower used for every measure: to 0..1 by its loud end (95th percentile), then
/// fast up, slower down.
fn normalise(vals: &[f32], release: f32) -> Vec<f32> {
    normalise_floor(vals, release, 1e-6)
}

/// Same, but a measure whose loud end stays under `floor` is treated as near silence.
fn normalise_floor(vals: &[f32], release: f32, floor: f32) -> Vec<f32> {
    let mut s = vals.to_vec();
    let at = (s.len() as f32 * 0.95) as usize;
    let hi = if at < s.len() { *s.select_nth_unstable_by(at, f32::total_cmp).1 } else { 1.0 }.max(floor);
    let mut env = 0.0f32;
    vals.iter()
        .map(|v| {
            let x = (v / hi).min(1.0);
            env = if x > env { x } else { env * release + x * (1.0 - release) };
            env
        })
        .collect()
}

/// Rise of an energy (log) above its recent average: the attacks.
fn onsets(energy: &[f32]) -> Vec<f32> {
    let mut hist = VecDeque::new();
    energy
        .iter()
        .map(|&e| {
            // Below about -60 dBFS nothing counts as an attack.
            let e = e.max(1e-6).ln();
            let avg = if hist.is_empty() { e } else { hist.iter().sum::<f32>() / hist.len() as f32 };
            hist.push_back(e);
            if hist.len() > 4 {
                hist.pop_front();
            }
            (e - avg).max(0.0)
        })
        .collect()
}

/// Power per 1/12-octave bin, per TV frame (full scale sine ≈ 1.0).
pub struct Spectrum {
    pub rows: Vec<f32>,
    pub frames: usize,
}

impl Spectrum {
    fn analyse(track: &Track) -> Self {
        let frame_len = RATE as f64 / TV_FPS;
        let frames = tv_frames(track.pcm.len() / 2);
        let transform = Transform::new(FFT_N);
        // Which spectrum bin each FFT bin falls in.
        let hz = RATE as f32 / FFT_N as f32;
        let map: Vec<i32> = (0..FFT_N / 2)
            .map(|k| {
                let b = freq_bin(k as f32 * hz).round();
                if k == 0 || b < 0.0 || b >= SPEC_BINS as f32 { -1 } else { b as i32 }
            })
            .collect();
        let scale = 1.0 / (FFT_N as f32 / 4.0).powi(2);
        let mut rows = vec![0.0f32; frames * SPEC_BINS];
        rows.par_chunks_mut(SPEC_BINS).enumerate().for_each_init(
            || transform.buffers(),
            |work, (f, row)| {
                transform.run(&track.pcm, ((f as f64 + 0.5) * frame_len) as isize, work);
                for (k, &b) in map.iter().enumerate() {
                    if b >= 0 {
                        row[b as usize] += work.0[k].norm_sqr() * scale;
                    }
                }
                // Bins too narrow to hold an FFT bin borrow their neighbour's level.
                for b in 1..SPEC_BINS {
                    if row[b] == 0.0 {
                        row[b] = row[b - 1];
                    }
                }
            },
        );
        Self { rows, frames }
    }

    pub fn row(&self, k: i64) -> &[f32] {
        if self.frames == 0 {
            return &[];
        }
        let k = (k.max(0) as usize).min(self.frames - 1);
        &self.rows[k * SPEC_BINS..(k + 1) * SPEC_BINS]
    }

    /// Band energy per frame, between two frequencies.
    fn band_energy(&self, lo: f32, hi: f32) -> Vec<f32> {
        let b0 = freq_bin(lo.min(hi)).round().clamp(0.0, SPEC_BINS as f32 - 1.0) as usize;
        let b1 = freq_bin(lo.max(hi)).round().clamp(b0 as f32, SPEC_BINS as f32 - 1.0) as usize;
        (0..self.frames).map(|f| self.rows[f * SPEC_BINS + b0..=f * SPEC_BINS + b1].iter().sum()).collect()
    }
}

impl Features {
    pub fn analyse(track: &Track) -> Self {
        let frame_len = RATE as f64 / TV_FPS;
        let n_samples = track.pcm.len() / 2;
        let n_frames = tv_frames(n_samples);
        // Band split with one-pole filters: bass < 150 Hz, treble > 3 kHz, mid in between.
        let a = |fc: f64| (1.0 - (-2.0 * std::f64::consts::PI * fc / RATE as f64).exp()) as f32;
        let (a_lo, a_hi) = (a(150.0), a(3000.0));
        let (mut lo1, mut lo2, mut hi1) = (0.0f32, 0.0f32, 0.0f32);
        let mut raw = vec![[0.0f32; 4]; n_frames];
        let mut start = 0;
        for (k, r) in raw.iter_mut().enumerate() {
            let end = if k + 1 == n_frames { n_samples } else { frame_end(k, frame_len).min(n_samples) };
            for s in track.pcm[2 * start..2 * end].chunks_exact(2) {
                let x = 0.5 * (s[0] + s[1]);
                lo1 += a_lo * (x - lo1);
                lo2 += a_lo * (lo1 - lo2);
                hi1 += a_hi * (x - hi1);
                let treble = x - hi1;
                let bass = lo2;
                let mid = hi1 - lo2;
                r[0] += x * x;
                r[1] += bass * bass;
                r[2] += mid * mid;
                r[3] += treble * treble;
            }
            let count = (end - start).max(1) as f32;
            for v in r.iter_mut() {
                *v = (*v / count).sqrt();
            }
            start = end;
        }
        // Hits: rise of low+mid energy (log) above its recent average.
        let flux = onsets(&raw.iter().map(|r| r[1] + r[2]).collect::<Vec<_>>());
        let cols: Vec<Vec<f32>> = (0..4).map(|c| normalise(&raw.iter().map(|r| r[c]).collect::<Vec<_>>(), 0.75)).collect();
        let hits = normalise(&flux, 0.55);
        let rows = (0..n_frames).map(|k| [cols[0][k], cols[1][k], cols[2][k], cols[3][k], hits[k]]).collect();
        Self { rows }
    }

    pub fn at(&self, tv_k: i64) -> [f32; N_GLOBAL] {
        if tv_k < 0 || self.rows.is_empty() {
            return [0.0; N_GLOBAL];
        }
        self.rows[(tv_k as usize).min(self.rows.len() - 1)]
    }
}

/// First sample of the TV frame after `k`: the first `i` whose `i / frame_len` passes `k`, found
/// once per frame instead of dividing at every sample.
fn frame_end(k: usize, frame_len: f64) -> usize {
    let mut e = ((k + 1) as f64 * frame_len) as usize;
    while (e as f64 / frame_len) as usize <= k {
        e += 1;
    }
    while e > 0 && ((e - 1) as f64 / frame_len) as usize > k {
        e -= 1;
    }
    e
}

/// The beats of the track: its tempo, and where each TV frame falls in beats. The beats are
/// tracked one by one, so the grid follows a tempo that drifts.
pub struct BeatGrid {
    pub bpm: f64,
    /// Position in beats at each TV frame.
    pos: Vec<f64>,
    /// The track's attacks, to land a restart on the hit it was aimed at.
    onsets: Vec<f32>,
}

impl BeatGrid {
    /// Onset analysis runs at 100 measures a second.
    const HOP: usize = 480;
    const WIN: usize = 1024;
    const LOG_BANDS: usize = 24;

    /// How much new energy appears at each hop, summed over log-spaced bands.
    fn onset_strength(track: &Track) -> Vec<f32> {
        let hops = track.pcm.len() / 2 / Self::HOP;
        let transform = Transform::new(Self::WIN);
        // Band edges in FFT bins, up to about 9 kHz.
        let mut edges = [1usize; Self::LOG_BANDS + 1];
        for i in 1..=Self::LOG_BANDS {
            let e = 200f32.powf(i as f32 / Self::LOG_BANDS as f32).round() as usize;
            edges[i] = e.max(edges[i - 1] + 1);
        }
        let scale = 1.0 / (Self::WIN as f32 / 4.0).powi(2);
        let mut levels = vec![0.0f32; hops * Self::LOG_BANDS];
        levels.par_chunks_mut(Self::LOG_BANDS).enumerate().for_each_init(
            || transform.buffers(),
            |work, (h, row)| {
                transform.run(&track.pcm, (h * Self::HOP) as isize, work);
                for (b, v) in row.iter_mut().enumerate() {
                    let power: f32 = work.0[edges[b]..edges[b + 1]].iter().map(|c| c.norm_sqr()).sum();
                    *v = (1.0 + 100.0 * (power * scale).sqrt()).ln();
                }
            },
        );
        let mut flux = vec![0.0f32; hops];
        for h in 1..hops {
            let (prev, cur) = (&levels[(h - 1) * Self::LOG_BANDS..h * Self::LOG_BANDS], &levels[h * Self::LOG_BANDS..(h + 1) * Self::LOG_BANDS]);
            flux[h] = cur.iter().zip(prev).map(|(c, p)| (c - p).max(0.0)).sum();
        }
        flux
    }

    /// The most likely beat period in hops. A beat repeats at all its multiples, so each candidate
    /// is scored on a comb of them: first coarsely with a preference for usual tempos, which
    /// settles double and half time, then finely over many multiples, which settles the decimals.
    fn period(o: &[f32]) -> Option<f64> {
        const MIN_LAG: f64 = 33.0; // About 180 BPM.
        const MAX_LAG: f64 = 86.0; // About 70 BPM.
        const TEETH: usize = 16;
        if o.len() < MAX_LAG as usize * 4 {
            return None;
        }
        let top = (MAX_LAG as usize * TEETH + 2).min(o.len() / 2);
        let mut ac: Vec<f32> = (0..=top)
            .into_par_iter()
            .map(|l| o[l..].iter().zip(o).map(|(a, b)| a * b).sum::<f32>() / (o.len() - l) as f32)
            .collect();
        // Only what stands above the average similarity is rhythm.
        let floor = ac[MIN_LAG as usize..].iter().sum::<f32>() / (ac.len() - MIN_LAG as usize) as f32;
        ac.iter_mut().for_each(|v| *v = (*v - floor).max(0.0));
        let at = |l: f64| {
            let i = l as usize;
            if i + 1 >= ac.len() { 0.0 } else { ac[i] + (ac[i + 1] - ac[i]) * (l - i as f64) as f32 }
        };
        // Coarsely, a tooth takes the best value around it: peaks are narrow and lags are whole.
        let around = |l: f64| at(l - 1.0).max(at(l)).max(at(l + 1.0));
        let comb_with = |value: &dyn Fn(f64) -> f32, p: f64, teeth: usize| {
            let (mut sum, mut weight) = (0.0, 0.0);
            for k in 1..=teeth {
                let l = p * k as f64;
                if l + 1.0 >= ac.len() as f64 {
                    break;
                }
                // A pattern repeats exactly every bar: that tells a beat from a syncopation.
                let w = if k % 4 == 0 { 2.0 } else { 1.0 } / (k as f32).sqrt();
                sum += value(l) * w;
                weight += w;
            }
            if weight > 0.0 { sum / weight } else { 0.0 }
        };
        let rough = |p: f64| comb_with(&around, p, 8);
        let usual = |p: f64| (-0.5 * ((6000.0 / p / 120.0).log2() / 0.8).powi(2)).exp() as f32;
        let steps = ((MAX_LAG - MIN_LAG) / 0.25) as usize;
        let mut coarse = (0..=steps)
            .map(|i| MIN_LAG + i as f64 * 0.25)
            .max_by(|&a, &b| (rough(a) * usual(a)).total_cmp(&(rough(b) * usual(b))))?;
        // As strong at half the period: the real pulse is the faster one.
        if coarse / 2.0 >= MIN_LAG && rough(coarse / 2.0) >= 0.85 * rough(coarse) {
            coarse /= 2.0;
        }
        (-60..=60).map(|i| coarse + i as f64 * 0.01).max_by(|&a, &b| comb_with(&at, a, TEETH).total_cmp(&comb_with(&at, b, TEETH)))
    }

    /// A straight line through the beats, if the track really keeps a steady tempo: `(start,
    /// period)` in hops, and each beat's number. Robust to a few beats landing off the grid.
    fn steady_fit(hops: &[usize], period: f64) -> Option<(f64, f64, Vec<i64>)> {
        let mut numbers = Vec::with_capacity(hops.len());
        let mut n = 0i64;
        for (i, &h) in hops.iter().enumerate() {
            if i > 0 {
                n += (((h - hops[i - 1]) as f64 / period).round() as i64).max(1);
            }
            numbers.push(n);
        }
        let line = |keep: &dyn Fn(usize) -> bool| -> Option<(f64, f64)> {
            let (mut c, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for i in (0..hops.len()).filter(|&i| keep(i)) {
                let (x, y) = (numbers[i] as f64, hops[i] as f64);
                c += 1.0;
                sx += x;
                sy += y;
                sxx += x * x;
                sxy += x * y;
            }
            let d = c * sxx - sx * sx;
            (d > 0.0).then(|| ((sy * sxx - sx * sxy) / d, (c * sxy - sx * sy) / d))
        };
        let (a, b) = line(&|_| true)?;
        const NEAR: f64 = 3.0; // 30 ms.
        let near = |i: usize, a: f64, b: f64| (hops[i] as f64 - (a + b * numbers[i] as f64)).abs() <= NEAR;
        let (a, b) = line(&|i| near(i, a, b))?;
        // A straight line must hold everywhere: a stretch off it, even a short one at the end of a
        // long track, means the tempo moved and the beats have to be followed one by one.
        let (mut inliers, mut run, mut longest) = (0, 0, 0);
        for i in 0..hops.len() {
            if near(i, a, b) {
                inliers += 1;
                run = 0;
            } else {
                run += 1;
                longest = longest.max(run);
            }
        }
        (inliers * 20 >= hops.len() * 19 && longest <= 8 && b > 0.0).then_some((a, b, numbers))
    }

    /// A long track at one tempo, measured over its whole length: `(first beat, period)` in hops.
    /// Beat-by-beat tracking can gain a beat at each break or syncopation, which adds up to a
    /// wrong tempo on a long track. So each 10 s window says where the pulse falls on a ruler at
    /// the estimated period: if those places form a straight line, its slope gives the exact
    /// period. Half-beats are used because the loudest hits can move to the off-beat for a while.
    fn steady_grid(o: &[f32], period: f64) -> Option<(f64, f64)> {
        const WINDOW: usize = 1000;
        let windows = o.len() / WINDOW;
        if windows < 6 {
            return None;
        }
        let half = period / 2.0;
        let at = |x: f64| {
            let i = x as usize;
            if i + 1 >= o.len() { 0.0 } else { o[i] + (o[i + 1] - o[i]) * (x - i as f64) as f32 }
        };
        let raw: Vec<(f64, f64)> = (0..windows)
            .into_par_iter()
            .map(|w| {
                let start = ((w * WINDOW) as f64 / half).ceil() * half;
                let end = ((w + 1) * WINDOW) as f64;
                let strength = |ph: f64| {
                    let (mut x, mut sum) = (start + ph, 0.0f32);
                    while x < end {
                        sum += at(x);
                        x += half;
                    }
                    sum
                };
                let steps = (half * 4.0) as usize;
                let best = (0..steps).map(|i| i as f64 / 4.0).max_by(|&a, &b| strength(a).total_cmp(&strength(b))).unwrap_or(0.0);
                (start, best)
            })
            .collect();
        // The phase wraps every half-beat: follow it from the recent windows, so that one odd
        // window can't throw the rest off by a half-beat.
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(raw.len());
        for &(t, ph) in &raw {
            let mut recent: Vec<f64> = points.iter().rev().take(5).map(|p| p.1).collect();
            recent.sort_by(f64::total_cmp);
            let reference = recent.get(recent.len() / 2).copied().unwrap_or(ph);
            points.push((t, ph + ((reference - ph) / half).round() * half));
        }
        let line = |keep: &dyn Fn(&(f64, f64)) -> bool| -> Option<(f64, f64)> {
            let (mut c, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for &(x, y) in points.iter().filter(|p| keep(p)) {
                c += 1.0;
                sx += x;
                sy += y;
                sxx += x * x;
                sxy += x * y;
            }
            let d = c * sxx - sx * sx;
            (d > 0.0).then(|| ((sy * sxx - sx * sxy) / d, (c * sxy - sx * sy) / d))
        };
        let (mut a, mut slope) = line(&|_| true)?;
        // Tightening in steps: the line settles on the bulk of the track, so a part that really
        // changed tempo stays off it instead of tilting it.
        for limit in [6.0, 6.0, 3.0, 3.0] {
            (a, slope) = line(&|&(x, y)| (y - (a + slope * x)).abs() <= limit)?;
        }
        // The test of a steady track: in each window, does the straight grid land about as hard
        // as the best grid for that window alone? A real change of tempo leaves it between the
        // hits; a window that is only ambiguous (a break, a syncopation) does not.
        let exact = period * (1.0 + slope);
        let step = exact / 2.0;
        let near = |x: f64| at(x - 1.0).max(at(x)).max(at(x + 1.0));
        let misses: Vec<bool> = (0..windows)
            .into_par_iter()
            .map(|w| {
                let (start, end) = ((w * WINDOW) as f64, ((w + 1) * WINDOW) as f64);
                let strength = |first: f64| {
                    let (mut x, mut sum) = (first, 0.0f32);
                    while x < end {
                        sum += near(x);
                        x += step;
                    }
                    sum
                };
                let on_grid = strength(start + (a - start).rem_euclid(step));
                let best = (0..(step * 2.0) as usize).map(|i| strength(start + i as f64 / 2.0)).fold(0.0f32, f32::max);
                best > 1.0 && on_grid < 0.5 * best
            })
            .collect();
        // A real change of tempo never comes back onto the line: the grid then misses for a long
        // stretch, or through the very end or start. A miss the line recovers from is only noise.
        let missed = misses.iter().filter(|&&m| m).count();
        let longest = misses.split(|m| !*m).map(<[bool]>::len).max().unwrap_or(0);
        if missed * 100 > windows * 15 || longest > 3 || misses[0] || misses[windows - 1] {
            return None;
        }
        // On the ruler or between its marks: the beat is whichever lands on the harder hits.
        let total = |first: f64| {
            let (mut x, mut sum) = (first, 0.0f32);
            while x < o.len() as f64 {
                sum += at(x);
                x += exact;
            }
            sum
        };
        let first = a.rem_euclid(exact);
        let other = (first + exact / 2.0).rem_euclid(exact);
        Some((if total(other) > total(first) { other } else { first }, exact))
    }

    /// Which of the four beats of a bar hits hardest: that one becomes the "1".
    fn strongest_of_four(o: &[f32], hops: &[usize], numbers: &[i64]) -> f64 {
        let mut sum = [0.0f32; 4];
        for (&h, &n) in hops.iter().zip(numbers) {
            sum[n.rem_euclid(4) as usize] += o[h];
        }
        (0..4).max_by(|&a, &b| sum[a].total_cmp(&sum[b])).unwrap_or(0) as f64
    }

    /// Picks the chain of hops that lands on strong onsets while keeping close to the period.
    fn track_beats(o: &[f32], period: f64) -> Vec<usize> {
        const TIGHTNESS: f64 = 100.0;
        let lo = (period / 2.0).round().max(1.0) as usize;
        let hi = (period * 2.0).round() as usize;
        let cost: Vec<f32> = (0..=hi).map(|d| (TIGHTNESS * (d.max(1) as f64 / period).ln().powi(2)) as f32).collect();
        let mut score = vec![0.0f32; o.len()];
        let mut back = vec![usize::MAX; o.len()];
        for t in 0..o.len() {
            let mut best = 0.0f32;
            if t >= lo {
                for d in lo..=hi.min(t) {
                    let v = score[t - d] - cost[d];
                    if v > best {
                        best = v;
                        back[t] = t - d;
                    }
                }
            }
            score[t] = o[t] + best;
        }
        let tail = o.len().saturating_sub(period.ceil() as usize);
        let Some(mut t) = (tail..o.len()).max_by(|&a, &b| score[a].total_cmp(&score[b])) else {
            return Vec::new();
        };
        let mut beats = vec![t];
        while back[t] != usize::MAX {
            t = back[t];
            beats.push(t);
        }
        beats.reverse();
        beats
    }

    /// None if the track has no usable pulse (silence, very short, a steady tone).
    pub fn analyse(track: &Track) -> Option<Self> {
        let flux = Self::onset_strength(track);
        // A pulse needs several separate attacks: the start of a steady tone is only one.
        if flux.windows(2).filter(|w| w[0] < 0.5 && w[1] >= 0.5).count() < 4 {
            return None;
        }
        // Only what rises above the last and next half second counts.
        let mut sum = vec![0.0f64; flux.len() + 1];
        for (i, v) in flux.iter().enumerate() {
            sum[i + 1] = sum[i] + *v as f64;
        }
        let mut o: Vec<f32> = (0..flux.len())
            .map(|i| {
                let (a, b) = (i.saturating_sub(50), (i + 51).min(flux.len()));
                (flux[i] - ((sum[b] - sum[a]) / (b - a) as f64) as f32).max(0.0)
            })
            .collect();
        let mean = o.iter().sum::<f32>() / o.len().max(1) as f32;
        let std = (o.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / o.len().max(1) as f32).sqrt();
        if std < 1e-4 {
            return None;
        }
        o.iter_mut().for_each(|v| *v /= std);
        let period = Self::period(&o)?;
        let frames = tv_frames(track.pcm.len() / 2);
        let secs = Self::HOP as f64 / RATE as f64;
        if let Some((a, b)) = Self::steady_grid(&o, period) {
            let count = ((o.len() as f64 - a) / b).ceil().max(0.0) as i64;
            let numbers: Vec<i64> = (0..count).collect();
            let hops: Vec<usize> = numbers.iter().map(|&n| ((a + b * n as f64).round() as usize).min(o.len() - 1)).collect();
            let bar = Self::strongest_of_four(&o, &hops, &numbers);
            let pos = (0..frames).map(|k| (k as f64 / TV_FPS - a * secs) / (b * secs) - bar).collect();
            return Some(Self { bpm: 60.0 / (b * secs), pos, onsets: o });
        }
        let hops = Self::track_beats(&o, period);
        if hops.len() < 4 {
            return None;
        }
        // A steady track gets a perfectly regular grid; otherwise the beats are followed one by one.
        if let Some((a, b, numbers)) = Self::steady_fit(&hops, period) {
            let bar = Self::strongest_of_four(&o, &hops, &numbers);
            let pos = (0..frames).map(|k| (k as f64 / TV_FPS - a * secs) / (b * secs) - bar).collect();
            return Some(Self { bpm: 60.0 / (b * secs), pos, onsets: o });
        }
        let numbers: Vec<i64> = (0..hops.len() as i64).collect();
        let bar = Self::strongest_of_four(&o, &hops, &numbers);
        let (mean_gap, mut pos) = Self::positions(&hops, frames);
        pos.iter_mut().for_each(|v| *v -= bar);
        Some(Self { bpm: 60.0 / mean_gap, pos, onsets: o })
    }

    /// Mean time between the beats, and the position in beats at each TV frame.
    fn positions(hops: &[usize], frames: usize) -> (f64, Vec<f64>) {
        let secs = Self::HOP as f64 / RATE as f64;
        let beats: Vec<f64> = hops.iter().map(|&h| h as f64 * secs).collect();
        let mean_gap = (beats[beats.len() - 1] - beats[0]) / (beats.len() - 1) as f64;
        let mut i = 0;
        let pos = (0..frames)
            .map(|k| {
                let t = k as f64 / TV_FPS;
                while i + 2 < beats.len() && t >= beats[i + 1] {
                    i += 1;
                }
                if t < beats[0] {
                    (t - beats[0]) / mean_gap
                } else if t >= beats[beats.len() - 1] {
                    (beats.len() - 1) as f64 + (t - beats[beats.len() - 1]) / mean_gap
                } else {
                    i as f64 + (t - beats[i]) / (beats[i + 1] - beats[i])
                }
            })
            .collect();
        (mean_gap, pos)
    }

    /// Position on the track's own grid at a TV frame. Past the end of the analysis the beats
    /// carry on at the same pace.
    fn grid_at(&self, k: f64) -> Option<f64> {
        let last = self.pos.len().checked_sub(1)?;
        let k = k.max(0.0);
        if k >= last as f64 {
            return Some(self.pos[last] + (k - last as f64) * self.bpm / 60.0 / TV_FPS);
        }
        let (i, f) = (k as usize, k.fract());
        Some(self.pos[i] + (self.pos[i + 1] - self.pos[i]) * f)
    }

    /// "Align start" is clicked by hand, a little early or late: it lands on the strongest attack
    /// within 80 ms, if there is one.
    fn snap_to_hit(&self, t: f64) -> f64 {
        let per_second = RATE as f64 / Self::HOP as f64;
        let h = (t * per_second).round() as usize;
        let (lo, hi) = (h.saturating_sub(8), (h + 9).min(self.onsets.len()));
        let best = (lo..hi).max_by(|&a, &b| self.onsets[a].total_cmp(&self.onsets[b]));
        match best {
            Some(i) if self.onsets[i] >= 1.5 => i as f64 / per_second,
            _ => t,
        }
    }

    /// Position in beats at a TV frame, with the user's restart, half/double time and shift.
    fn position(&self, tv_k: i64, octave: i8, phase: f32, restart: f32) -> Option<f64> {
        let at = self.grid_at(tv_k as f64)?;
        let origin = if restart >= 0.0 { self.grid_at(self.snap_to_hit(restart as f64) * TV_FPS)? } else { 0.0 };
        Some((at - origin + phase as f64) * 2f64.powi(octave as i32))
    }

    /// Pulses on ½, 1, 2 and 4 beats, then slow waves over 1 and 4 beats, each 0..1.
    #[cfg(test)]
    fn measures(&self, tv_k: i64, octave: i8, phase: f32) -> [f32; N_TEMPO] {
        self.position(tv_k, octave, phase, -1.0).map_or([0.0; N_TEMPO], beat_shapes)
    }
}

/// What each tempo source is worth at a position in beats.
fn beat_shapes(pos: f64) -> [f32; N_TEMPO] {
    let pulse = |div: f64| (-5.0 * pos.rem_euclid(div) / div.min(1.0)).exp() as f32;
    let wave = |div: f64| (0.5 + 0.5 * (std::f64::consts::TAU * pos / div).cos()) as f32;
    [pulse(0.5), pulse(1.0), pulse(2.0), pulse(4.0), wave(1.0), wave(4.0)]
}

/// The soundtrack and its analysis, loaded once per file.
pub struct Sound {
    pub track: Track,
    pub features: Features,
    pub spectrum: Spectrum,
    pub tempo: Option<BeatGrid>,
    bands: Mutex<BandCache>,
}

/// A user band's measure, by (low Hz, high Hz, mode).
type BandCache = HashMap<(u32, u32, u8), Arc<Vec<f32>>>;

impl Sound {
    pub fn load(path: &Path) -> Result<Arc<Self>> {
        let track = Track::decode(path)?;
        // The features are one long sequential filter: the other two share the remaining cores.
        let (features, (spectrum, tempo)) =
            rayon::join(|| Features::analyse(&track), || rayon::join(|| Spectrum::analyse(&track), || BeatGrid::analyse(&track)));
        Ok(Arc::new(Self { track, features, spectrum, tempo, bands: Mutex::new(HashMap::new()) }))
    }

    /// A user band's measure over the whole file (computed once per setting, then kept).
    fn band(&self, b: &crate::params::Band) -> Arc<Vec<f32>> {
        let key = (b.lo.round() as u32, b.hi.round() as u32, b.mode);
        if let Some(v) = lock(&self.bands).get(&key) {
            return v.clone();
        }
        let e = self.spectrum.band_energy(b.lo, b.hi);
        let v = Arc::new(if b.mode == 1 {
            normalise(&onsets(&e), 0.55)
        } else {
            // Level floor about -45 dBFS: an empty band stays at zero.
            normalise_floor(&e.iter().map(|x| x.sqrt()).collect::<Vec<_>>(), 0.75, 0.006)
        });
        let mut cache = lock(&self.bands);
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(key, v.clone());
        v
    }

    /// Where the music is at a TV frame, in beats. None if the track has no tempo.
    pub fn beat_position(&self, tv_k: i64, p: &crate::params::Params) -> Option<f64> {
        self.tempo.as_ref()?.position(tv_k, p.tempo_octave, p.tempo_phase, p.tempo_restart)
    }

    /// All measures for a TV frame: the 5 global ones, the 4 bands, then the tempo.
    pub fn at(&self, tv_k: i64, p: &crate::params::Params) -> [f32; N_FEAT] {
        let mut out = [0.0; N_FEAT];
        out[..N_GLOBAL].copy_from_slice(&self.features.at(tv_k));
        if let Some(x) = self.beat_position(tv_k, p) {
            out[N_GLOBAL + N_BANDS..].copy_from_slice(&beat_shapes(x));
        }
        if tv_k >= 0 {
            for (i, b) in p.bands.iter().enumerate() {
                let v = self.band(b);
                if !v.is_empty() {
                    let g = 10f32.powf(b.sens / 20.0);
                    out[N_GLOBAL + i] = (v[(tv_k as usize).min(v.len() - 1)] * g).min(1.0);
                }
            }
        }
        out
    }
}

/// What the player asks of the sound card. Only the latest request counts.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Play(f64),
    Stop,
    Seek(f64),
}

/// Seconds to a read position. Whatever comes in, the position stays a sane number.
fn to_frames(t: f64) -> f64 {
    const LONGEST: f64 = 1e7; // Almost four months.
    if t.is_finite() {
        t.clamp(0.0, LONGEST) * RATE as f64
    } else {
        0.0
    }
}

/// Where the sound is, as the card's callback last worked it out.
#[derive(Clone, Copy, Default)]
struct Clock {
    /// An instant, and the track time reaching the ear at that instant.
    anchor: Option<(Instant, f64)>,
    /// A jump under way: the clock already reads its destination.
    jump: Option<f64>,
}

impl Clock {
    /// Past this without news from the card, the clock is not to be trusted.
    const STALE_SECS: f64 = 0.5;
    /// Corrections below this are callback jitter, and are smoothed out.
    const JITTER_SECS: f64 = 0.03;
    const SMOOTHING: f64 = 0.1;

    fn read(&self, now: Instant) -> Option<f64> {
        if let Some(t) = self.jump {
            return Some(t);
        }
        let (at, t) = self.anchor?;
        let since = now.saturating_duration_since(at).as_secs_f64();
        (since < Self::STALE_SECS).then_some(t + since)
    }

    /// Takes in a new measure. The callback wakes up a little late by a varying amount, which
    /// would make the picture shake if followed as it is; a real jump is followed at once.
    fn update(&mut self, now: Instant, heard: f64) {
        let predicted = self.anchor.map(|(at, t)| t + now.saturating_duration_since(at).as_secs_f64());
        let t = match predicted {
            Some(p) if (heard - p).abs() < Self::JITTER_SECS => p + (heard - p) * Self::SMOOTHING,
            _ => heard,
        };
        self.anchor = Some((now, t));
    }
}

/// Reads the track straight from the sound card's callback, so the card never runs out of
/// samples, whatever the picture is doing. Starts, stops and jumps are short fades: no clicks.
///
/// It belongs to the callback alone. It never holds the track: the callback only borrows it for
/// the length of a buffer, so a multi-gigabyte buffer can't end up being freed on the audio thread.
struct Mixer {
    /// Read position, in 48 kHz frames.
    pos: f64,
    gain: f32,
    target: f32,
    /// A jump waiting for the fade-out to end.
    seek_to: Option<f64>,
    clock: Clock,
}

impl Mixer {
    const FADE_SECS: f32 = 0.005;
    /// A delay reported above this is a driver's mistake: the picture must not wait for it.
    const MAX_LATENCY: f64 = 1.0;

    fn new() -> Self {
        Self { pos: 0.0, gain: 0.0, target: 0.0, seek_to: None, clock: Clock::default() }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Play(t) => {
                let at = to_frames(t);
                if self.gain <= 0.0 {
                    self.pos = at;
                    self.seek_to = None;
                    self.target = 1.0;
                } else {
                    self.seek_to = Some(at);
                    self.target = 0.0;
                }
                self.clock.anchor = None;
            }
            Action::Stop => {
                self.target = 0.0;
                self.seek_to = None;
            }
            // Also while another jump is under way: the latest destination wins.
            Action::Seek(t) => {
                if self.target > 0.0 || self.seek_to.is_some() {
                    self.target = 0.0;
                    self.seek_to = Some(to_frames(t));
                    self.clock.anchor = None;
                }
            }
        }
    }

    /// At the start of a buffer: its first sample will reach the ear `latency` seconds from `now`.
    fn begin(&mut self, now: Instant, latency: f64) {
        self.clock.jump = self.seek_to.map(|t| t / RATE as f64);
        if self.seek_to.is_some() || self.target <= 0.0 {
            self.clock.anchor = None;
        } else {
            let latency = if latency.is_finite() { latency.clamp(0.0, Self::MAX_LATENCY) } else { 0.0 };
            self.clock.update(now, self.pos / RATE as f64 - latency);
        }
    }

    /// The stereo frame at this position (between two samples, a straight line).
    fn sample(pcm: &[f32], pos: f64) -> (f32, f32) {
        let i = pos as usize;
        let Some(s) = i.checked_mul(2).and_then(|a| pcm.get(a..a.checked_add(4)?)) else { return (0.0, 0.0) };
        let f = (pos - i as f64) as f32;
        (s[0] + (s[2] - s[0]) * f, s[1] + (s[3] - s[1]) * f)
    }

    fn fill(&mut self, pcm: &[f32], out: &mut [f32], channels: usize, rate: u32) {
        let step = RATE as f64 / rate as f64;
        let fade = 1.0 / (Self::FADE_SECS * rate as f32);
        for frame in out.chunks_exact_mut(channels) {
            if self.gain <= 0.0 {
                if let Some(t) = self.seek_to.take() {
                    self.pos = t;
                    self.target = 1.0;
                }
            }
            self.gain = if self.target > self.gain { (self.gain + fade).min(self.target) } else { (self.gain - fade).max(self.target) };
            let (l, r) = if self.gain > 0.0 { Self::sample(pcm, self.pos) } else { (0.0, 0.0) };
            put_frame(frame, (l * self.gain).clamp(-1.0, 1.0), (r * self.gain).clamp(-1.0, 1.0));
            if self.gain > 0.0 || self.target > 0.0 {
                self.pos += step;
            }
        }
    }
}

/// Stereo onto any layout: mono gets both sides, and on a surround device the front pair plays
/// (its other channels are centre, sub and rears, which must not get a copy of one side).
fn put_frame(frame: &mut [f32], l: f32, r: f32) {
    match frame {
        [] => {}
        [m] => *m = 0.5 * (l + r),
        [a, b, rest @ ..] => {
            *a = l;
            *b = r;
            rest.fill(0.0);
        }
    }
}

/// The player's side of a request, next to what it is known to have asked for.
#[derive(Default)]
struct Request {
    action: Option<Action>,
    playing: bool,
    /// Address of the sound in the card's slot (0 = none), to compare without touching it.
    sound_id: usize,
}

/// What the player thread and the card's callback share. The callback only ever uses `try_lock`
/// and atomics, so it can't be held up by the player: if a lock is busy it picks the news up on
/// the next buffer.
struct Link {
    request: Mutex<Request>,
    sound: Mutex<Option<Arc<Sound>>>,
    clock: Mutex<Clock>,
    born: Instant,
    /// Milliseconds after `born` of the latest callback.
    last_beat: AtomicU64,
    audible: AtomicBool,
    gone: AtomicBool,
    error_logged: AtomicBool,
}

impl Link {
    /// Without a callback for this long, the card is considered gone.
    const LOST_MS: u64 = 2000;
    /// The callback of a dead device can be retried in a tight loop.
    const ERROR_STORM_MS: u64 = 200;

    fn new() -> Self {
        Self {
            request: Mutex::default(),
            sound: Mutex::default(),
            clock: Mutex::default(),
            born: Instant::now(),
            last_beat: AtomicU64::new(0),
            audible: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            error_logged: AtomicBool::new(false),
        }
    }

    fn silent_ms(&self) -> u64 {
        (self.born.elapsed().as_millis() as u64).saturating_sub(self.last_beat.load(Ordering::Relaxed))
    }

    /// The callback's whole job. No allocation, no blocking, and no `Arc` released here.
    #[allow(clippy::too_many_arguments)]
    fn render<T: cpal::Sample + cpal::FromSample<f32>>(
        &self,
        mixer: &mut Mixer,
        scratch: &mut [f32],
        out: &mut [T],
        channels: usize,
        rate: u32,
        now: Instant,
        latency: f64,
    ) {
        self.last_beat.store(now.saturating_duration_since(self.born).as_millis() as u64, Ordering::Relaxed);
        // The request stays locked until the clock is out, so that the player never sees a
        // request gone with the clock still reading the old position.
        let mut request = self.request.try_lock();
        if let Ok(r) = &mut request {
            if let Some(a) = r.action.take() {
                mixer.apply(a);
            }
        }
        mixer.begin(now, latency);
        if let Ok(mut c) = self.clock.try_lock() {
            *c = mixer.clock;
        }
        drop(request);
        {
            // Busy only while the player swaps tracks, which it does after fading to silence.
            let held = self.sound.try_lock();
            let pcm: &[f32] = match &held {
                Ok(s) => s.as_ref().map_or(&[], |s| &s.track.pcm),
                Err(_) => &[],
            };
            for chunk in out.chunks_mut(scratch.len()) {
                let part = &mut scratch[..chunk.len()];
                mixer.fill(pcm, part, channels, rate);
                for (o, v) in chunk.iter_mut().zip(part.iter()) {
                    *o = T::from_sample(*v);
                }
            }
        }
        self.audible.store(mixer.gain > 0.0, Ordering::Relaxed);
    }

    fn on_error(&self, e: cpal::StreamError) {
        if matches!(e, cpal::StreamError::DeviceNotAvailable) {
            self.gone.store(true, Ordering::Relaxed);
        }
        if !self.error_logged.swap(true, Ordering::Relaxed) {
            eprintln!("sound output: {e}");
        }
        // On a dead device the backend calls this in a loop: don't let it eat a whole core.
        if self.silent_ms() > Self::ERROR_STORM_MS {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The sound card. Silent if there is none.
pub struct Output {
    link: Arc<Link>,
    stream: Option<cpal::Stream>,
}

impl Output {
    /// Longest wait for a fade-out before swapping tracks.
    const QUIET_WAIT: Duration = Duration::from_millis(40);

    pub fn open() -> Self {
        let link = Arc::new(Link::new());
        let stream = match start_stream(link.clone()) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("sound output: {e:#}");
                None
            }
        };
        Self { link, stream }
    }

    /// Plays from `t` seconds, unless this sound is already playing.
    pub fn play(&self, sound: &Arc<Sound>, t: f64) {
        let id = Arc::as_ptr(sound) as usize;
        let same = {
            let r = lock(&self.link.request);
            if r.sound_id == id && r.playing {
                return;
            }
            r.sound_id == id
        };
        if !same {
            self.swap_sound(Some(sound.clone()));
        }
        let mut r = lock(&self.link.request);
        r.sound_id = id;
        r.playing = true;
        r.action = Some(Action::Play(t));
    }

    pub fn stop(&self) {
        let mut r = lock(&self.link.request);
        r.playing = false;
        r.action = Some(Action::Stop);
    }

    pub fn seek(&self, t: f64) {
        let mut r = lock(&self.link.request);
        if r.playing {
            r.action = Some(match r.action {
                Some(Action::Play(_)) => Action::Play(t),
                _ => Action::Seek(t),
            });
        }
    }

    /// Stops and lets go of the track, so that its memory can be reused.
    pub fn release(&self) {
        self.swap_sound(None);
        lock(&self.link.request).sound_id = 0;
    }

    /// Fades out, then changes the track. The old one is freed here, on the player's thread.
    fn swap_sound(&self, next: Option<Arc<Sound>>) {
        self.stop();
        if self.stream.is_some() {
            let t0 = Instant::now();
            while self.link.audible.load(Ordering::Relaxed) && t0.elapsed() < Self::QUIET_WAIT {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let old = std::mem::replace(&mut *lock(&self.link.sound), next);
        drop(old);
    }

    /// The sound's clock, which the picture follows. None without a working sound card.
    pub fn time(&self) -> Option<f64> {
        self.stream.as_ref()?;
        if self.link.silent_ms() as f64 > Clock::STALE_SECS * 1000.0 {
            return None;
        }
        match lock(&self.link.request).action {
            Some(Action::Stop) => return None,
            Some(Action::Play(t) | Action::Seek(t)) => return Some(t),
            None => {}
        }
        lock(&self.link.clock).read(Instant::now())
    }

    /// No working card: none was found, or it stopped answering.
    pub fn lost(&self) -> bool {
        self.stream.is_none() || self.link.gone.load(Ordering::Relaxed) || self.link.silent_ms() > Link::LOST_MS
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // Closing a dead stream waits for a backend thread that may be retrying forever: leave
        // that thread behind, but not the track it could still reach.
        if self.stream.is_some() && self.lost() {
            drop(lock(&self.link.sound).take());
            std::mem::forget(self.stream.take());
        }
    }
}

/// Frames converted at a time: a fixed buffer, so the callback never allocates.
const SCRATCH_FRAMES: usize = 4096;

fn start_stream(link: Arc<Link>) -> Result<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat as F;

    fn build<T>(dev: &cpal::Device, config: &cpal::StreamConfig, link: Arc<Link>) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static,
    {
        let channels = (config.channels as usize).max(1);
        let rate = config.sample_rate.0.max(1);
        let (mut mixer, mut scratch) = (Mixer::new(), vec![0.0f32; SCRATCH_FRAMES * channels]);
        let errors = link.clone();
        let stream = dev.build_output_stream(
            config,
            move |d: &mut [T], info: &cpal::OutputCallbackInfo| {
                let t = info.timestamp();
                let latency = t.playback.duration_since(&t.callback).map_or(0.0, |l| l.as_secs_f64());
                link.render(&mut mixer, &mut scratch, d, channels, rate, Instant::now(), latency);
            },
            move |e| errors.on_error(e),
            None,
        )?;
        Ok(stream)
    }

    let host = cpal::default_host();
    let dev = host.default_output_device().context("no sound output")?;
    let cfg = dev.default_output_config()?;
    let fmt = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    let stream = match fmt {
        F::F32 => build::<f32>(&dev, &config, link)?,
        F::F64 => build::<f64>(&dev, &config, link)?,
        F::I8 => build::<i8>(&dev, &config, link)?,
        F::I16 => build::<i16>(&dev, &config, link)?,
        F::I32 => build::<i32>(&dev, &config, link)?,
        F::I64 => build::<i64>(&dev, &config, link)?,
        F::U8 => build::<u8>(&dev, &config, link)?,
        F::U16 => build::<u16>(&dev, &config, link)?,
        F::U32 => build::<u32>(&dev, &config, link)?,
        F::U64 => build::<u64>(&dev, &config, link)?,
        other => bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Params;

    /// A kick on each beat, a quieter hat between them, over a steady tone.
    fn beat_track(bpm: f64, secs: f64, first: f64) -> Track {
        let n = (RATE as f64 * secs) as usize;
        let gap = 60.0 / bpm;
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            let since = (t - first).rem_euclid(gap);
            let kick = if t >= first { (std::f64::consts::TAU * 70.0 * since).sin() * (-30.0 * since).exp() * 0.8 } else { 0.0 };
            let off = (t - first - gap / 2.0).rem_euclid(gap);
            let hat = if t >= first + gap / 2.0 { (std::f64::consts::TAU * 6000.0 * t).sin() * (-120.0 * off).exp() * 0.15 } else { 0.0 };
            let pad = (std::f64::consts::TAU * 330.0 * t).sin() * 0.05;
            let x = (kick + hat + pad) as f32;
            pcm.push(x);
            pcm.push(x);
        }
        track(pcm)
    }

    fn track(pcm: Vec<f32>) -> Track {
        Track { pcm, truncated: false }
    }

    #[test]
    fn tempo_is_found_and_lands_on_the_beats() {
        for bpm in [90.0, 128.0, 174.0] {
            let first = 0.3;
            let g = BeatGrid::analyse(&beat_track(bpm, 20.0, first)).expect("a beat");
            // 174 may legitimately read as its half time.
            let found = if (g.bpm * 2.0 - bpm).abs() < (g.bpm - bpm).abs() { g.bpm * 2.0 } else { g.bpm };
            assert!((found - bpm).abs() < 1.0, "{bpm} BPM read as {}", g.bpm);
            // The beat pulse must be strong just after a kick, weak just before the next one.
            let gap = 60.0 / g.bpm;
            let (mut on, mut off) = (0.0, 0.0);
            let n = 16;
            for b in 8..8 + n {
                let t = first + b as f64 * gap;
                on += g.measures((t * TV_FPS).ceil() as i64, 0, 0.0)[1];
                off += g.measures(((t - 0.12 * gap) * TV_FPS) as i64, 0, 0.0)[1];
            }
            let (on, off) = (on / n as f32, off / n as f32);
            assert!(on > 0.45 && off < 0.2, "{bpm} BPM: pulse {on:.2} on the beat, {off:.2} before");
        }
    }

    #[test]
    fn no_tempo_in_silence_or_a_steady_tone() {
        assert!(BeatGrid::analyse(&track(vec![0.0; RATE as usize * 2 * 10])).is_none());
        let tone: Vec<f32> = (0..RATE as usize * 10)
            .flat_map(|i| {
                let x = (std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin() * 0.5;
                [x, x]
            })
            .collect();
        assert!(BeatGrid::analyse(&track(tone)).is_none());
    }

    fn tone() -> Arc<Sound> {
        let pcm: Vec<f32> = (0..RATE as usize)
            .flat_map(|i| {
                let x = (std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin() * 0.5;
                [x, x]
            })
            .collect();
        let track = super::tests::track(pcm);
        let features = Features { rows: Vec::new() };
        let spectrum = Spectrum { rows: Vec::new(), frames: 0 };
        Arc::new(Sound { track, features, spectrum, tempo: None, bands: Mutex::new(HashMap::new()) })
    }

    /// The biggest jump between two samples, however the card slices its buffers.
    fn biggest_step(m: &mut Mixer, pcm: &[f32], rate: u32, total: usize) -> f32 {
        let (mut out, mut last, mut max) = (Vec::new(), None::<f32>, 0.0f32);
        let mut left = total;
        for size in [64usize, 333, 1024, 7, 480].iter().cycle() {
            if left == 0 {
                break;
            }
            let n = (*size).min(left);
            left -= n;
            out.resize(n * 2, 0.0);
            m.fill(pcm, &mut out, 2, rate);
            for f in out.chunks_exact(2) {
                if let Some(l) = last {
                    max = max.max((f[0] - l).abs());
                }
                last = Some(f[0]);
            }
        }
        max
    }

    #[test]
    fn the_sound_has_no_jump_across_buffers() {
        // A 440 Hz sine at 0.5 moves by at most 0.029 a sample at 48 kHz, 0.032 at 44.1 kHz.
        let snd = tone();
        for (rate, limit) in [(48_000, 0.035), (44_100, 0.038)] {
            let mut m = Mixer::new();
            m.apply(Action::Play(0.1));
            let step = biggest_step(&mut m, &snd.track.pcm, rate, 20_000);
            assert!(step < limit, "{rate} Hz: jump of {step}");
        }
    }

    #[test]
    fn stop_and_jump_are_faded() {
        let snd = tone();
        let pcm = &snd.track.pcm;
        let mut m = Mixer::new();
        m.apply(Action::Play(0.1));
        biggest_step(&mut m, pcm, 48_000, 2_000);
        m.apply(Action::Seek(0.6));
        assert!(biggest_step(&mut m, pcm, 48_000, 2_000) < 0.2, "the jump clicks");
        assert_eq!(m.seek_to, None);
        m.apply(Action::Stop);
        assert!(biggest_step(&mut m, pcm, 48_000, 2_000) < 0.2, "the stop clicks");
        assert_eq!(m.gain, 0.0);
        m.begin(Instant::now(), 0.0);
        assert_eq!(m.clock.read(Instant::now()), None);
    }

    #[test]
    fn a_second_jump_replaces_the_one_under_way() {
        let snd = tone();
        let mut m = Mixer::new();
        m.apply(Action::Play(0.0));
        biggest_step(&mut m, &snd.track.pcm, 48_000, 1_000);
        m.apply(Action::Seek(0.2));
        m.apply(Action::Seek(0.7));
        assert_eq!(m.seek_to, Some(0.7 * RATE as f64));
        // A stopped mixer doesn't start again because of a jump.
        m.apply(Action::Stop);
        m.apply(Action::Seek(0.3));
        assert_eq!((m.seek_to, m.target), (None, 0.0));
    }

    #[test]
    fn absurd_positions_read_silence() {
        let snd = tone();
        for t in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -5.0, 1e300, 1e9] {
            let mut m = Mixer::new();
            m.apply(Action::Play(t));
            let mut out = [0.0f32; 512];
            m.fill(&snd.track.pcm, &mut out, 2, 48_000);
            assert!(out.iter().all(|v| v.is_finite()), "{t}");
            assert!(m.pos.is_finite(), "{t}");
        }
        assert_eq!(Mixer::sample(&[0.1, 0.2], 0.0), (0.0, 0.0));
        assert_eq!(Mixer::sample(&[], 5.0), (0.0, 0.0));
        assert_eq!(Mixer::sample(&[0.0; 8], usize::MAX as f64), (0.0, 0.0));
    }

    #[test]
    fn the_track_can_end_mid_buffer() {
        let mut m = Mixer::new();
        m.apply(Action::Play(0.0));
        let pcm = vec![0.5f32; 200];
        let mut out = vec![1.0f32; 1000];
        m.fill(&pcm, &mut out, 2, 48_000);
        assert_eq!(&out[900..], &[0.0; 100]);
    }

    #[test]
    fn stereo_goes_onto_any_layout() {
        let mut f = [9.0; 1];
        put_frame(&mut f, 1.0, 0.0);
        assert_eq!(f, [0.5]);
        let mut f = [9.0; 2];
        put_frame(&mut f, 0.25, -0.25);
        assert_eq!(f, [0.25, -0.25]);
        let mut f = [9.0; 6];
        put_frame(&mut f, 0.25, -0.25);
        assert_eq!(f, [0.25, -0.25, 0.0, 0.0, 0.0, 0.0]);
    }

    /// One callback, as the card would make it, without a card.
    fn beat<T: cpal::Sample + cpal::FromSample<f32>>(l: &Link, m: &mut Mixer, out: &mut [T], channels: usize, now: Instant) {
        let mut scratch = vec![0.0f32; 64 * channels];
        l.render(m, &mut scratch, out, channels, 48_000, now, 0.02);
    }

    fn quiet_output() -> Output {
        Output { link: Arc::new(Link::new()), stream: None }
    }

    #[test]
    fn every_sample_format_gets_the_signal() {
        fn peak<T: cpal::SizedSample + cpal::FromSample<f32> + Into<f64>>(mid: f64, full: f64) {
            let o = quiet_output();
            let pcm = vec![1.0f32; 48_000];
            *lock(&o.link.sound) = Some(sound_of(pcm));
            lock(&o.link.request).action = Some(Action::Play(0.0));
            let mut m = Mixer::new();
            let mut out = vec![T::EQUILIBRIUM; 2000];
            beat(&o.link, &mut m, &mut out, 2, Instant::now());
            let last: f64 = out[1999].into();
            assert!(((last - mid) / (full - mid) - 1.0).abs() < 0.01, "{:?}: {last}", T::FORMAT);
        }
        peak::<f32>(0.0, 1.0);
        peak::<f64>(0.0, 1.0);
        peak::<i8>(0.0, 127.0);
        peak::<i16>(0.0, 32767.0);
        peak::<i32>(0.0, 2147483647.0);
        peak::<u8>(128.0, 255.0);
        peak::<u16>(32768.0, 65535.0);
        peak::<u32>(2147483648.0, 4294967295.0);
    }

    fn sound_of(pcm: Vec<f32>) -> Arc<Sound> {
        let track = track(pcm);
        let features = Features { rows: Vec::new() };
        let spectrum = Spectrum { rows: Vec::new(), frames: 0 };
        Arc::new(Sound { track, features, spectrum, tempo: None, bands: Mutex::new(HashMap::new()) })
    }

    #[test]
    fn buffers_larger_than_the_scratch_are_whole() {
        let o = quiet_output();
        *lock(&o.link.sound) = Some(sound_of(vec![0.5; 96_000]));
        lock(&o.link.request).action = Some(Action::Play(0.0));
        let mut m = Mixer::new();
        let mut out = vec![0.0f32; 2 * 1000];
        beat(&o.link, &mut m, &mut out, 2, Instant::now());
        assert!((m.pos - 1000.0).abs() < 1e-9);
        assert!((out[1999] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_callback_gets_past_busy_locks() {
        let o = quiet_output();
        let snd = sound_of(vec![0.5; 9600]);
        *lock(&o.link.sound) = Some(snd);
        let mut m = Mixer::new();
        m.apply(Action::Play(0.0));
        let held = (o.link.request.lock().unwrap(), o.link.sound.lock().unwrap(), o.link.clock.lock().unwrap());
        let mut out = [1.0f32; 128];
        beat(&o.link, &mut m, &mut out, 2, Instant::now());
        assert!(out.iter().all(|v| *v == 0.0), "a busy track plays as silence");
        drop(held);
    }

    #[test]
    fn the_callback_never_frees_a_track() {
        let o = quiet_output();
        let (a, b) = (sound_of(vec![0.5; 9600]), sound_of(vec![0.25; 9600]));
        let weak = Arc::downgrade(&a);
        o.play(&a, 0.0);
        let mut m = Mixer::new();
        let mut out = [0.0f32; 128];
        beat(&o.link, &mut m, &mut out, 2, Instant::now());
        assert_eq!(Arc::strong_count(&a), 2, "the callback takes no reference of its own");
        // Swapping, then dropping the player's handle, frees it on this thread at once.
        o.play(&b, 0.0);
        drop(a);
        assert!(weak.upgrade().is_none());
        beat(&o.link, &mut m, &mut out, 2, Instant::now());
        o.release();
        assert_eq!(Arc::strong_count(&b), 1);
    }

    #[test]
    fn requests_keep_only_the_latest() {
        let o = quiet_output();
        let snd = sound_of(vec![0.5; 9600]);
        o.seek(3.0); // Nothing is playing.
        assert_eq!(lock(&o.link.request).action, None);
        o.play(&snd, 1.0);
        o.play(&snd, 2.0); // Already playing: a frame's call changes nothing.
        assert_eq!(lock(&o.link.request).action, Some(Action::Play(1.0)));
        o.seek(0.5);
        assert_eq!(lock(&o.link.request).action, Some(Action::Play(0.5)));
        o.stop();
        o.play(&snd, 0.25);
        assert_eq!(lock(&o.link.request).action, Some(Action::Play(0.25)));
    }

    #[test]
    fn no_card_means_no_clock() {
        let o = quiet_output();
        o.play(&sound_of(vec![0.5; 9600]), 1.0);
        assert_eq!(o.time(), None);
        assert!(o.lost());
    }

    #[test]
    fn the_clock_subtracts_the_delay_and_runs_on() {
        let mut m = Mixer::new();
        m.apply(Action::Play(1.0));
        let t0 = Instant::now();
        m.begin(t0, 0.1);
        let at = |d: f64| m.clock.read(t0 + Duration::from_secs_f64(d)).unwrap();
        assert!((at(0.0) - 0.9).abs() < 1e-9);
        assert!((at(0.2) - 1.1).abs() < 1e-9);
        assert_eq!(m.clock.read(t0 + Duration::from_secs_f64(0.6)), None, "stale");
    }

    #[test]
    fn the_clock_ignores_callback_jitter_and_follows_real_jumps() {
        let mut m = Mixer::new();
        m.apply(Action::Play(0.0));
        let t0 = Instant::now();
        let buffer = 1024usize;
        let (mut out, mut worst) = (vec![0.0f32; buffer * 2], 0.0f64);
        for n in 0..400 {
            // Woken up to 4 ms late, in a pattern that never settles.
            let late = [0.0, 0.004, 0.001, 0.003][n % 4];
            let ideal = n as f64 * buffer as f64 / 48_000.0;
            let now = t0 + Duration::from_secs_f64(ideal + late);
            m.begin(now, 0.02);
            m.fill(&[], &mut out, 2, 48_000);
            if n > 50 {
                worst = worst.max((m.clock.read(now).unwrap() - (ideal + late - 0.02 - 0.002)).abs());
            }
        }
        assert!(worst < 0.0026, "off by {worst}");
        // A real discontinuity is taken at once.
        let now = t0 + Duration::from_secs(9);
        m.pos = 5.0 * RATE as f64;
        m.begin(now, 0.0);
        assert!((m.clock.read(now).unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_silly_reported_delay_is_bounded() {
        for l in [f64::NAN, f64::INFINITY, 1e6, -3.0] {
            let mut m = Mixer::new();
            m.apply(Action::Play(10.0));
            let now = Instant::now();
            m.begin(now, l);
            let t = m.clock.read(now).unwrap();
            assert!((9.0..=10.0).contains(&t), "{l}: {t}");
        }
    }

    #[test]
    fn pcm_stream_is_read_whole_in_any_slices() {
        /// Hands out at most `n` bytes at a time, with interruptions.
        struct Drip<'a>(&'a [u8], usize, bool);
        impl Read for Drip<'_> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.2 = !self.2;
                if self.2 {
                    return Err(ErrorKind::Interrupted.into());
                }
                let n = self.1.min(b.len()).min(self.0.len());
                b[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let vals: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let bytes: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        for n in [1, 3, 4, 7, 1000, 100_000] {
            let (pcm, cut) = read_pcm(&mut Drip(&bytes, n, false), usize::MAX);
            assert_eq!((pcm, cut), (vals.clone(), false), "slices of {n}");
        }
        // A trailing half frame or half sample is dropped.
        let (pcm, _) = read_pcm(&mut &bytes[..4 * 3 + 2], usize::MAX);
        assert_eq!(pcm.len(), 2);
        // The memory limit stops it, and says so.
        let (pcm, cut) = read_pcm(&mut &bytes[..], 100);
        assert_eq!((pcm.len(), cut), (100, true));
        let (pcm, cut) = read_pcm(&mut &bytes[..400], 100);
        assert_eq!((pcm.len(), cut), (100, false));
    }

    #[test]
    fn bad_float_samples_are_cleaned() {
        let bytes: Vec<u8> = [f32::NAN, f32::INFINITY, -1e30, 0.5].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(read_pcm(&mut &bytes[..], usize::MAX).0, vec![0.0, 0.0, -MAX_SAMPLE, 0.5]);
    }

    #[test]
    fn free_memory_is_parsed() {
        assert_eq!(mem_available("MemTotal: 9 kB\nMemAvailable:    2048 kB\n"), Some(2048 * 1024));
        assert_eq!(mem_available("MemTotal: 9 kB\n"), None);
        assert_eq!(mem_available("MemAvailable: x kB"), None);
    }

    #[test]
    fn frame_boundaries_match_a_division_at_every_sample() {
        let frame_len = RATE as f64 / TV_FPS;
        let mut k = 0;
        for i in 0..2_000_000usize {
            let at = (i as f64 / frame_len) as usize;
            if at != k {
                assert_eq!(frame_end(k, frame_len), i, "frame {k}");
                k = at;
            }
        }
    }

    #[test]
    fn odd_tracks_never_panic_the_analysis() {
        let noise = |n: usize| -> Vec<f32> { (0..n).map(|i| ((i * 7919) % 1000) as f32 / 500.0 - 1.0).collect() };
        let mut clicks = vec![0.0f32; RATE as usize * 2 * 12];
        clicks.iter_mut().step_by(RATE as usize / 2).for_each(|v| *v = 1.0);
        let cases: Vec<Vec<f32>> = vec![
            vec![],
            vec![0.5],
            vec![0.5, -0.5],
            vec![0.1, 0.2, 0.3],
            vec![0.0; 958],
            noise(959),
            noise(960),
            noise(2 * 1024),
            // Just around the lengths at which the tempo search changes its mind.
            noise(2 * 480 * 479),
            noise(2 * 480 * 480),
            noise(2 * 480 * 481),
            vec![1.0; 96_000],
            vec![f32::NAN; 96_000],
            vec![f32::INFINITY; 96_000],
            clicks,
        ];
        let p = Params::default();
        for pcm in cases {
            let len = pcm.len();
            let track = track(pcm);
            let features = Features::analyse(&track);
            let spectrum = Spectrum::analyse(&track);
            let tempo = BeatGrid::analyse(&track);
            if let Some(g) = &tempo {
                assert!(g.bpm.is_finite() && g.bpm > 0.0, "{len} samples: {} BPM", g.bpm);
            }
            let snd = Sound { track, features, spectrum, tempo, bands: Mutex::new(HashMap::new()) };
            for k in [-1, 0, 1, 1_000_000] {
                snd.at(k, &p);
                snd.spectrum.row(k);
            }
        }
    }

    /// A drum pattern over noise and a pad: (beat in the bar, 0 kick / 1 snare / 2 hat).
    fn groove(pattern: &[(f64, u8)], bpm: f64, secs: f64) -> Track {
        let n = (RATE as f64 * secs) as usize;
        let mut mono = vec![0.0f32; n];
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let gap = 60.0 / bpm;
        for bar in 0..(secs / (4.0 * gap)) as usize + 1 {
            for &(beat, kind) in pattern {
                let a = ((0.2 + (bar as f64 * 4.0 + beat) * gap) * RATE as f64) as usize;
                for j in 0..RATE as usize / 5 {
                    let Some(v) = mono.get_mut(a + j) else { break };
                    let s = j as f32 / RATE as f32;
                    *v += match kind {
                        0 => (std::f32::consts::TAU * 65.0 * s).sin() * (-28.0 * s).exp() * 0.7,
                        1 => noise() * (-35.0 * s).exp() * 0.35,
                        _ => (std::f32::consts::TAU * 8000.0 * s).sin() * (-160.0 * s).exp() * 0.10,
                    };
                }
            }
        }
        for (i, v) in mono.iter_mut().enumerate() {
            *v += (std::f32::consts::TAU * 220.0 * i as f32 / RATE as f32).sin() * 0.04 + noise() * 0.008;
        }
        Track { pcm: mono.iter().flat_map(|&x| [x, x]).collect(), truncated: false }
    }

    #[test]
    fn tempo_bench() {
        const H: [(f64, u8); 8] = [(0.0, 2), (0.5, 2), (1.0, 2), (1.5, 2), (2.0, 2), (2.5, 2), (3.0, 2), (3.5, 2)];
        let mix = |extra: &[(f64, u8)]| -> Vec<(f64, u8)> { extra.iter().chain(H.iter()).copied().collect() };
        let styles: Vec<(&str, Vec<(f64, u8)>)> = vec![
            ("4x4", mix(&[(0.0, 0), (1.0, 0), (2.0, 0), (3.0, 0), (1.0, 1), (3.0, 1)])),
            ("rock", mix(&[(0.0, 0), (2.0, 0), (1.0, 1), (3.0, 1)])),
            ("break", mix(&[(0.0, 0), (0.75, 0), (2.5, 0), (1.0, 1), (3.0, 1), (1.75, 1)])),
            ("funk", mix(&[(0.0, 0), (1.5, 0), (2.25, 0), (1.0, 1), (3.0, 1), (3.75, 1)])),
            ("swing", vec![(0.0, 0), (2.0, 0), (1.0, 1), (3.0, 1), (0.0, 2), (0.67, 2), (1.0, 2), (1.67, 2), (2.0, 2), (2.67, 2), (3.0, 2), (3.67, 2)]),
        ];
        let (mut good, mut total, mut octave) = (0, 0, 0);
        for (name, pattern) in &styles {
            let mut line = format!("{name:6}");
            for bpm in [76.0, 90.0, 100.0, 116.0, 124.0, 128.0, 140.0, 174.0] {
                let found = BeatGrid::analyse(&groove(pattern, bpm, 30.0)).map_or(0.0, |g| g.bpm);
                let ok = (found - bpm).abs() < 0.5;
                let oct = [0.5, 2.0].iter().any(|m| (found * m - bpm).abs() < 0.5);
                // Drum & bass is commonly felt at half time: ÷2 / ×2 are there for that.
                assert!(ok || (bpm > 170.0 && oct), "{name} at {bpm} BPM read as {found:.1}");
                total += 1;
                good += ok as i32;
                octave += oct as i32;
                line += &format!(" {bpm:.0}:{found:.1}{}", if ok { "" } else if oct { "~" } else { "!" });
            }
            eprintln!("{line}");
        }
        eprintln!("exact {good}/{total}, double or half {octave}");
    }

    #[test]
    fn a_steady_track_gets_a_regular_grid_and_a_drifting_one_is_followed() {
        let gap = 60.0 / 128.0;
        let g = BeatGrid::analyse(&beat_track(128.0, 60.0, 0.3)).expect("a beat");
        assert!((g.bpm - 128.0).abs() < 0.05, "{}", g.bpm);
        let dp = g.pos[1000] - g.pos[100];
        assert!((dp - 900.0 / TV_FPS / gap).abs() < 0.03, "not regular");
        let (mut t, mut times) = (0.3, Vec::new());
        while t < 45.0 {
            times.push(t);
            t += 60.0 / (116.0 + t * 0.2);
        }
        let n = (RATE as f64 * 46.0) as usize;
        let mut mono = vec![0.0f32; n];
        for &k in &times {
            let a = (k * RATE as f64) as usize;
            for j in 0..RATE as usize / 5 {
                let s = j as f64 / RATE as f64;
                mono[a + j] += ((std::f64::consts::TAU * 70.0 * s).sin() * (-30.0 * s).exp() * 0.8) as f32;
            }
        }
        let g = BeatGrid::analyse(&Track { pcm: mono.iter().flat_map(|&x| [x, x]).collect(), truncated: false }).expect("a beat");
        let last = &times[times.len() - 12..times.len() - 1];
        let on: f32 = last.iter().map(|t| g.measures((t * TV_FPS).ceil() as i64, 0, 0.0)[1]).sum::<f32>() / last.len() as f32;
        assert!(on > 0.45, "lost the moving beat: {on:.2}");
    }

    #[test]
    fn an_alignment_lands_on_the_hit() {
        let mut onsets = vec![0.0f32; 2000];
        onsets[503] = 5.0;
        let pos = (0..600).map(|k| k as f64 / TV_FPS * 2.0 + 0.37).collect();
        let g = BeatGrid { bpm: 120.0, pos, onsets };
        // Pressed 30 ms before the hit at 5.03 s: that hit becomes the "1".
        let beat = |t: f64| g.position((t * TV_FPS).round() as i64, 0, 0.0, 5.0).unwrap();
        assert!(beat(5.03).abs() < 0.04, "{}", beat(5.03));
        assert!((beat(7.03) - 4.0).abs() < 0.04, "{}", beat(7.03));
        // Nothing to land on: it starts where it was pressed.
        assert!(g.position((12.0 * TV_FPS) as i64, 0, 0.0, 12.0).unwrap().abs() < 0.07);
    }

    /// Long breaks and hits moving to the off-beat used to make the tracker gain beats.
    #[test]
    fn a_long_steady_track_with_breaks_keeps_its_exact_tempo() {
        let gap = 60.0 / 77.0;
        let mut times = Vec::new();
        for b in 0..400 {
            let t = 0.4 + b as f64 * gap;
            match b / 32 % 4 {
                0 | 2 => times.push(t),
                1 => times.push(t + gap / 2.0), // The hits move to the off-beat.
                _ if b % 4 == 0 => times.push(t + gap * 0.75), // A sparse, syncopated break.
                _ => {}
            }
        }
        let secs = 0.4 + 400.0 * gap + 1.0;
        let n = (RATE as f64 * secs) as usize;
        let mut mono = vec![0.0f32; n];
        for &k in &times {
            let a = (k * RATE as f64) as usize;
            for j in 0..RATE as usize / 5 {
                let s = j as f64 / RATE as f64;
                mono[a + j] += ((std::f64::consts::TAU * 70.0 * s).sin() * (-30.0 * s).exp() * 0.8) as f32;
            }
        }
        let g = BeatGrid::analyse(&Track { pcm: mono.iter().flat_map(|&x| [x, x]).collect(), truncated: false }).expect("a beat");
        assert!((g.bpm - 77.0).abs() < 0.02, "read as {:.3} BPM", g.bpm);
    }

    #[test]
    fn a_tempo_that_slips_late_in_a_long_track_is_followed() {
        // 100 s steady, then a slightly faster last part: a single straight line would slide off.
        let (mut t, mut times) = (0.3, Vec::new());
        while t < 120.0 {
            times.push(t);
            t += 60.0 / if t < 100.0 { 124.0 } else { 124.8 };
        }
        let n = (RATE as f64 * 121.0) as usize;
        let mut mono = vec![0.0f32; n];
        for &k in &times {
            let a = (k * RATE as f64) as usize;
            for j in 0..RATE as usize / 5 {
                let s = j as f64 / RATE as f64;
                mono[a + j] += ((std::f64::consts::TAU * 70.0 * s).sin() * (-30.0 * s).exp() * 0.8) as f32;
            }
        }
        let g = BeatGrid::analyse(&Track { pcm: mono.iter().flat_map(|&x| [x, x]).collect(), truncated: false }).expect("a beat");
        let last = &times[times.len() - 13..times.len() - 1];
        let on: f32 = last.iter().map(|t| g.measures((t * TV_FPS).ceil() as i64, 0, 0.0)[1]).sum::<f32>() / last.len() as f32;
        assert!(on > 0.45, "slid off the beat at the end: pulse {on:.2}");
    }

    #[test]
    fn half_and_double_time_and_offset() {
        let g = BeatGrid { bpm: 120.0, pos: (0..64).map(|k| k as f64 / 8.0).collect(), onsets: Vec::new() };
        // Frame 8 is beat 1: on the beat, but not on the 2-beat pulse.
        assert!(g.measures(8, 0, 0.0)[1] > 0.99);
        assert!(g.measures(8, 0, 0.0)[2] < 0.05);
        // Moved by one beat, it is. Halved, a beat is every 2 of the old ones.
        assert!(g.measures(8, 0, 1.0)[2] > 0.99);
        assert!(g.measures(8, -1, 0.0)[1] < 0.2);
        assert!(g.measures(16, -1, 0.0)[1] > 0.99);
    }

    #[test]
    fn bands_follow_their_frequencies() {
        let secs = 3.0;
        let n = (RATE as f64 * secs) as usize;
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let kick = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * (-12.0 * (t % 0.5)).exp() * 0.8;
            let hat = (2.0 * std::f32::consts::PI * 10000.0 * t).sin() * 0.2;
            pcm.push(kick + hat);
            pcm.push(kick + hat);
        }
        let track = super::tests::track(pcm);
        let features = Features::analyse(&track);
        let spectrum = Spectrum::analyse(&track);
        let snd = Sound { track, features, spectrum, tempo: None, bands: Mutex::new(HashMap::new()) };
        let fr = |t: f64| (t * TV_FPS) as i64;
        let p = Params::default();
        let at = |t: f64| snd.at(fr(t), &p);
        // Kick band: high right after a kick, low just before the next one.
        let on = at(1.02)[N_GLOBAL];
        let off = at(1.45)[N_GLOBAL];
        assert!(on > 0.6 && off < 0.3, "kick band on {on} off {off}");
        // Hi-hat band: steady and high. Voice band: nearly silent.
        let hat = at(1.25)[N_GLOBAL + 3];
        let voice = at(1.25)[N_GLOBAL + 2];
        assert!(hat > 0.7, "hat band {hat}");
        eprintln!("kick on {on:.2} off {off:.2}, hat {hat:.2}, voice {voice:.2}");
    }
}
