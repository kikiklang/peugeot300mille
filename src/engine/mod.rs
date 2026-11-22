//! The box, one TV frame at a time:
//!
//! source RGB → input processor (+ LUT) → (+ camera filming the last screen) → YIQ →
//! composite NTSC → video amp (enhancer + feedback) → [sync mixer] → [VHS deck] → TV → screen
//! (→ output LUT).

pub mod amp;
pub mod camera;
pub mod encoder;
pub mod input;
pub mod lut;
pub mod overlay;
pub mod signal;
pub mod syncmix;
pub mod tv;
pub mod vhs;

use crate::params::{Params, INPUT_KNOBS, KNOBS};
use rayon::prelude::*;
use signal::*;
use std::sync::{mpsc::sync_channel, Arc};

/// Lines handed from one stage of the circuit to the next at a time.
const BLOCK_LINES: usize = 16;
/// Blocks that may wait between two stages.
const BLOCKS_QUEUED: usize = 4;

pub struct Engine {
    tpl: Template,
    amp: amp::Amp,
    sync: syncmix::SyncMix,
    tv: tv::Tv,
    cam: camera::Camera,
    vhs: vhs::Vhs,
    pub lut_in: Option<Arc<lut::Lut>>,
    pub lut_out: Option<Arc<lut::Lut>>,
    lut_out_amount: f32,
    out_rgb: Vec<f32>,
    /// Video loops laid over the picture.
    pub overlays: [Option<Arc<overlay::Clip>>; overlay::N_OVERLAYS],
    /// The TV frame on the timeline (set by the runner), which drives the loops.
    pub tv_k: i64,
    /// Where the music is at this TV frame, in beats (set by the runner), for loops on the tempo.
    pub beat: Option<f64>,
    last_p: Params,
    frame: u64,
    rgb: Vec<f32>,
    y: Vec<f32>,
    i: Vec<f32>,
    q: Vec<f32>,
    comp: Vec<f32>,
    src_comp: Vec<f32>,
}

impl Engine {
    pub fn new() -> Self {
        let tpl = Template::new();
        let tv = tv::Tv::new(&tpl);
        Self {
            tpl,
            amp: amp::Amp::new(),
            sync: syncmix::SyncMix::new(),
            tv,
            cam: camera::Camera::new(),
            vhs: vhs::Vhs::new(),
            lut_in: None,
            lut_out: None,
            lut_out_amount: 1.0,
            out_rgb: vec![0.0; W * H * 3],
            overlays: [None, None, None],
            tv_k: 0,
            beat: None,
            last_p: Params::default(),
            frame: 0,
            rgb: vec![0.0; W * H * 3],
            y: vec![0.0; W * H],
            i: vec![0.0; W * H],
            q: vec![0.0; W * H],
            comp: vec![0.0; FRAME],
            src_comp: vec![0.0; FRAME],
        }
    }

    /// Back to power-on: no history.
    pub fn reset(&mut self) {
        self.amp = amp::Amp::new();
        self.sync = syncmix::SyncMix::new();
        self.tv.reset();
        self.cam.reset();
        self.vhs = vhs::Vhs::new();
        self.frame = 0;
    }

    /// Runs one TV frame with `src` (RGB8, 768 × 480) as the source picture.
    pub fn step(&mut self, src: &[u8], p: &Params) {
        let p = sanitized(p).clamped();
        self.rgb.par_chunks_mut(W * 3).zip(src.par_chunks(W * 3)).for_each(|(ds, ss)| {
            for (d, s) in ds.iter_mut().zip(ss) {
                *d = input::level(*s as usize);
            }
        });
        input::process(&mut self.rgb, &p, self.lut_in.as_deref());
        overlay::composite(&mut self.rgb, &self.overlays, &p, 0, self.tv_k, self.beat);
        self.lut_out_amount = p.lut_out_amount;
        self.last_p = p;
        if p.mix > 0.0 {
            self.cam.film_and_mix(&self.tv.raster, &mut self.rgb, &p);
        }
        encoder::rgb_to_yiq(&self.rgb, &mut self.y, &mut self.i, &mut self.q);
        let abs0 = self.frame as i64 * FRAME as i64;
        let phase0 = (abs0 & 3) as usize;
        encoder::encode(&self.tpl, &self.y, &self.i, &self.q, &mut self.comp, phase0);
        if p.sync > 0.0 {
            self.src_comp.copy_from_slice(&self.comp);
        }
        self.run_circuit(&p, abs0);
        self.frame += 1;
    }

