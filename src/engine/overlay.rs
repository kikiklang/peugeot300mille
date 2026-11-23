//! Video loops laid over the picture: before the circuit (so every effect works on them) or on
//! top of the finished picture.

use super::signal::{rgb_of, smoothstep, yiq_of, H, TV_FPS, W};
use crate::params::{Overlay, Params};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const N_OVERLAYS: usize = 8;
/// Longest loop kept in memory, and its largest side.
pub const MAX_SECONDS: f64 = 20.0;
const MAX_SIDE: u32 = 384;

pub const PLACES: [&str; 2] = ["Dans le circuit", "Par-dessus"];
/// Blend modes. New ones are added at the end so saved settings keep their mode.
pub const MODES: [&str; 18] = [
    "Normal",
    "Écran",
    "Addition",
    "Multiplier",
    "Incrustation du noir",
    "Incrustation du blanc",
    "Soustraction",
    "Différence",
    "Superposition",
    "Lumière douce",
    "Lumière forte",
    "Éclaircir",
    "Assombrir",
    "Densité couleur −",
    "Densité couleur +",
    "Couleur",
    "Luminosité",
    "Incrustation du vert",
];

/// Help shown for each mode.
pub const MODE_HELP: [&str; 18] = [
    "La boucle recouvre l'image.",
    "Éclaircit : le noir de la boucle disparaît, le reste s'ajoute en douceur.",
    "Additionne la lumière : flashs, néons, particules.",
    "Assombrit : le blanc de la boucle disparaît.",
    "Le fond noir d'une boucle VJ disparaît (l'image garde ses couleurs).",
    "Le fond blanc disparaît.",
    "Retire la lumière de la boucle à l'image.",
    "Écart entre les deux : couleurs inversées là où la boucle est claire.",
    "Contraste : renforce les clairs et les sombres de l'image avec la boucle.",
    "Comme Superposition, en plus doux.",
    "Comme Superposition, en plus dur (c'est la boucle qui décide).",
    "Garde le plus clair des deux, couleur par couleur.",
    "Garde le plus sombre des deux.",
    "Éclaircit fortement là où la boucle est claire (brûlé).",
    "Assombrit fortement là où la boucle est sombre.",
    "Prend la couleur de la boucle, garde la lumière de l'image.",
    "Prend la lumière de la boucle, garde la couleur de l'image.",
    "Le fond vert (chroma key) disparaît.",
];

/// Loop lengths offered, in beats (0 = free).
pub fn loop_lengths(meter: u8) -> [u8; 6] {
    [0, 1, 2, meter, 2 * meter, 4 * meter]
}
/// Longest loop in beats: four bars of the longest meter.
pub const MAX_LOOP_BEATS: u8 = 28;

/// A short clip, decoded once at the TV's frame rate, RGBA.
pub struct Clip {
    pub path: PathBuf,
    pub name: String,
    /// The part of the file that loops, in seconds, and the file's length.
    pub start: f64,
    pub end: f64,
    pub duration: f64,
    pub w: usize,
    pub h: usize,
    /// Every image of the loop, one after the other.
    pixels: Vec<u8>,
    n_frames: usize,
}

