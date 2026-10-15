//! Two-finger navigation on a touch screen (phones, tablets): a second finger turns the gesture
//! into view navigation. Its pan moves the image with the fingers; the pinch zoom and the twist
//! (Preferences › Enhanced Controls › Zoom with Trackpad Pinch / Rotate View with Trackpad) come
//! through `wheel_nav` and `rotate_view`, which read the same multi-touch state.
//!
//! The tool never sees such a gesture: a stroke or drag the first finger had started is dropped
//! when the second lands (nothing is committed), and the canvas ignores the fingers until all of
//! them are lifted, so the finger left on the glass doesn't start painting. Preferences ›
//! Enhanced Controls › Touch Gestures off leaves touches to the tools.

use egui::{Context, Id, Vec2};

/// One frame of a two-finger gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// Screen points the fingers' centre moved this frame.
    pub pan: Vec2,
    /// The gesture began this frame: drop whatever the first finger started.
    pub started: bool,
}

/// The gesture this frame, if fingers are navigating (`None`: the tools have the input). `id`
/// keeps the gesture's state per canvas; a gesture starts only with the fingers' centre in `area`.
pub fn frame(ctx: &Context, id: Id, area: egui::Rect, enabled: bool) -> Option<Step> {
    let (multi, any) = ctx.input(|i| (i.multi_touch(), i.any_touches()));
    let was = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    match multi {
        Some(m) if enabled && m.num_touches >= 2 && (was || area.contains(m.center_pos)) => {
            if !was {
                ctx.data_mut(|d| d.insert_temp(id, true));
            }
            let pan = if m.translation_delta.is_finite() { m.translation_delta } else { Vec2::ZERO };
            ctx.request_repaint();
            Some(Step { pan, started: !was })
        }
        // Fingers still down after the gesture: they stay off the tools until all are lifted.
        _ if was && any => Some(Step { pan: Vec2::ZERO, started: false }),
        _ => {
            if was {
                ctx.data_mut(|d| d.remove::<bool>(id));
            }
            None
        }
    }
}

/// How long a finger must rest for a long press (seconds), and how far it may wander (points).
pub const LONG_PRESS_S: f64 = 0.5;
const LONG_PRESS_SLOP: f32 = 8.0;

