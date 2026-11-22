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

use crate::params::Params;
use signal::*;
use std::sync::Arc;

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
        let p = p.clamped();
        for (d, s) in self.rgb.iter_mut().zip(src) {
            *d = *s as f32 * (1.0 / 255.0);
        }
        input::process(&mut self.rgb, &p, self.lut_in.as_deref());
        overlay::composite(&mut self.rgb, &self.overlays, &p, 0, self.tv_k);
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
        self.amp.process(&mut self.comp, &p);
        if p.sync > 0.0 {
            self.sync.process(&mut self.comp, &self.src_comp, &self.tpl, p.sync);
        }
        if vhs::Vhs::active(&p) {
            self.vhs.process(&mut self.comp, abs0, &p);
        }
        self.tv.push(&self.comp, abs0);
        self.frame += 1;
    }

    /// The screen as RGB8, 768 × 480.
    pub fn screen_rgb8(&mut self, out: &mut [u8]) {
        let p = self.last_p;
        let on_top = self.overlays.iter().zip(&p.overlays).any(|(c, o)| c.is_some() && o.place == 1);
        let lut = self.lut_out.as_ref().filter(|_| self.lut_out_amount > 0.0);
        let src = if on_top || lut.is_some() {
            for (d, s) in self.out_rgb.iter_mut().zip(&self.tv.raster) {
                *d = s.clamp(0.0, 1.0);
            }
            if on_top {
                overlay::composite(&mut self.out_rgb, &self.overlays, &p, 1, self.tv_k);
            }
            if let Some(l) = lut {
                input::apply_lut(&mut self.out_rgb, l, self.lut_out_amount);
            }
            &self.out_rgb
        } else {
            &self.tv.raster
        };
        for (d, s) in out.iter_mut().zip(src) {
            *d = (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }

    pub fn h_lock(&self) -> f32 {
        self.tv.h_lock
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
