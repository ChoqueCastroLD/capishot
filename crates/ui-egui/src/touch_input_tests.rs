use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, TouchDeviceId, TouchId, TouchPhase, pos2};

fn touch(id: u64, phase: TouchPhase, x: f32, y: f32) -> Event {
    Event::Touch { device_id: TouchDeviceId(1), id: TouchId(id), phase, pos: pos2(x, y), force: None }
}

/// A browser's touch event as eframe delivers it: its pointer emulation for the primary finger
/// first, then the touch.
fn browser(id: u64, phase: TouchPhase, x: f32, y: f32, primary: bool) -> Vec<Event> {
    let p = pos2(x, y);
    let mut v = Vec::new();
    if primary {
        match phase {
            TouchPhase::Start => v.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }),
            TouchPhase::Move => v.push(Event::PointerMoved(p)),
            _ => {
                v.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
                v.push(Event::PointerGone);
            }
        }
    }
    v.push(touch(id, phase, x, y));
    v
}

fn frame(t: &mut TouchInput, events: Vec<Event>, time: f64) -> Vec<Event> {
    let mut raw = RawInput { events, time: Some(time), ..Default::default() };
    t.route(&mut raw, false);
    raw.events
}

fn presses(events: &[Event]) -> Vec<(bool, Pos2)> {
    events.iter().filter_map(|e| if let Event::PointerButton { pressed, pos, .. } = e { Some((*pressed, *pos)) } else { None }).collect()
}

fn router() -> TouchInput {
    TouchInput { gestures: true, button_enabled: true, button: ButtonState { center: Some(pos2(40.0, 600.0)), ..Default::default() }, ..Default::default() }
}

#[test]
fn one_finger_is_the_pointer_once() {
    let mut t = router();
    let out = frame(&mut t, browser(1, TouchPhase::Start, 200.0, 200.0, true), 1.0);
    assert!(presses(&out).is_empty(), "the browser's press is gone, ours waits a moment: {out:?}");
    assert!(out.iter().any(|e| matches!(e, Event::Touch { .. })), "egui still sees the touch");
    let out = frame(&mut t, vec![], 1.0 + PRESS_WAIT_S);
    assert_eq!(presses(&out), vec![(true, pos2(200.0, 200.0))], "then the press, where the finger landed");
    let out = frame(&mut t, browser(1, TouchPhase::Move, 260.0, 210.0, true), 1.1);
    assert_eq!(out.iter().filter(|e| matches!(e, Event::PointerMoved(p) if *p == pos2(260.0, 210.0))).count(), 1);
    let out = frame(&mut t, browser(1, TouchPhase::End, 260.0, 210.0, true), 1.2);
    assert_eq!(presses(&out), vec![(false, pos2(260.0, 210.0))]);
    assert!(t.take_actions().is_empty());
}

#[test]
fn a_mouse_is_left_alone() {
    let mut t = router();
    let p = pos2(10.0, 10.0);
    let ev = vec![Event::PointerMoved(p), Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }];
    assert_eq!(frame(&mut t, ev.clone(), 1.0), ev);
    assert!(!t.touch_seen);
}

#[test]
fn the_modifier_finger_holds_shift_then_alt_and_never_reaches_egui() {
    let mut t = router();
    // The finger on the button is the browser's primary touch: its emulated pointer goes too.
    let out = frame(&mut t, browser(1, TouchPhase::Start, 42.0, 602.0, true), 1.0);
    assert_eq!(out, vec![Event::ModifiersChanged(Modifiers::SHIFT)], "only egui's Shift: no touch, no pointer");
    assert_eq!(t.held_modifiers(), Modifiers::SHIFT);
    // A drawing finger: the pointer, with Shift on its press and on the frame.
    let started = frame(&mut t, browser(2, TouchPhase::Start, 300.0, 300.0, false), 1.1);
    assert!(started.iter().all(|e| !matches!(e, Event::Touch { id: TouchId(1), .. })));
    let out = frame(&mut t, vec![], 1.1 + PRESS_WAIT_S);
    assert!(
        matches!(out.iter().find(|e| matches!(e, Event::PointerButton { .. })), Some(Event::PointerButton { pressed: true, modifiers, .. }) if modifiers.shift),
        "{out:?}"
    );
    // Slid out of the button: Alt.
    frame(&mut t, browser(1, TouchPhase::Move, 110.0, 600.0, true), 1.2);
    assert_eq!(t.held_modifiers(), Modifiers::ALT);
    assert!(t.button.alt);
    frame(&mut t, browser(1, TouchPhase::End, 110.0, 600.0, true), 1.3);
    assert_eq!(t.held_modifiers(), Modifiers::NONE);
    // The drawing finger's release.
    let out = frame(&mut t, browser(2, TouchPhase::End, 300.0, 300.0, false), 1.4);
    assert_eq!(presses(&out), vec![(false, pos2(300.0, 300.0))]);
}

