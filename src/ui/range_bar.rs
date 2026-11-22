//! The range bar: which part of a file an overlay loops.

use super::{ACCENT, BG, WEAK};
use eframe::egui::{self, Color32, FontId, RichText};

/// What a drag on the range bar is moving.
#[derive(Clone, Copy, PartialEq)]
enum RangeGrab {
    Start,
    End,
    Both,
}

/// How far outside the selection (in pixels) a press still takes a handle without jumping it.
const HANDLE_REACH: f32 = 10.0;

/// What a press takes: the strip under the timeline moves the whole selection, however short it
/// is; on the timeline, the nearest end.
fn grab_at(p: egui::Pos2, strip_top: f32, xa: f32, xb: f32) -> RangeGrab {
    if p.y >= strip_top {
        RangeGrab::Both
    } else if p.x < (xa + xb) / 2.0 {
        RangeGrab::Start
    } else {
        RangeGrab::End
    }
}

/// The selection moved whole by `dt` seconds, keeping its length, inside the file.
fn moved(a: f64, b: f64, dt: f64, dur: f64) -> (f64, f64) {
    let len = b - a;
    let na = (a + dt).clamp(0.0, (dur - len).max(0.0));
    (na, na + len)
}

/// A valid selection inside the file, whatever was asked for.
fn sane_range(a: f64, b: f64, dur: f64, min_len: f64) -> (f64, f64) {
    let a = if a.is_finite() { a.clamp(0.0, (dur - min_len).max(0.0)) } else { 0.0 };
    let b = if b.is_finite() { b.clamp((a + min_len).min(dur), dur) } else { dur };
    (a, b)
}