/// A finger resting on `area` for [`LONG_PRESS_S`] (touch screens have no right button): where,
/// once per press. The canvas opens the context menu a right-click opens there, and holds the
/// fingers off the tools until they lift (`frame` reports the hold), so the press that opened the
/// menu does nothing else.
pub fn long_press(ctx: &Context, id: Id, area: egui::Rect) -> Option<egui::Pos2> {
    let fired_id = id.with("long-press");
    let (touching, multi, down, origin, start, now, pos) = ctx.input(|i| {
        (
            i.any_touches(),
            i.multi_touch().is_some(),
            i.pointer.primary_down(),
            i.pointer.press_origin(),
            i.pointer.press_start_time(),
            i.time,
            i.pointer.latest_pos(),
        )
    });
    if !touching {
        ctx.data_mut(|d| d.remove::<bool>(fired_id));
        return None;
    }
    let fired = ctx.data(|d| d.get_temp::<bool>(fired_id)).unwrap_or(false);
    let (Some(origin), Some(start), Some(pos)) = (origin, start, pos) else { return None };
    if fired || multi || !down || !area.contains(origin) || (pos - origin).length() > LONG_PRESS_SLOP {
        return None;
    }
    let held = now - start;
    if held < LONG_PRESS_S {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((LONG_PRESS_S - held).max(0.0) + 0.01));
        return None;
    }
    ctx.data_mut(|d| {
        d.insert_temp(fired_id, true);
        // Keep the fingers off the tools until they lift (see `frame`).
        d.insert_temp(id, true);
    });
    Some(origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Pos2, RawInput, TouchDeviceId, TouchId, TouchPhase};

    fn touch(id: u64, phase: TouchPhase, x: f32, y: f32) -> Event {
        Event::Touch { device_id: TouchDeviceId(1), id: TouchId(id), phase, pos: Pos2::new(x, y), force: None }
    }

    fn run(ctx: &Context, events: Vec<Event>, enabled: bool) -> Option<Step> {
        let mut out = None;
        // eframe sends the primary touch as the pointer too; egui starts a gesture where it is.
        let mut events = events;
        if let Some(Event::Touch { pos, .. }) = events.first() {
            events.push(Event::PointerMoved(*pos));
        }
        let input = RawInput { events, ..Default::default() };
        let mut full = ctx.run_ui(input, |ui| out = frame(ui.ctx(), Id::new("t"), egui::Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), enabled));
        full.textures_delta.clear();
        out
    }

    #[test]
    fn two_fingers_pan_and_hold_the_tools_until_lifted() {
        let ctx = Context::default();
        assert_eq!(run(&ctx, vec![touch(1, TouchPhase::Start, 100.0, 100.0)], true), None, "one finger is a tool gesture");
        // egui reports the pinch from the frame the second finger lands or the next.
        let a = run(&ctx, vec![touch(2, TouchPhase::Start, 200.0, 100.0)], true);
        let b = run(&ctx, vec![touch(1, TouchPhase::Move, 110.0, 120.0), touch(2, TouchPhase::Move, 210.0, 120.0)], true).expect("moving");
        assert!(a.map_or(b.started, |a| a.started && !b.started), "started once: {a:?} {b:?}");
        let s = run(&ctx, vec![touch(1, TouchPhase::Move, 120.0, 140.0), touch(2, TouchPhase::Move, 220.0, 140.0)], true).expect("moving");
        assert!(!s.started);
        assert!((s.pan - Vec2::new(10.0, 20.0)).length() < 0.01, "{:?}", s.pan);
        let s = run(&ctx, vec![touch(2, TouchPhase::End, 210.0, 120.0)], true).expect("a finger still down");
        assert_eq!(s.pan, Vec2::ZERO);
        assert_eq!(run(&ctx, vec![touch(1, TouchPhase::End, 110.0, 120.0)], true), None, "all lifted");
        assert_eq!(run(&ctx, vec![touch(3, TouchPhase::Start, 10.0, 10.0)], true), None, "the next touch is the tool's");
    }

    #[test]
    fn a_gesture_elsewhere_is_not_the_canvas_s() {
        let ctx = Context::default();
        run(&ctx, vec![touch(1, TouchPhase::Start, 1500.0, 100.0)], true);
        assert_eq!(run(&ctx, vec![touch(2, TouchPhase::Start, 1600.0, 100.0)], true), None);
    }

    #[test]
    fn a_resting_finger_is_a_long_press_once_then_holds_the_tools() {
        let ctx = Context::default();
        let area = egui::Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0));
        let step = |events: Vec<Event>, t: f64| {
            let mut out = (None, None);
            let input = RawInput { events, time: Some(t), ..Default::default() };
            let mut full = ctx.run_ui(input, |ui| {
                let lp = long_press(ui.ctx(), Id::new("t"), area);
                out = (lp, frame(ui.ctx(), Id::new("t"), area, true));
            });
            full.textures_delta.clear();
            out
        };
        let p = Pos2::new(100.0, 100.0);
        let press = vec![
            touch(1, TouchPhase::Start, 100.0, 100.0),
            Event::PointerMoved(p),
            Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        ];
        assert_eq!(step(press, 1.0).0, None);
        assert_eq!(step(vec![touch(1, TouchPhase::Move, 102.0, 101.0), Event::PointerMoved(Pos2::new(102.0, 101.0))], 1.3).0, None);
        let (lp, _) = step(vec![], 1.6);
        assert_eq!(lp, Some(p));
        let (lp, nav) = step(vec![], 1.9);
        assert_eq!(lp, None, "once per press");
        assert!(nav.is_some(), "the tools sit out the rest of the press");
        let up = vec![
            touch(1, TouchPhase::End, 102.0, 101.0),
            Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
        ];
        assert_eq!(step(up, 2.0), (None, None));
    }

    #[test]
    fn a_moving_finger_is_no_long_press() {
        let ctx = Context::default();
        let area = egui::Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0));
        let mut fired = false;
        for (k, t) in [1.0, 1.3, 1.6, 1.9].into_iter().enumerate() {
            let x = 100.0 + 20.0 * k as f32;
            let p = Pos2::new(x, 100.0);
            let mut events = vec![touch(1, if k == 0 { TouchPhase::Start } else { TouchPhase::Move }, x, 100.0), Event::PointerMoved(p)];
            if k == 0 {
                events.push(Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
            }
            let mut full =
                ctx.run_ui(RawInput { events, time: Some(t), ..Default::default() }, |ui| fired |= long_press(ui.ctx(), Id::new("m"), area).is_some());
            full.textures_delta.clear();
        }
        assert!(!fired);
    }

    #[test]
    fn off_leaves_touches_to_the_tools() {
        let ctx = Context::default();
        run(&ctx, vec![touch(1, TouchPhase::Start, 100.0, 100.0)], false);
        assert_eq!(run(&ctx, vec![touch(2, TouchPhase::Start, 200.0, 100.0)], false), None);
    }
}