#[test]
fn a_held_button_alone_moves() {
    let mut t = router();
    frame(&mut t, browser(1, TouchPhase::Start, 40.0, 600.0, true), 1.0);
    frame(&mut t, vec![], 1.6);
    assert!(t.button.moving);
    assert_eq!(t.held_modifiers(), Modifiers::NONE, "moving is no modifier");
    frame(&mut t, browser(1, TouchPhase::Move, 140.0, 500.0, true), 1.7);
    assert_eq!(t.button.center, Some(pos2(140.0, 500.0)));
    frame(&mut t, browser(1, TouchPhase::End, 140.0, 500.0, true), 1.8);
    assert!(!t.button.moving && !t.button.held);
}

#[test]
fn two_finger_tap_undoes_three_redo_four_hide() {
    for (n, want) in [(2, TouchAction::Undo), (3, TouchAction::Redo), (4, TouchAction::ToggleUi)] {
        let mut t = router();
        let mut ev = Vec::new();
        for k in 0..n {
            ev.extend(browser(k + 1, TouchPhase::Start, 200.0 + 50.0 * k as f32, 300.0, k == 0));
        }
        frame(&mut t, ev, 1.0);
        let mut ev = Vec::new();
        for k in 0..n {
            ev.extend(browser(k + 1, TouchPhase::End, 200.0 + 50.0 * k as f32, 300.0, k == 0));
        }
        let out = frame(&mut t, ev, 1.15);
        assert_eq!(t.take_actions(), vec![want], "{n} fingers");
        // The tool never heard of the fingers: no press, no release (no click under them).
        assert!(presses(&out).is_empty(), "{out:?}");
    }
}

#[test]
fn no_tap_after_the_tool_finger_drew() {
    let mut t = router();
    frame(&mut t, browser(1, TouchPhase::Start, 100.0, 100.0, true), 1.0);
    frame(&mut t, browser(1, TouchPhase::Move, 160.0, 100.0, true), 1.05);
    frame(&mut t, browser(2, TouchPhase::Start, 300.0, 100.0, false), 1.1);
    frame(&mut t, [browser(1, TouchPhase::End, 160.0, 100.0, true), browser(2, TouchPhase::End, 300.0, 100.0, false)].concat(), 1.2);
    assert!(t.take_actions().is_empty(), "a stroke cancelled by a second finger is no undo");
}

#[test]
fn a_slow_or_moving_pair_is_no_tap() {
    let mut t = router();
    frame(&mut t, [browser(1, TouchPhase::Start, 100.0, 100.0, true), browser(2, TouchPhase::Start, 200.0, 100.0, false)].concat(), 1.0);
    frame(&mut t, [browser(1, TouchPhase::Move, 80.0, 100.0, true), browser(2, TouchPhase::Move, 240.0, 100.0, false)].concat(), 1.1);
    frame(&mut t, [browser(1, TouchPhase::End, 80.0, 100.0, true), browser(2, TouchPhase::End, 240.0, 100.0, false)].concat(), 1.2);
    assert!(t.take_actions().is_empty(), "a pinch");
}

#[test]
fn two_fingers_held_still_repeat_undo() {
    let mut t = router();
    frame(&mut t, [browser(1, TouchPhase::Start, 100.0, 100.0, true), browser(2, TouchPhase::Start, 200.0, 100.0, false)].concat(), 1.0);
    let mut fired = 0;
    for k in 1..=16 {
        frame(&mut t, vec![], 1.0 + 0.1 * f64::from(k));
        fired += t.take_actions().iter().filter(|a| **a == TouchAction::Undo).count();
    }
    assert!(fired >= 4, "repeats: {fired}");
    frame(&mut t, [browser(1, TouchPhase::End, 100.0, 100.0, true), browser(2, TouchPhase::End, 200.0, 100.0, false)].concat(), 2.7);
    assert!(t.take_actions().is_empty(), "the release after repeats adds none");
}

