//! The part of the player's status the window shows.

use crate::audio::N_FEAT;
use crate::player::{OverlayInfo, Status};

/// What the window needs to know about the open file.
#[derive(Clone, Copy)]
pub struct FileView {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
}

impl FileView {
    /// Length for the timeline: never zero, whatever the file announced.
    pub fn timeline(&self) -> f64 {
        if self.duration.is_finite() { self.duration.max(0.01) } else { 0.01 }
    }

    pub fn aspect(&self) -> Option<f32> {
        (self.width > 0 && self.height > 0).then(|| self.width as f32 / self.height as f32)
    }
}

/// Refreshed in place each frame. `Status` also holds the recorded automation, which grows with
/// every frame of a take: cloning it 60 times a second is what this avoids.
#[derive(Default)]
pub struct View {
    pub file: Option<FileView>,
    pub time: f64,
    pub playing: bool,
    pub live_rec: bool,
    pub auto_rec: bool,
    pub auto_frames: usize,
    pub has_automation: bool,
    pub fps: f64,
    pub h_lock: f32,
    pub has_sound: bool,
    pub bpm: Option<f32>,
    pub lut_in: Option<String>,
    pub lut_out: Option<String>,
    pub overlays: [Option<OverlayInfo>; 3],
    pub feat: [f32; N_FEAT],
    pub spectrum: Vec<f32>,
    pub message: String,
}

impl View {
    pub fn refresh(&mut self, s: &Status) {
        self.file = s.file.as_ref().map(|f| FileView { duration: f.duration, width: f.width, height: f.height });
        self.time = s.time;
        self.playing = s.playing;
        self.live_rec = s.live_rec.is_some();
        self.auto_rec = s.auto_rec;
        self.auto_frames = s.auto_frames;
        self.has_automation = s.automation.is_some();
        self.fps = s.fps;
        self.h_lock = s.h_lock;
        self.has_sound = s.has_sound;
        self.bpm = s.bpm;
        self.feat = s.feat;
        // clone_from keeps the existing buffers, so a steady status allocates nothing.
        self.lut_in.clone_from(&s.lut_in);
        self.lut_out.clone_from(&s.lut_out);
        self.spectrum.clone_from(&s.spectrum);
        self.message.clone_from(&s.message);
        for (to, from) in self.overlays.iter_mut().zip(&s.overlays) {
            match (to, from) {
                (Some(to), Some(from)) => {
                    to.name.clone_from(&from.name);
                    to.path.clone_from(&from.path);
                    (to.start, to.end, to.duration) = (from.start, from.end, from.duration);
                }
                (to, from) => *to = from.clone(),
            }
        }
    }
}
