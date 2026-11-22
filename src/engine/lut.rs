//! 3D LUTs in the `.cube` format (Resolve / Adobe / color.io / Kdenlive).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub struct Lut {
    pub path: PathBuf,
    pub title: String,
    size: usize,
    /// RGB triplets, red varying fastest.
    data: Vec<[f32; 3]>,
    dmin: [f32; 3],
    dmax: [f32; 3],
}

impl Lut {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut size = 0usize;
        let mut title = String::new();
        let mut dmin = [0.0f32; 3];
        let mut dmax = [1.0f32; 3];
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let key = it.next().unwrap();
            match key {
                "TITLE" => title = line[5..].trim().trim_matches('"').to_string(),
                "LUT_3D_SIZE" => size = it.next().context("LUT_3D_SIZE")?.parse()?,
                "LUT_1D_SIZE" => bail!("1D LUTs are not supported, export a 3D LUT (.cube)"),
                "DOMAIN_MIN" => dmin = three(it)?,
                "DOMAIN_MAX" => dmax = three(it)?,
                k if k.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') => {
                    let r: f32 = k.parse()?;
                    let mut rest = it.map(|v| v.parse::<f32>());
                    let g = rest.next().context("bad LUT line")??;
                    let b = rest.next().context("bad LUT line")??;
                    data.push([r, g, b]);
                }
                _ => {} // other keywords (LUT_3D_INPUT_RANGE, …) are ignored
            }
        }
        if size < 2 || data.len() != size * size * size {
            bail!("{}: not a valid 3D .cube LUT ({} entries for size {size})", path.display(), data.len());
        }
        if title.is_empty() {
            title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        }
        Ok(Self { path: path.to_path_buf(), title, size, data, dmin, dmax })
    }

    /// Trilinear lookup of an RGB triplet (0..1).
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let mut idx = [0usize; 3];
        let mut fr = [0.0f32; 3];
        for c in 0..3 {
            let v = ((rgb[c] - self.dmin[c]) / (self.dmax[c] - self.dmin[c])).clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (v as usize).min(n - 2);
            idx[c] = i;
            fr[c] = v - i as f32;
        }
        let at = |r: usize, g: usize, b: usize| self.data[r + n * (g + n * b)];
        let mut out = [0.0f32; 3];
        for (dz, wz) in [(0, 1.0 - fr[2]), (1, fr[2])] {
            for (dy, wy) in [(0, 1.0 - fr[1]), (1, fr[1])] {
                for (dx, wx) in [(0, 1.0 - fr[0]), (1, fr[0])] {
                    let w = wx * wy * wz;
                    if w == 0.0 {
                        continue;
                    }
                    let v = at(idx[0] + dx, idx[1] + dy, idx[2] + dz);
                    out[0] += v[0] * w;
                    out[1] += v[1] * w;
                    out[2] += v[2] * w;
                }
            }
        }
        out
    }
}

fn three<'a>(mut it: impl Iterator<Item = &'a str>) -> Result<[f32; 3]> {
    let mut v = [0.0; 3];
    for x in v.iter_mut() {
        *x = it.next().context("expected 3 values")?.parse()?;
    }
    Ok(v)
}