#[test]
fn once_a_pen_is_seen_fingers_navigate() {
    let mut t = router();
    let mut raw = RawInput { time: Some(1.0), ..Default::default() };
    t.route(&mut raw, true);
    assert!(t.pen_seen && !t.fingers_paint());
    let out = frame(&mut t, browser(1, TouchPhase::Start, 100.0, 100.0, true), 2.0);
    assert!(presses(&out).is_empty(), "a finger doesn't paint: {out:?}");
    frame(&mut t, browser(1, TouchPhase::Move, 130.0, 110.0, true), 2.1);
    assert_eq!(t.take_pan(), egui::vec2(30.0, 10.0));
    frame(&mut t, browser(1, TouchPhase::End, 130.0, 110.0, true), 2.2);
    t.mode = StylusMode::FingerPaints;
    frame(&mut t, browser(2, TouchPhase::Start, 100.0, 100.0, true), 3.0);
    let out = frame(&mut t, vec![], 3.0 + PRESS_WAIT_S);
    assert_eq!(presses(&out).len(), 1, "Finger paints");
}

#[test]
fn pen_only_never_lets_a_finger_paint() {
    let mut t = TouchInput { mode: StylusMode::PenOnly, ..router() };
    let out = frame(&mut t, browser(1, TouchPhase::Start, 100.0, 100.0, true), 1.0);
    assert!(presses(&out).is_empty());
}

#[test]
fn a_quick_tap_is_a_click_and_a_late_second_finger_cancels_without_a_click() {
    let mut t = router();
    frame(&mut t, browser(1, TouchPhase::Start, 50.0, 50.0, true), 1.0);
    let out = frame(&mut t, browser(1, TouchPhase::End, 50.0, 50.0, true), 1.05);
    assert_eq!(presses(&out), vec![(true, pos2(50.0, 50.0)), (false, pos2(50.0, 50.0))], "press and release: a click");
    // A stroke under way, then a second finger: the press went out, so the release comes from
    // far off (no click).
    frame(&mut t, browser(1, TouchPhase::Start, 50.0, 50.0, true), 2.0);
    frame(&mut t, browser(1, TouchPhase::Move, 90.0, 50.0, true), 2.2);
    frame(&mut t, browser(2, TouchPhase::Start, 200.0, 50.0, false), 2.3);
    let out = frame(&mut t, [browser(1, TouchPhase::End, 90.0, 50.0, true), browser(2, TouchPhase::End, 200.0, 50.0, false)].concat(), 2.4);
    let rel: Vec<_> = presses(&out).into_iter().filter(|p| !p.0).collect();
    assert!(rel.len() == 1 && rel[0].1.x > 10_000.0, "{rel:?}");
}

