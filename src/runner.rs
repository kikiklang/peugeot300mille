//! Runs the box against a source video on its timeline.
//!
//! The TV always runs at 29.97 frames a second. Output frame `j` (at the source's frame rate)
//! shows the TV frame running at that moment; each TV frame takes the source frame of its own
//! moment. Starting anywhere warms the box up from 8 TV frames earlier.

use crate::audio::{Sound, N_FEAT};
use crate::engine::overlay::N_OVERLAYS;
use crate::engine::{sanitized, signal::TV_FPS, Engine};
use crate::media::{Decoder, Probe, FRAME_BYTES};
use crate::params::Params;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const PREROLL: i64 = 8;
/// TV frames the sound is read ahead of the picture.
pub const LOOKAHEAD: i64 = 2;

pub struct Runner {
    pub probe: Probe,
    pub engine: Engine,
    dec: Decoder,
    src: Vec<u8>,
    src_idx: i64,
    eof: bool,
    tv_k: i64,
    /// Next output frame.
    pub out_j: i64,
    pub screen: Vec<u8>,
    /// The soundtrack driving the knobs, if any.
    pub sound: Option<Arc<Sound>>,
    /// Sound measures of the last TV frame run.
    pub feat: [f32; N_FEAT],
}

impl Runner {
    /// Positions the box so the next output frame is the one at time `t`, warmed up.
    /// `p_at` gives the knobs for an output frame index.
    pub fn start(
        probe: Probe,
        engine: Option<Engine>,
        sound: Option<Arc<Sound>>,
        t: f64,
        p_at: &dyn Fn(i64) -> Params,
    ) -> Result<Self> {
        let fps = probe.fps();
        let j0 = out_frame_at(t, fps);
        let k_target = tv_frame_for(j0, fps);
        let k0 = (k_target - PREROLL).max(0);
        let s0 = src_frame_for(k0, fps);
        let dec = Decoder::open(&probe, s0 as f64 / fps)?;
        let mut engine = engine.unwrap_or_default();
        engine.reset();
        let mut r = Self {
            probe,
            engine,
            dec,
            src: vec![0; FRAME_BYTES],
            src_idx: s0 - 1,
            eof: false,
            tv_k: k0,
            out_j: j0,
            screen: vec![0; FRAME_BYTES],
            sound,
            feat: [0.0; N_FEAT],
        };
        // Warm up to just before j0's TV frame.
        while r.tv_k < k_target {
            let p = p_at(out_frame_for(r.tv_k, fps).max(j0));
            if !r.step_tv(&p)? {
                break;
            }
        }
        Ok(r)
    }

    fn step_tv(&mut self, p: &Params) -> Result<bool> {
        let fps = self.probe.fps();
        let want = src_frame_for(self.tv_k, fps);
        while self.src_idx < want && !self.eof {
            if self.dec.read(&mut self.src)? {
                self.src_idx += 1;
            } else {
                self.eof = true;
            }
        }
        // Past the end of the source by more than a frame: stop.
        if self.eof && (self.src_idx < 0 || want > self.src_idx + 1) {
            return Ok(false);
        }
        // Before the sound reads the knobs: its bands must not be NaN either.
        let p = &sanitized(p);
        let p = match &self.sound {
            Some(snd) => {
                // Read slightly ahead: the circuit takes a couple of frames to build up.
                self.feat = snd.at(self.tv_k + LOOKAHEAD, p);
                p.modulated(&self.feat)
            }
            None => *p,
        };
        self.engine.tv_k = self.tv_k;
        self.engine.beat = self.sound.as_ref().and_then(|s| s.beat_position(self.tv_k, &p));
        self.engine.step(&self.src, &p);
        self.tv_k += 1;
        Ok(true)
    }

    /// Produces the next output frame into `self.screen`. False at the end of the video.
    pub fn next(&mut self, p: &Params) -> Result<bool> {
        let fps = self.probe.fps();
        let last = self.probe.frame_count();
        if last > 0 && self.out_j >= last {
            return Ok(false);
        }
        let k = tv_frame_for(self.out_j, fps);
        while self.tv_k <= k {
            if !self.step_tv(p)? {
                return Ok(false);
            }
        }
        self.engine.screen_rgb8(&mut self.screen);
        self.out_j += 1;
        Ok(true)
    }

    /// Index of the last TV frame run.
    pub fn tv_frame(&self) -> i64 {
        self.tv_k - 1
    }

    /// Time of the frame now in `screen`.
    pub fn time(&self) -> f64 {
        (self.out_j - 1).max(0) as f64 / self.probe.fps()
    }
}

pub fn tv_frame_for(j: i64, fps: f64) -> i64 {
    ((j as f64 / fps) * TV_FPS + 1e-6).floor() as i64
}
pub fn src_frame_for(k: i64, fps: f64) -> i64 {
    ((k as f64 / TV_FPS) * fps + 1e-6).floor() as i64
}
pub fn out_frame_for(k: i64, fps: f64) -> i64 {
    src_frame_for(k, fps)
}

/// The output frame showing at time `t`. Bounded so that the frame arithmetic can't overflow,
/// however absurd the requested start.
pub fn out_frame_at(t: f64, fps: f64) -> i64 {
    (t * fps).round().clamp(0.0, 1e15) as i64
}

/// Knob values recorded frame by frame while playing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Automation {
    pub source: String,
    pub start: f64,
    pub fps_num: u32,
    pub fps_den: u32,
    /// One entry per output frame from `start`.
    pub frames: Vec<Params>,
    /// LUT files in use while recording.
    #[serde(default)]
    pub lut_in: Option<String>,
    #[serde(default)]
    pub lut_out: Option<String>,
    /// Overlay files in use while recording.
    #[serde(default)]
    #[serde(deserialize_with = "crate::params::fill_slots")]
    pub overlays: [Option<String>; N_OVERLAYS],
    /// The looping part of each overlay file (start, end) in seconds.
    #[serde(default)]
    #[serde(deserialize_with = "crate::params::fill_slots")]
    pub overlay_ranges: [Option<(f64, f64)>; N_OVERLAYS],
}

impl Automation {
    pub fn at(&self, j_rel: i64) -> Params {
        if self.frames.is_empty() {
            return Params::default();
        }
        self.frames[j_rel.clamp(0, self.frames.len() as i64 - 1) as usize]
    }
}
