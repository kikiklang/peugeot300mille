//! The sync mixer: takes sync from the clean source signal and re-inserts it into the looped
//! signal. Its "decision line" (the Sync knob) moves it from restoring a solid lock, to dropping
//! vertical sync (the picture rolls), to lifting the picture to white, adding snow, losing sync
//! altogether and finally sinking to black.

use super::signal::*;

/// Below this knob value the mixer fades in, instead of switching on at once.
const FADE_IN: f32 = 0.05;
/// Knob value from which the colour burst is fully clipped.
const BURST_CLIP_AT: f32 = 0.10;

/// The knob worked out for the current frame.
#[derive(Clone, Copy, Default)]
struct Levels {
    w_roll: f32,
    a_src: f32,
    gain: f32,
    lift: f32,
    sink: f32,
    noise: f32,
    depth: f32,
    h_drop: f32,
    /// How much of the mixer is in the signal.
    wet: f32,
    /// How much of the signal below blanking is clipped outside sync, burst included.
    clip: f32,
}

pub struct SyncMix {
    src_lp: f32,
    keep_v: bool,
    rng: Rng,
    field_seen: [bool; 2],
    levels: Levels,
}

impl SyncMix {
    pub fn new() -> Self {
        Self { src_lp: BLANK, keep_v: true, rng: Rng(0x9e37_79b9), field_seen: [false; 2], levels: Levels::default() }
    }

    /// Readies a frame, to be fed in order through [`SyncMix::run`].
    pub fn begin(&mut self, sync: f32) {
        let u = sync.clamp(0.0, 1.0);
        let w_roll = smoothstep(0.10, 0.32, u);
        let w_lift = smoothstep(0.35, 0.58, u);
        let w_lsnow = smoothstep(0.55, 0.70, u);
        let w_snow = smoothstep(0.68, 0.85, u);
        let w_sink = smoothstep(0.86, 1.0, u);

        let a_src = rc(1.5e6);
        let gain = (0.9 * (1.0 - 0.5 * w_lift) + 0.1) * (1.0 - 0.75 * w_snow) * (1.0 - 0.6 * w_sink);
        let lift = 0.62 * w_lift * (1.0 - w_snow * 0.5) * (1.0 - w_sink);
        let sink = -0.30 * w_sink;
        let noise = 0.06 * w_lsnow + 0.45 * w_snow * (1.0 - 0.5 * w_sink);
        let depth = BLANK * (1.0 - 0.75 * w_snow) * (1.0 - 0.9 * w_sink);
        let h_drop = 0.6 * w_snow;
        let wet = (u / FADE_IN).min(1.0);
        let clip = smoothstep(0.0, BURST_CLIP_AT, u);
        self.levels = Levels { w_roll, a_src, gain, lift, sink, noise, depth, h_drop, wet, clip };
        self.field_seen = [false; 2];
    }

    /// `sig` is a block of the amp's output starting at sample `a0` of the frame (modified in
    /// place), `src` the same block of the clean composite before the loop.
    pub fn run(&mut self, sig: &mut [f32], src: &[f32], tpl: &Template, a0: usize) {
        let Levels { w_roll, a_src, gain, lift, sink, noise, depth, h_drop, wet, clip } = self.levels;
        let mut st = self.rng;
        let (mut src_lp, mut keep_v) = (self.src_lp, self.keep_v);
        for (a, ((v, &s), &vi)) in (a0..).zip(sig.iter_mut().zip(src).zip(&tpl.vi[a0..])) {
            // A new field: decide whether its vertical sync gets through.
            let field = (a >= FIELD) as usize;
            if vi && !self.field_seen[field] {
                self.field_seen[field] = true;
                keep_v = st.unit() >= w_roll;
            }
            src_lp += a_src * (s - src_lp);
            let in_sync = src_lp < BLANK * 0.5;
            let is_sync = in_sync && (!vi || keep_v);
            // The looped signal's own sync is stripped; the mixer's goes in instead. The clip also
            // bites the colour burst, which is what drains the picture to black and white as the
            // knob rises: a wanted part of the look, brought in gradually from zero.
            let clipped = v.max(BLANK - 0.01);
            let video = if in_sync || clip >= 1.0 { clipped } else { *v + (clipped - *v) * clip };
            let mut out = (video - BLANK) * gain + BLANK + lift + sink + noise * st.tri();
            if is_sync && (h_drop == 0.0 || st.unit() >= h_drop * 0.02) {
                out -= depth;
            }
            *v = if wet >= 1.0 { out } else { *v + (out - *v) * wet };
        }
        (self.rng, self.src_lp, self.keep_v) = (st, src_lp, keep_v);
    }
}