/// eframe keeps emulating the first finger down: with the modifier finger first, a drawing
/// finger's touchstart brings a press at the modifier's place. It must not reach the tools.
#[test]
fn the_browser_s_press_at_the_modifier_finger_is_dropped() {
    let mut t = router();
    frame(&mut t, browser(1, TouchPhase::Start, 40.0, 600.0, true), 1.0);
    let mut ev = vec![Event::PointerButton { pos: pos2(40.0, 600.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }];
    ev.push(touch(2, TouchPhase::Start, 300.0, 300.0));
    let out = frame(&mut t, ev, 1.1);
    assert!(presses(&out).is_empty(), "{out:?}");
    let out = frame(&mut t, vec![], 1.1 + PRESS_WAIT_S);
    assert_eq!(presses(&out), vec![(true, pos2(300.0, 300.0))]);
}

/// Once fingers only navigate (a pen was used), a finger on the interface still taps it.
#[test]
fn after_a_pen_a_finger_still_taps_the_interface() {
    let mut t = router();
    t.pen_seen = true;
    let on_canvas = |p: Pos2| p.y < 800.0;
    let mut raw = RawInput { events: browser(1, TouchPhase::Start, 200.0, 850.0, true), time: Some(1.0), ..Default::default() };
    t.route_with(&mut raw, false, &on_canvas);
    let mut raw = RawInput { events: browser(1, TouchPhase::End, 200.0, 850.0, true), time: Some(1.05), ..Default::default() };
    t.route_with(&mut raw, false, &on_canvas);
    assert_eq!(presses(&raw.events).len(), 2, "a click on the bar: {:?}", raw.events);
    let mut raw = RawInput { events: browser(2, TouchPhase::Start, 200.0, 300.0, true), time: Some(2.0), ..Default::default() };
    t.route_with(&mut raw, false, &on_canvas);
    let mut raw = RawInput { events: vec![], time: Some(2.2), ..Default::default() };
    t.route_with(&mut raw, false, &on_canvas);
    assert!(presses(&raw.events).is_empty(), "on the canvas it navigates");
}

/// A finger's touch with a force (Android reports one) is no pen.
#[test]
fn a_finger_with_a_force_is_no_pen() {
    let mut t = router();
    t.pen_seen = true;
    let ev = vec![Event::Touch { device_id: TouchDeviceId(1), id: TouchId(3), phase: TouchPhase::Start, pos: pos2(100.0, 100.0), force: Some(1.0) }];
    frame(&mut t, ev, 1.0);
    let out = frame(&mut t, vec![], 1.0 + PRESS_WAIT_S);
    assert!(presses(&out).is_empty(), "it navigates");
}

/// A lost touchend: the platform's list of contacts lets the stale fingers go (no endless undo).
#[test]
fn a_lost_touch_end_does_not_leave_fingers_held() {
    let mut t = router();
    frame(&mut t, [browser(1, TouchPhase::Start, 100.0, 100.0, true), browser(2, TouchPhase::Start, 200.0, 100.0, false)].concat(), 1.0);
    // Both ends were lost; the next event says nothing is down.
    let mut raw = RawInput { time: Some(1.1), ..Default::default() };
    t.sync_active(&mut raw, &[]);
    t.route(&mut raw, false);
    t.take_actions();
    for k in 1..=10 {
        frame(&mut t, vec![], 1.1 + 0.2 * f64::from(k));
    }
    assert!(t.take_actions().is_empty(), "no repeating undo once the fingers are gone");
    frame(&mut t, browser(3, TouchPhase::Start, 50.0, 50.0, true), 4.0);
    let out = frame(&mut t, vec![], 4.0 + PRESS_WAIT_S + 0.01);
    assert_eq!(presses(&out).len(), 1, "a new finger paints again");
}

/// A slow, small two-finger twist (a slow phone takes long over it) is no held undo.
#[test]
fn a_slow_small_twist_does_not_repeat_undo() {
    let mut t = router();
    frame(&mut t, [browser(1, TouchPhase::Start, 140.0, 300.0, true), browser(2, TouchPhase::Start, 260.0, 300.0, false)].concat(), 1.0);
    for k in 1..=12 {
        // Each finger moves 8 pt over 1.2 s: the twist of a rotation, too slow for egui's eye.
        let d = 8.0 * k as f32 / 12.0;
        frame(
            &mut t,
            [browser(1, TouchPhase::Move, 140.0, 300.0 + d, true), browser(2, TouchPhase::Move, 260.0, 300.0 - d, false)].concat(),
            1.0 + 0.1 * f64::from(k),
        );
    }
    frame(&mut t, [browser(1, TouchPhase::End, 140.0, 308.0, true), browser(2, TouchPhase::End, 260.0, 292.0, false)].concat(), 2.4);
    assert!(t.take_actions().is_empty());
}

/// A double tap whose taps reach one frame (frames running late): the second tap waits for the
/// next frame, so the canvas and egui see two clicks, one per frame.
#[test]
fn a_double_tap_in_one_frame_clicks_twice() {
    let mut t = router();
    let mut events = browser(1, TouchPhase::Start, 200.0, 200.0, true);
    events.extend(browser(1, TouchPhase::End, 200.0, 200.0, true));
    events.extend(browser(2, TouchPhase::Start, 201.0, 200.0, true));
    events.extend(browser(2, TouchPhase::End, 201.0, 200.0, true));
    let out = frame(&mut t, events, 1.0);
    assert_eq!(presses(&out), vec![(true, pos2(200.0, 200.0)), (false, pos2(200.0, 200.0))]);
    assert!(t.wants_repaint() && t.has_carry());
    let out = frame(&mut t, vec![], 1.016);
    assert_eq!(presses(&out), vec![(true, pos2(201.0, 200.0)), (false, pos2(201.0, 200.0))]);
    assert!(!t.has_carry());
}