/// Seconds between two ticks, so that a file of any length keeps about 24 of them at most.
fn tick_step(dur: f64) -> f64 {
    [0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter().find(|s| dur / s <= 24.0).unwrap_or_else(|| (dur / 24.0 / 120.0).ceil() * 120.0)
}

/// A thin timeline for the whole file with the looping part between two thin handles, and under
/// it a strip that moves the selection. Drag on the timeline to move the nearest end, drag the
/// strip to move it whole; Shift = 10× finer. The wheel on a handle steps one frame. Returns true
/// when a drag or an edit has finished.
pub fn range_bar(ui: &mut egui::Ui, id: egui::Id, a: &mut f64, b: &mut f64, dur: f64, max_len: f64, frame: f64) -> bool {
    use egui::{pos2, vec2, Align2, CursorIcon, Rect, Sense, Stroke};
    let dur = if dur.is_finite() { dur.max(0.2) } else { b.max(0.2) };
    let min_len = frame.max(0.04);
    // The range can come from a longer file than the one now loaded.
    (*a, *b) = sane_range(*a, *b, dur, min_len);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click_and_drag());
    let track = Rect::from_min_size(rect.min + vec2(6.0, 4.0), vec2((rect.width() - 12.0).max(1.0), 18.0));
    let strip = Rect::from_min_max(pos2(track.left(), track.bottom() + 15.0), pos2(track.right(), rect.bottom()));
    let tx = |t: f64| track.left() + (t / dur) as f32 * track.width();
    // For the pointer-to-time conversions: a collapsed bar must not divide by zero.
    let width = track.width().max(1.0);
    let painter = ui.painter_at(rect);

    // Track, seconds ticks, selection, handles.
    painter.rect_filled(track, 4.0, BG);
    let step = tick_step(dur);
    for n in 0..=((dur + 1e-9) / step) as i64 {
        let t = n as f64 * step;
        let x = tx(t);
        painter.line_segment([pos2(x, track.bottom()), pos2(x, track.bottom() + 3.0)], Stroke::new(1.0, Color32::from_gray(70)));
        if n % 2 == 0 {
            let align = if x > track.right() - 8.0 { Align2::RIGHT_BOTTOM } else if x < track.left() + 8.0 { Align2::LEFT_BOTTOM } else { Align2::CENTER_BOTTOM };
            painter.text(pos2(x, track.bottom() + 14.0), align, format!("{t:.0}"), FontId::proportional(9.0), Color32::from_gray(110));
        }
    }
    let sel = Rect::from_x_y_ranges(tx(*a)..=tx(*b).max(tx(*a) + 1.0), track.y_range());
    let ptr = resp.hover_pos();
    let near = |x: f32| ptr.is_some_and(|p| (p.x - x).abs() < 7.0);
    // Which part a press would move.
    let pick = |p: egui::Pos2| grab_at(p, strip.top(), tx(*a), tx(*b));
    let grabbed = ui.data(|d| d.get_temp::<RangeGrab>(id));
    let target = grabbed.or(ptr.map(pick));
    painter.rect_filled(sel, 2.0, ACCENT.gamma_multiply(if target == Some(RangeGrab::Both) { 0.7 } else { 0.35 }));
    // The strip, with a grip that stays easy to see however short the selection is.
    let moving = target == Some(RangeGrab::Both);
    painter.rect_filled(strip, 3.0, BG);
    let grip_w = (tx(*b) - tx(*a)).max(44.0).min(strip.width());
    let grip_x = ((tx(*a) + tx(*b)) / 2.0).clamp(strip.left() + grip_w / 2.0, strip.right() - grip_w / 2.0);
    let grip = Rect::from_center_size(pos2(grip_x, strip.center().y), vec2(grip_w, strip.height()));
    painter.rect_filled(grip, 3.0, if moving { ACCENT } else { ACCENT.gamma_multiply(0.55) });
    for dx in [-4.0, 0.0, 4.0] {
        let x = grip.center().x + dx;
        painter.line_segment([pos2(x, grip.top() + 3.0), pos2(x, grip.bottom() - 3.0)], Stroke::new(1.0, Color32::BLACK));
    }
    for (x, which) in [(tx(*a), RangeGrab::Start), (tx(*b), RangeGrab::End)] {
        let over = ptr.is_some_and(|p| (p.x - x).abs() <= HANDLE_REACH);
        let hot = grabbed == Some(which) || (grabbed.is_none() && target == Some(which) && over);
        painter.rect_filled(
            Rect::from_center_size(pos2(x, track.center().y), vec2(if hot { 4.0 } else { 2.5 }, track.height() + 6.0)),
            1.5,
            if hot { Color32::WHITE } else { ACCENT },
        );
    }

    if ptr.is_some() {
        let icon = match (target, grabbed) {
            (Some(RangeGrab::Both), Some(_)) => CursorIcon::Grabbing,
            (Some(RangeGrab::Both), None) => CursorIcon::Grab,
            _ => CursorIcon::ResizeHorizontal,
        };
        ui.ctx().set_cursor_icon(icon);
    }
    let clamp_start = |v: f64, b: f64| v.clamp((b - max_len).max(0.0), (b - min_len).max(0.0));
    let clamp_end = |v: f64, a: f64| v.clamp((a + min_len).min(dur), (a + max_len).min(dur));
    let mut done = false;
    if resp.drag_started() {
        // Where the button went down, not where the pointer is once the drag is recognised: by
        // then it has moved a few pixels, enough to have left a narrow selection.
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            let g = pick(p);
            // A press outside the part jumps the nearest end there.
            let t = ((p.x - track.left()) / width) as f64 * dur;
            match g {
                RangeGrab::Start if p.x < tx(*a) - HANDLE_REACH => *a = clamp_start(t, *b),
                RangeGrab::End if p.x > tx(*b) + HANDLE_REACH => *b = clamp_end(t, *a),
                _ => {}
            }
            ui.data_mut(|d| d.insert_temp(id, g));
        }
    }
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
        let dt = resp.drag_delta().x as f64 / width as f64 * dur * fine;
        match ui.data(|d| d.get_temp::<RangeGrab>(id)) {
            Some(RangeGrab::Start) => *a = clamp_start(*a + dt, *b),
            Some(RangeGrab::End) => *b = clamp_end(*b + dt, *a),
            Some(RangeGrab::Both) => {
                (*a, *b) = moved(*a, *b, dt, dur);
            }
            None => {}
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<RangeGrab>(id));
        done = true;
    }
    // Mouse wheel on a handle: one frame per notch.
    if let Some(p) = ptr {
        let dy = ui.input(|i| i.raw_scroll_delta.y);
        if dy != 0.0 && (near(tx(*a)) || near(tx(*b))) {
            let step = frame * dy.signum() as f64;
            if (p.x - tx(*a)).abs() <= (p.x - tx(*b)).abs() {
                *a = clamp_start(*a + step, *b);
            } else {
                *b = clamp_end(*b + step, *a);
            }
            done = true;
        }
    }
    // No tooltip on this bar: it opens over the timeline and swallows the next press.

    // Exact times, editable.
    ui.horizontal(|ui| {
        let mut na = *a;
        let mut nb = *b;
        let ra = ui.add(egui::DragValue::new(&mut na).range(0.0..=dur).speed(frame).suffix(" s").fixed_decimals(2)).on_hover_text("Début");
        ui.label(RichText::new("→").color(WEAK));
        let rb = ui.add(egui::DragValue::new(&mut nb).range(0.0..=dur).speed(frame).suffix(" s").fixed_decimals(2)).on_hover_text("Fin");
        if ra.changed() {
            *a = clamp_start(na, *b);
        }
        if rb.changed() {
            *b = clamp_end(nb, *a);
        }
        done |= ra.drag_stopped() || ra.lost_focus() || rb.drag_stopped() || rb.lost_focus();
        let len = *b - *a;
        let note = if len >= max_len - 1e-3 { format!("{len:.2} s (maximum)") } else { format!("{len:.2} s") };
        ui.label(RichText::new(format!("boucle de {note}, fichier de {dur:.1} s · bande du bas : déplacer")).small().color(WEAK));
    });
    done
}

