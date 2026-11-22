//! The soundtrack: decoded once, analysed per TV frame (what drives the knobs), and played back
//! during the preview.

use crate::engine::signal::TV_FPS;
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Sources that can drive a knob.
pub const SOURCES: [&str; 10] =
    ["—", "Volume", "Basses", "Médiums", "Aigus", "Attaques", "Bande 1", "Bande 2", "Bande 3", "Bande 4"];
/// Sources offered in the menus: none, Volume, Attacks, Bands 1–4. (Bass, mid and treble are
/// still measured so that older saved settings keep working, but are no longer offered.)
pub const MENU_SOURCES: [u8; 7] = [0, 1, 5, 6, 7, 8, 9];

/// Measures per frame: the 5 global ones, then the 4 user bands (source `i` is measure `i - 1`).
pub const N_FEAT: usize = 9;
pub const N_GLOBAL: usize = 5;
pub const N_BANDS: usize = 4;

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

/// The decoded soundtrack, stereo interleaved, 48 kHz.
pub struct Track {
    pub pcm: Vec<f32>,
}

impl Track {
    pub fn decode(path: &Path) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-vn", "-ac", "2", "-ar", &RATE.to_string(), "-f", "f32le", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("ffmpeg not found")?;
        let mut bytes = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut bytes)?;
        child.wait()?;
        if bytes.is_empty() {
            bail!("no sound");
        }
        let pcm = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        Ok(Self { pcm })
    }

    /// Stereo samples (interleaved) for the time span [t0, t1).
    pub fn span(&self, t0: f64, t1: f64) -> &[f32] {
        let n = self.pcm.len() / 2;
        let a = ((t0 * RATE as f64).round().max(0.0) as usize).min(n);
        let b = ((t1 * RATE as f64).round().max(0.0) as usize).clamp(a, n);
        &self.pcm[a * 2..b * 2]
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
    s.sort_by(|a, b| a.total_cmp(b));
    let hi = s.get((s.len() as f32 * 0.95) as usize).copied().unwrap_or(1.0).max(floor);
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
        use rustfft::{num_complex::Complex, FftPlanner};
        let frame_len = RATE as f64 / TV_FPS;
        let n_samples = track.pcm.len() / 2;
        let frames = (n_samples as f64 / frame_len).ceil() as usize;
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_N);
        let window: Vec<f32> = (0..FFT_N)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_N as f32).cos())
            .collect();
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
            || vec![Complex::new(0.0f32, 0.0); FFT_N],
            |buf, (f, row)| {
                let centre = ((f as f64 + 0.5) * frame_len) as isize;
                for (i, c) in buf.iter_mut().enumerate() {
                    let j = centre - FFT_N as isize / 2 + i as isize;
                    let x = if j >= 0 && (j as usize) < n_samples {
                        let j = j as usize;
                        0.5 * (track.pcm[2 * j] + track.pcm[2 * j + 1])
                    } else {
                        0.0
                    };
                    *c = Complex::new(x * window[i], 0.0);
                }
                fft.process(buf);
                for (k, &b) in map.iter().enumerate() {
                    if b >= 0 {
                        row[b as usize] += buf[k].norm_sqr() * scale;
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
        let n_frames = (n_samples as f64 / frame_len).ceil() as usize;
        // Band split with one-pole filters: bass < 150 Hz, treble > 3 kHz, mid in between.
        let a = |fc: f64| (1.0 - (-2.0 * std::f64::consts::PI * fc / RATE as f64).exp()) as f32;
        let (a_lo, a_hi) = (a(150.0), a(3000.0));
        let (mut lo1, mut lo2, mut hi1) = (0.0f32, 0.0f32, 0.0f32);
        let mut raw = vec![[0.0f32; 4]; n_frames];
        let mut counts = vec![0u32; n_frames];
        for i in 0..n_samples {
            let x = 0.5 * (track.pcm[2 * i] + track.pcm[2 * i + 1]);
            lo1 += a_lo * (x - lo1);
            lo2 += a_lo * (lo1 - lo2);
            hi1 += a_hi * (x - hi1);
            let treble = x - hi1;
            let bass = lo2;
            let mid = hi1 - lo2;
            let k = ((i as f64 / frame_len) as usize).min(n_frames - 1);
            let r = &mut raw[k];
            r[0] += x * x;
            r[1] += bass * bass;
            r[2] += mid * mid;
            r[3] += treble * treble;
            counts[k] += 1;
        }
        for (r, &c) in raw.iter_mut().zip(&counts) {
            for v in r.iter_mut() {
                *v = (*v / c.max(1) as f32).sqrt();
            }
        }
        // Hits: rise of low+mid energy (log) above its recent average.
        let mut flux = vec![0.0f32; n_frames];
        let mut hist = VecDeque::new();
        for k in 0..n_frames {
            let e = ((raw[k][1] + raw[k][2]).max(1e-6)).ln();
            let avg = if hist.is_empty() { e } else { hist.iter().sum::<f32>() / hist.len() as f32 };
            flux[k] = (e - avg).max(0.0);
            hist.push_back(e);
            if hist.len() > 4 {
                hist.pop_front();
            }
        }
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

/// The soundtrack and its analysis, loaded once per file.
pub struct Sound {
    pub track: Track,
    pub features: Features,
    pub spectrum: Spectrum,
    bands: Mutex<HashMap<(u32, u32, u8), Arc<Vec<f32>>>>,
}

impl Sound {
    pub fn load(path: &Path) -> Result<Arc<Self>> {
        let track = Track::decode(path)?;
        let features = Features::analyse(&track);
        let spectrum = Spectrum::analyse(&track);
        Ok(Arc::new(Self { track, features, spectrum, bands: Mutex::new(HashMap::new()) }))
    }

    /// A user band's measure over the whole file (computed once per setting, then kept).
    fn band(&self, b: &crate::params::Band) -> Arc<Vec<f32>> {
        let key = (b.lo.round() as u32, b.hi.round() as u32, b.mode);
        if let Some(v) = self.bands.lock().unwrap().get(&key) {
            return v.clone();
        }
        let e = self.spectrum.band_energy(b.lo, b.hi);
        let v = Arc::new(if b.mode == 1 {
            normalise(&onsets(&e), 0.55)
        } else {
            // Level floor about -45 dBFS: an empty band stays at zero.
            normalise_floor(&e.iter().map(|x| x.sqrt()).collect::<Vec<_>>(), 0.75, 0.006)
        });
        let mut cache = self.bands.lock().unwrap();
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(key, v.clone());
        v
    }

    /// All measures for a TV frame: the 5 global ones, then the 4 bands.
    pub fn at(&self, tv_k: i64, bands: &[crate::params::Band; N_BANDS]) -> [f32; N_FEAT] {
        let mut out = [0.0; N_FEAT];
        out[..N_GLOBAL].copy_from_slice(&self.features.at(tv_k));
        if tv_k >= 0 {
            for (i, b) in bands.iter().enumerate() {
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

/// Plays samples handed to it through a short queue. Silent if there is no sound device.
pub struct Output {
    queue: Arc<Mutex<VecDeque<f32>>>,
    _stream: Option<cpal::Stream>,
    pub rate: u32,
}

impl Output {
    pub fn open() -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::with_capacity(RATE as usize)));
        let (stream, rate) = match start_stream(queue.clone()) {
            Ok((s, r)) => (Some(s), r),
            Err(_) => (None, RATE),
        };
        Self { queue, _stream: stream, rate }
    }

    /// Queues stereo 48 kHz samples; keeps at most ~150 ms queued so sound stays on the picture.
    pub fn push(&self, stereo48: &[f32]) {
        let mut q = self.queue.lock().unwrap();
        let ratio = self.rate as f64 / RATE as f64;
        if (ratio - 1.0).abs() < 1e-9 {
            q.extend(stereo48.iter().copied());
        } else {
            let n_in = stereo48.len() / 2;
            let n_out = (n_in as f64 * ratio) as usize;
            for i in 0..n_out {
                let j = ((i as f64 / ratio) as usize).min(n_in.saturating_sub(1));
                q.push_back(stereo48[2 * j]);
                q.push_back(stereo48[2 * j + 1]);
            }
        }
        let max = (self.rate as f32 * 0.15) as usize * 2;
        while q.len() > max {
            q.pop_front();
        }
    }

    pub fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }
}

fn start_stream(queue: Arc<Mutex<VecDeque<f32>>>) -> Result<(cpal::Stream, u32)> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let dev = host.default_output_device().context("no sound output")?;
    let cfg = dev.default_output_config()?;
    let rate = cfg.sample_rate().0;
    let channels = cfg.channels() as usize;
    let fmt = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    let err = |e| eprintln!("sound output: {e}");
    let pull = move |out: &mut dyn FnMut(usize, f32), frames: usize| {
        let mut q = queue.lock().unwrap();
        for f in 0..frames {
            let (l, r) = (q.pop_front().unwrap_or(0.0), q.pop_front().unwrap_or(0.0));
            for c in 0..channels {
                out(f * channels + c, if c % 2 == 0 { l } else { r });
            }
        }
    };
    let stream = match fmt {
        cpal::SampleFormat::F32 => dev.build_output_stream(
            &config,
            move |d: &mut [f32], _| {
                let n = d.len() / channels;
                pull(&mut |i, v| d[i] = v, n)
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_output_stream(
            &config,
            move |d: &mut [i16], _| {
                let n = d.len() / channels;
                pull(&mut |i, v| d[i] = (v.clamp(-1.0, 1.0) * 32767.0) as i16, n)
            },
            err,
            None,
        )?,
        other => bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;
    Ok((stream, rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::DEFAULT_BANDS;

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
        let track = Track { pcm };
        let features = Features::analyse(&track);
        let spectrum = Spectrum::analyse(&track);
        let snd = Sound { track, features, spectrum, bands: Mutex::new(HashMap::new()) };
        let fr = |t: f64| (t * TV_FPS) as i64;
        let at = |t: f64| snd.at(fr(t), &DEFAULT_BANDS);
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
