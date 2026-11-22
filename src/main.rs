//! peugeot300mille — analog video feedback glitch box for Linux.
//!
//!     peugeot300mille [input.mp4]                                  open the desktop app
//!     peugeot300mille render IN OUT [--params p.json | --automation a.json] [--start S] [--frames N]

mod app;
mod audio;
mod download;
mod engine;
mod media;
mod params;
mod player;
mod render;
mod runner;
mod ui;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("render") {
        return cli_render(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("--help") {
        println!("peugeot300mille [input]\npeugeot300mille render IN OUT [--params p.json | --automation a.json] [--start S] [--frames N] [--lut-in a.cube] [--lut-out b.cube]");
        return Ok(());
    }
    app::run(args.first().map(PathBuf::from))
}

fn cli_render(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: peugeot300mille render IN OUT [--params p.json | --automation a.json] [--start S] [--frames N]");
    }
    let input = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    let mut params = params::Params::default();
    let mut auto: Option<runner::Automation> = None;
    let mut start = 0.0;
    let mut frames = None;
    let mut lut_in: Option<String> = None;
    let mut lut_out: Option<String> = None;
    let mut ov: [Option<String>; 3] = [None, None, None];
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().context("missing value");
        match a.as_str() {
            // Hand-edited files reach the engine too: out-of-range indices must not.
            "--params" => params = read_json::<params::Params>(&val()?)?.clamped(),
            "--automation" => {
                let mut a: runner::Automation = read_json(&val()?)?;
                a.frames.iter_mut().for_each(|p| *p = p.clamped());
                auto = Some(a);
            }
            "--start" => {
                start = val()?.parse()?;
                if !(start >= 0.0 && f64::is_finite(start)) {
                    bail!("--start: expected a time in seconds");
                }
            }
            "--frames" => frames = Some(val()?.parse::<u32>().context("--frames: expected a number of frames")? as i64),
            "--lut-in" => lut_in = Some(val()?),
            "--lut-out" => lut_out = Some(val()?),
            "--overlay1" => ov[0] = Some(val()?),
            "--overlay2" => ov[1] = Some(val()?),
            "--overlay3" => ov[2] = Some(val()?),
            other => bail!("unknown option {other}"),
        }
    }
    if let Some(a) = &auto {
        lut_in = lut_in.or_else(|| a.lut_in.clone());
        lut_out = lut_out.or_else(|| a.lut_out.clone());
        for (o, ao) in ov.iter_mut().zip(&a.overlays) {
            if o.is_none() {
                *o = ao.clone();
            }
        }
    }
    let load = |p: &Option<String>| -> Result<Option<std::sync::Arc<engine::lut::Lut>>> {
        p.as_ref().map(|p| engine::lut::Lut::load(std::path::Path::new(p)).map(std::sync::Arc::new)).transpose()
    };
    let luts = (load(&lut_in)?, load(&lut_out)?);
    let mut clips: [Option<std::sync::Arc<engine::overlay::Clip>>; 3] = [None, None, None];
    for (i, (c, p)) in clips.iter_mut().zip(&ov).enumerate() {
        if let Some(p) = p {
            let (path, range) = match split_overlay_range(p) {
                (f, Some(r)) => (f, Some(r)),
                (f, None) => (f, auto.as_ref().and_then(|a| a.overlay_ranges[i])),
            };
            let (s, e) = range.map_or((0.0, None), |(s, e)| (s, Some(e)));
            *c = Some(std::sync::Arc::new(engine::overlay::Clip::load_range(std::path::Path::new(path), s, e)?));
        }
    }
    let knobs = match &auto {
        Some(a) => {
            start = a.start;
            render::Knobs::Recorded(a)
        }
        None => render::Knobs::Fixed(params),
    };
    let cancel = AtomicBool::new(false);
    let t0 = std::time::Instant::now();
    let mut last = 0.0;
    render::render(&input, &output, knobs, start, frames, luts, clips, &cancel, &mut |d, n| {
        let el = t0.elapsed().as_secs_f64();
        if el - last > 1.0 || d == n {
            last = el;
            eprint!("\r{d}/{n} frames, {:.1} fps   ", d as f64 / el.max(1e-9));
        }
    })?;
    eprintln!("\nwrote {}", output.display());
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {path}"))?;
    serde_json::from_str(&text).with_context(|| format!("invalid settings in {path}"))
}

/// A file can be followed by @start-end in seconds: boucle.mp4@2.5-8. Anything else after an @
/// belongs to the file's name (moi@home-video.mp4).
fn split_overlay_range(arg: &str) -> (&str, Option<(f64, f64)>) {
    let range = arg.rsplit_once('@').and_then(|(file, r)| {
        let (a, b) = r.split_once('-')?;
        let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
        (a.is_finite() && b.is_finite() && a >= 0.0 && b > a).then_some((file, (a, b)))
    });
    match range {
        Some((file, r)) => (file, Some(r)),
        None => (arg, None),
    }
}

#[cfg(test)]
mod tests {
    use super::split_overlay_range;

    #[test]
    fn overlay_ranges_are_told_from_file_names() {
        assert_eq!(split_overlay_range("boucle.mp4@2.5-8"), ("boucle.mp4", Some((2.5, 8.0))));
        assert_eq!(split_overlay_range("boucle.mp4"), ("boucle.mp4", None));
        assert_eq!(split_overlay_range("moi@home-video.mp4"), ("moi@home-video.mp4", None));
        assert_eq!(split_overlay_range("a.mp4@nan-inf"), ("a.mp4@nan-inf", None));
        assert_eq!(split_overlay_range("a.mp4@8-2"), ("a.mp4@8-2", None));
    }
}