#[cfg(test)]
mod range_tests {
    use super::*;

    #[test]
    fn the_strip_takes_the_whole_selection_and_the_timeline_an_end() {
        assert!(grab_at(egui::pos2(400.0, 51.0), 45.0, 100.0, 104.0) == RangeGrab::Both);
        assert!(grab_at(egui::pos2(101.0, 20.0), 45.0, 100.0, 104.0) == RangeGrab::Start);
        assert!(grab_at(egui::pos2(103.0, 20.0), 45.0, 100.0, 104.0) == RangeGrab::End);
    }

    #[test]
    fn a_range_from_a_longer_file_is_brought_back_inside() {
        assert_eq!(sane_range(1.0, 5.0, 6.0, 0.04), (1.0, 5.0));
        let (a, b) = sane_range(30.0, 40.0, 6.0, 0.04);
        assert!(a < b && b <= 6.0 && a >= 0.0);
        assert_eq!(sane_range(f64::NAN, f64::NAN, 6.0, 0.04), (0.0, 6.0));
    }

    #[test]
    fn ticks_stay_few_on_very_long_files() {
        assert_eq!(tick_step(10.0), 0.5);
        assert_eq!(tick_step(2000.0), 120.0);
        assert!(36_000.0 / tick_step(36_000.0) <= 24.0);
    }

    /// Runs the bar on a 700 s file with a 10 s selection at 100 s, through a script of events.
    fn run_bar(script: Vec<Vec<egui::Event>>) -> (f64, f64) {
        use egui::{Pos2, RawInput, Rect};
        let ctx = egui::Context::default();
        let (mut a, mut b) = (100.0, 110.0);
        for (i, events) in script.into_iter().enumerate() {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(520.0, 200.0))), time: Some(i as f64 * 0.1), events, ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    range_bar(ui, egui::Id::new("bar"), &mut a, &mut b, 700.0, 20.0, 1.0 / 30.0);
                });
            });
        }
        (a, b)
    }

    fn drag(from: egui::Pos2, dx: f32) -> Vec<Vec<egui::Event>> {
        use egui::{Event, PointerButton};
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let to = from + egui::vec2(dx, 0.0);
        vec![
            vec![],
            vec![Event::PointerMoved(from)],
            vec![press(from, true)],
            vec![Event::PointerMoved(from + egui::vec2(dx / 2.0, 0.0))],
            vec![Event::PointerMoved(to)],
            vec![press(to, false)],
            vec![],
        ]
    }

    #[test]
    fn the_strip_moves_a_tiny_selection_from_anywhere() {
        let (a, b) = run_bar(drag(egui::Pos2::new(400.0, 51.0), 40.0));
        assert!(a > 110.0, "the selection did not move: {a}..{b}");
        assert!((b - a - 10.0).abs() < 1e-6, "its length changed: {a}..{b}");
    }

    /// The drag is recognised a few pixels after the press: it must take what was pressed.
    #[test]
    fn a_press_on_the_timeline_moves_only_the_end_it_is_near() {
        let (a, b) = run_bar(drag(egui::Pos2::new(92.0, 21.0), 30.0));
        assert_eq!(a, 100.0, "the start moved");
        assert!(b > 110.0, "the end did not follow: {b}");
    }

    /// A tooltip used to open over the left of the timeline and take the press.
    #[test]
    fn every_point_of_the_bar_answers_a_drag() {
        for y in [14.0, 21.0, 28.0, 46.0, 51.0, 56.0] {
            for x in [40.0, 92.0, 200.0, 400.0, 480.0] {
                let (a, b) = run_bar(drag(egui::Pos2::new(x, y), 30.0));
                assert!((a, b) != (100.0, 110.0), "nothing moved from ({x}, {y})");
            }
        }
    }

    /// A selection left over from a longer file used to panic in `clamp` when dragged whole.
    #[test]
    fn dragging_a_stale_selection_does_not_panic() {
        use egui::{Event, PointerButton, Pos2, RawInput, Rect};
        let ctx = egui::Context::default();
        let (mut a, mut b) = (0.0, 30.0);
        let centre = Pos2::new(250.0, 20.0);
        let press = |pressed| Event::PointerButton { pos: centre, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let script = [
            vec![Event::PointerMoved(centre)],
            vec![press(true)],
            vec![Event::PointerMoved(centre + egui::vec2(30.0, 0.0))],
            vec![Event::PointerMoved(centre + egui::vec2(60.0, 0.0))],
            vec![press(false)],
        ];
        for (i, events) in script.into_iter().enumerate() {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(520.0, 200.0))), time: Some(i as f64 * 0.1), events, ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // The bar is asked to show 0..30 s of a 6 s file.
                    range_bar(ui, egui::Id::new("bar"), &mut a, &mut b, 6.0, 20.0, 1.0 / 30.0);
                });
            });
        }
        assert!(a >= 0.0 && b <= 6.0 && a < b, "{a} {b}");
    }
}