impl Clip {
    /// The part of the file from `start` to `end` (at most [`MAX_SECONDS`] long).
    pub fn load_range(path: &Path, start: f64, end: Option<f64>) -> Result<Self> {
        let pr = crate::media::probe(path)?;
        let duration = if pr.duration > 0.0 { pr.duration } else { MAX_SECONDS };
        let start = start.clamp(0.0, (duration - 0.05).max(0.0));
        let end = end.unwrap_or(duration).min(duration).min(start + MAX_SECONDS).max(start + 0.05);
        let (sw, sh) = (pr.width.max(1) as f64, pr.height.max(1) as f64);
        let k = (MAX_SIDE as f64 / sw.max(sh)).min(1.0);
        let w = ((sw * k).round() as usize).max(2) & !1;
        let h = ((sh * k).round() as usize).max(2) & !1;
        let mut child = Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-ss", &format!("{start:.3}"), "-i"])
            .arg(path)
            .args([
                "-t",
                &format!("{:.3}", end - start),
                "-an",
                "-vf",
                &format!("fps={TV_FPS},scale={w}:{h}:flags=area,format=rgba"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("ffmpeg not found")?;
        let mut bytes = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut bytes)?;
        child.wait()?;
        // Kept in the buffer they were read into: a long loop is hundreds of megabytes, which
        // would otherwise exist twice while loading.
        let n_frames = bytes.len() / (w * h * 4);
        if n_frames == 0 {
            bail!("{}: no picture could be read", path.display());
        }
        bytes.truncate(n_frames * w * h * 4);
        bytes.shrink_to_fit();
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self { path: path.to_path_buf(), name, start, end, duration, w, h, pixels: bytes, n_frames })
    }

    fn frame(&self, tv_k: i64, o: &Overlay, beat: Option<f64>) -> &[u8] {
        let len = (self.end - self.start) * TV_FPS;
        let size = self.w * self.h * 4;
        let i = frame_index(self.n_frames as i64, len, tv_k, o, beat);
        &self.pixels[i * size..(i + 1) * size]
    }
}

/// for (`len` TV frames, not a whole number).
/// for (`len` TV frames, not a whole number), so a loop keeps its exact length.
fn frame_index(n: i64, len: f64, tv_k: i64, o: &Overlay, beat: Option<f64>) -> usize {
    // On the tempo, the loop fills its beats exactly and restarts on them: it can't drift.
    if let (Some(pos), true) = (beat, o.beats > 0) {
        let beats = o.beats as f64;
        let i = ((pos.rem_euclid(beats) / beats * n as f64).floor() as i64).clamp(0, n - 1);
        return (if o.speed < 0.0 { n - 1 - i } else { i }) as usize;
    }
    let whole = n as f64;
    // A file shorter than announced loops on the images it really has.
    let cycle = if len > 0.0 && (len - whole).abs() <= 1.5 { len } else { whole };
    let t = (tv_k as f64 * o.speed as f64).rem_euclid(cycle);
    (t.floor() as i64).clamp(0, n - 1) as usize
}

/// A coordinate along a side of `size` pixels, read from the other end if the loop is flipped.
fn flipped(c: f32, size: usize, flip: bool) -> f32 {
    if flip { (size - 1) as f32 - c } else { c }
}

/// Lays every overlay of the given placement over an RGB (0..1) 768 × 480 picture.
pub fn composite(rgb: &mut [f32], clips: &[Option<std::sync::Arc<Clip>>; N_OVERLAYS], p: &Params, place: u8, tv_k: i64, beat: Option<f64>) {
    for (s, clip) in clips.iter().enumerate() {
        let Some(clip) = clip else { continue };
        let o = &p.overlays[s];
        if o.place != place || o.opacity <= 0.0 || o.size <= 0.0 {
            continue;
        }
        let img = clip.frame(tv_k, o, beat);
        // Size is a fraction of the picture height; position is the centre, -1..1.
        let dh = (o.size * H as f32).max(2.0);
        let dw = dh * clip.w as f32 / clip.h as f32;
        let cx = (0.5 + 0.5 * o.x) * W as f32;
        let cy = (0.5 - 0.5 * o.y) * H as f32;
        let (x0, y0) = (cx - dw / 2.0, cy - dh / 2.0);
        let ys = (y0.floor().max(0.0) as usize)..((y0 + dh).ceil().min(H as f32) as usize);
        let xs = (x0.floor().max(0.0) as usize)..((x0 + dw).ceil().min(W as f32) as usize);
        if ys.is_empty() || xs.is_empty() {
            continue;
        }
        let (cw, ch) = (clip.w, clip.h);
        let opacity = o.opacity;
        let (mode, flip_h, flip_v) = (o.mode, o.flip_h, o.flip_v);
        let rows = &mut rgb[ys.start * W * 3..ys.end * W * 3];
        rows.par_chunks_mut(W * 3).zip(ys.clone()).for_each(|(row, y)| {
            let v = flipped(((y as f32 + 0.5 - y0) / dh * ch as f32 - 0.5).clamp(0.0, (ch - 1) as f32), ch, flip_v);
            for x in xs.clone() {
                let u = flipped(((x as f32 + 0.5 - x0) / dw * cw as f32 - 0.5).clamp(0.0, (cw - 1) as f32), cw, flip_h);
                let px = sample(img, cw, ch, u, v);
                let lum = 0.299 * px[0] + 0.587 * px[1] + 0.114 * px[2];
                let mut a = px[3] * opacity;
                match mode {
                    4 => a *= smoothstep(0.04, 0.18, lum),
                    5 => a *= 1.0 - smoothstep(0.82, 0.96, lum),
                    17 => {
                        // Green screen: how much greener than red and blue.
                        let g = px[1] - px[0].max(px[2]);
                        a *= 1.0 - smoothstep(0.08, 0.25, g);
                    }
                    _ => {}
                }
                if a <= 0.0 {
                    continue;
                }
                let base = [row[3 * x], row[3 * x + 1], row[3 * x + 2]];
                let blended: [f32; 3] = match mode {
                    15 | 16 => {
                        // Colour / luminosity: swap the light and the colour in YIQ.
                        let [yb, ib, qb] = yiq_of(base);
                        let [yo, io, qo] = yiq_of([px[0], px[1], px[2]]);
                        let (y, i, q) = if mode == 15 { (yb, io, qo) } else { (yo, ib, qb) };
                        rgb_of(y, i, q)
                    }
                    _ => std::array::from_fn(|c| blend(mode, base[c], px[c])),
                };
                for c in 0..3 {
                    let d = &mut row[3 * x + c];
                    *d += (blended[c].clamp(0.0, 1.0) - *d) * a;
                }
            }
        });
    }
}

/// One channel of a blend: `d` is the picture, `s` the loop.
#[inline]
fn blend(mode: u8, d: f32, s: f32) -> f32 {
    let overlay = |a: f32, b: f32| if a < 0.5 { 2.0 * a * b } else { 1.0 - 2.0 * (1.0 - a) * (1.0 - b) };
    match mode {
        1 => 1.0 - (1.0 - d) * (1.0 - s),
        2 => d + s,
        3 => d * s,
        6 => d - s,
        7 => (d - s).abs(),
        8 => overlay(d, s),
        9 => (1.0 - 2.0 * s) * d * d + 2.0 * s * d,
        10 => overlay(s, d),
        11 => d.max(s),
        12 => d.min(s),
        13 => d / (1.0 - s).max(0.02),
        14 => 1.0 - (1.0 - d) / s.max(0.02),
        _ => s,
    }
}

/// Bilinear RGBA sample, 0..1.
#[inline]
fn sample(img: &[u8], w: usize, h: usize, u: f32, v: f32) -> [f32; 4] {
    let x0 = u as usize;
    let y0 = v as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let (tx, ty) = (u - x0 as f32, v - y0 as f32);
    let at = |x: usize, y: usize, c: usize| img[(y * w + x) * 4 + c] as f32 * (1.0 / 255.0);
    let mut out = [0.0f32; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let a = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
        let b = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
        *o = a * (1.0 - ty) + b * ty;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_on_the_tempo_fills_its_beats() {
        let o = Overlay { beats: 4, ..Overlay::default() };
        assert_eq!(frame_index(100, 100.0, 7, &o, Some(0.0)), 0);
        assert_eq!(frame_index(100, 100.0, 7, &o, Some(2.0)), 50);
        assert_eq!(frame_index(100, 100.0, 7, &o, Some(3.999)), 99);
        assert_eq!(frame_index(100, 100.0, 7, &o, Some(4.0)), 0);
        assert_eq!(frame_index(100, 100.0, 7, &o, Some(-1.0)), 75);
        let back = Overlay { beats: 4, speed: -1.0, ..Overlay::default() };
        assert_eq!(frame_index(100, 100.0, 7, &back, Some(0.0)), 99);
        assert_eq!(frame_index(100, 100.0, 7, &o, None), 7, "without a tempo it runs free");
    }

    #[test]
    fn a_fractional_length_does_not_drift() {
        let o = Overlay::default();
        // 14.6 frames a cycle: after 1000 cycles the loop is still at its start.
        assert_eq!(frame_index(15, 14.6, 14_600, &o, None), 0);
        assert_eq!(frame_index(15, 14.6, 14_607, &o, None), 7);
    }

    #[test]
    fn speed_still_applies_and_a_wrong_length_is_ignored() {
        let o = Overlay { speed: 2.0, ..Overlay::default() };
        assert_eq!(frame_index(100, 100.0, 7, &o, None), 14);
        assert_eq!(frame_index(10, 600.0, 13, &Overlay::default(), None), 3);
    }
}