    /// Amp → sync mixer → VHS → TV. Each stage is a serial filter that remembers its last sample,
    /// so none can be split across cores; but they can overlap, each working on a block of lines
    /// while the previous stage prepares the next one. Every stage still sees the same samples
    /// in the same order.
    fn run_circuit(&mut self, p: &Params, abs0: i64) {
        let Self { amp, sync, vhs, tv, tpl, comp, src_comp, .. } = self;
        let (tpl, src_comp) = (&*tpl, &*src_comp);
        let with_sync = p.sync > 0.0;
        let with_vhs = vhs::Vhs::active(p);
        amp.begin(comp.len(), p);
        if with_sync {
            sync.begin(p.sync);
        }
        if with_vhs {
            vhs.begin(p);
        }
        tv.begin(abs0);
        let block = BLOCK_LINES * SPL;
        std::thread::scope(|scope| {
            let (to_mix, from_amp) = sync_channel::<(usize, &mut [f32])>(BLOCKS_QUEUED);
            let (to_tv, from_mix) = sync_channel::<&mut [f32]>(BLOCKS_QUEUED);
            scope.spawn(move || {
                for (n, b) in comp.chunks_mut(block).enumerate() {
                    amp.run(b, n * block);
                    if to_mix.send((n, b)).is_err() {
                        break;
                    }
                }
            });
            scope.spawn(move || {
                for (n, b) in from_amp {
                    if with_sync {
                        sync.run(b, &src_comp[n * block..], tpl, n * block);
                    }
                    if with_vhs {
                        vhs.run(b, n * BLOCK_LINES, abs0);
                    }
                    if to_tv.send(b).is_err() {
                        break;
                    }
                }
            });
            for b in from_mix {
                tv.feed(b);
            }
        });
        tv.end();
    }

    /// The screen as RGB8, 768 × 480.
    pub fn screen_rgb8(&mut self, out: &mut [u8]) {
        let p = self.last_p;
        let on_top = self.overlays.iter().zip(&p.overlays).any(|(c, o)| c.is_some() && o.place == 1);
        let lut = self.lut_out.as_ref().filter(|_| self.lut_out_amount > 0.0);
        let src = if on_top || lut.is_some() {
            self.out_rgb.par_chunks_mut(W * 3).zip(self.tv.raster.par_chunks(W * 3)).for_each(|(ds, ss)| {
                for (d, s) in ds.iter_mut().zip(ss) {
                    *d = s.clamp(0.0, 1.0);
                }
            });
            if on_top {
                overlay::composite(&mut self.out_rgb, &self.overlays, &p, 1, self.tv_k, self.beat);
            }
            if let Some(l) = lut {
                input::apply_lut(&mut self.out_rgb, l, self.lut_out_amount);
            }
            &self.out_rgb
        } else {
            &self.tv.raster
        };
        out.par_chunks_mut(W * 3).zip(src.par_chunks(W * 3)).for_each(|(ds, ss)| {
            for (d, s) in ds.iter_mut().zip(ss) {
                *d = (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        });
    }

    pub fn h_lock(&self) -> f32 {
        self.tv.h_lock
    }
}

/// The knobs with every NaN put back to its default. A NaN goes through `clamp` untouched, then
/// sticks in the circuit's filters for good (and a NaN band limit makes `clamped` panic).
pub fn sanitized(p: &Params) -> Params {
    let mut d = Params::default();
    let mut p = *p;
    let fix = |v: &mut f32, default: f32| {
        if v.is_nan() {
            *v = default;
        }
    };
    for name in KNOBS.iter().map(|k| k.0).chain(INPUT_KNOBS.iter().map(|k| k.0)) {
        fix(p.knob_mut(name), *d.knob_mut(name));
    }
    for (o, od) in p.overlays.iter_mut().zip(&d.overlays) {
        fix(&mut o.opacity, od.opacity);
        fix(&mut o.size, od.size);
        fix(&mut o.x, od.x);
        fix(&mut o.y, od.y);
        fix(&mut o.speed, od.speed);
    }
    for (b, bd) in p.bands.iter_mut().zip(&d.bands) {
        fix(&mut b.lo, bd.lo);
        fix(&mut b.hi, bd.hi);
        fix(&mut b.sens, bd.sens);
    }
    for m in p.mods.iter_mut() {
        fix(&mut m.depth, 0.0);
    }
    fix(&mut p.lut_in_amount, d.lut_in_amount);
    fix(&mut p.lut_out_amount, d.lut_out_amount);
    fix(&mut p.sound_amount, d.sound_amount);
    fix(&mut p.tempo_phase, d.tempo_phase);
    p
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_knobs_neither_panic_nor_reach_the_circuit() {
        let mut p = Params { mix: f32::NAN, blur: f32::NAN, sync: f32::NAN, vhs_wear: f32::NAN, ..Params::default() };
        p.bands[0].lo = f32::NAN;
        p.overlays[0].x = f32::NAN;
        let clean = sanitized(&p);
        assert_eq!(clean.mix, 0.0);
        assert_eq!(clean.bands[0].lo, Params::default().bands[0].lo);
        let _ = clean.clamped();
        let mut e = Engine::new();
        e.step(&vec![128; W * H * 3], &p);
        let mut out = vec![0; W * H * 3];
        e.screen_rgb8(&mut out);
        assert!(e.tv.raster.iter().all(|v| !v.is_nan()));
    }

    #[test]
    fn short_or_empty_buffers_are_harmless() {
        let mut e = Engine::new();
        let p = Params { mix: 0.5, ..Params::default() };
        e.step(&[], &p);
        e.step(&[200; 1000], &p);
        e.screen_rgb8(&mut []);
        e.screen_rgb8(&mut [0; 10]);
    }
}
