//! Video loops laid over the picture: before the circuit (so every effect works on them) or on
//! top of the finished picture.

use super::signal::{H, TV_FPS, W};
use crate::params::Params;
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const N_OVERLAYS: usize = 3;
/// Longest loop kept in memory, and its largest side.
pub const MAX_SECONDS: f64 = 20.0;
const MAX_SIDE: u32 = 384;

pub const PLACES: [&str; 2] = ["Dans le circuit", "Par-dessus"];
pub const MODES: [&str; 6] = ["Normal", "Écran", "Addition", "Multiplier", "Incrustation du noir", "Incrustation du blanc"];

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
    frames: Vec<Vec<u8>>,
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
        let n = w * h * 4;
        let frames: Vec<Vec<u8>> = bytes.chunks_exact(n).map(|c| c.to_vec()).collect();
        if frames.is_empty() {
            bail!("{}: no picture could be read", path.display());
        }
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self { path: path.to_path_buf(), name, start, end, duration, w, h, frames })
    }

    fn frame(&self, tv_k: i64, speed: f32) -> &[u8] {
        let n = self.frames.len() as i64;
        let i = ((tv_k as f64 * speed as f64).floor() as i64).rem_euclid(n);
        &self.frames[i as usize]
    }
}

/// Lays every overlay of the given placement over an RGB (0..1) 768 × 480 picture.
pub fn composite(rgb: &mut [f32], clips: &[Option<std::sync::Arc<Clip>>; N_OVERLAYS], p: &Params, place: u8, tv_k: i64) {
    for (s, clip) in clips.iter().enumerate() {
        let Some(clip) = clip else { continue };
        let o = &p.overlays[s];
        if o.place != place || o.opacity <= 0.0 || o.size <= 0.0 {
            continue;
        }
        let img = clip.frame(tv_k, o.speed);
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
        let mode = o.mode;
        rgb.par_chunks_mut(W * 3).enumerate().for_each(|(y, row)| {
            if !ys.contains(&y) {
                return;
            }
            let v = ((y as f32 + 0.5 - y0) / dh * ch as f32 - 0.5).clamp(0.0, (ch - 1) as f32);
            for x in xs.clone() {
                let u = ((x as f32 + 0.5 - x0) / dw * cw as f32 - 0.5).clamp(0.0, (cw - 1) as f32);
                let px = sample(img, cw, ch, u, v);
                let lum = 0.299 * px[0] + 0.587 * px[1] + 0.114 * px[2];
                let mut a = px[3] * opacity;
                match mode {
                    4 => a *= smooth(0.04, 0.18, lum),
                    5 => a *= 1.0 - smooth(0.82, 0.96, lum),
                    _ => {}
                }
                if a <= 0.0 {
                    continue;
                }
                for c in 0..3 {
                    let d = &mut row[3 * x + c];
                    let s = px[c];
                    let blended = match mode {
                        1 => 1.0 - (1.0 - *d) * (1.0 - s),
                        2 => (*d + s).min(1.0),
                        3 => *d * s,
                        _ => s,
                    };
                    *d += (blended - *d) * a;
                }
            }
        });
    }
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
