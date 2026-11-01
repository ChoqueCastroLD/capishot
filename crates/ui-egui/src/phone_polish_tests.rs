//! The phone's floating controls in the real app: none covers another or leaves the screen at
//! 320–430 points in any state (idle, painting, typing, an AI run), the touch chips stand in for
//! Shift and Alt, the AI card is one line, the brush starts at a size that suits the page, the
//! view follows the caret, a twist doesn't turn the view and the type bar keeps to one row.

use std::sync::{Mutex, mpsc};

use egui::{Event, Pos2, Rect, TouchDeviceId, TouchId, TouchPhase, pos2, vec2};
use egui_kittest::Harness;
use photocraft_engine::jobs::Started;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::touch_locks::Lock;

type H = Harness<'static, PhotocraftApp>;

fn phone(w: f32, h: f32) -> H {
    phone_with(w, h, |_| {})
}

fn phone_with(w: f32, h: f32, setup: impl FnOnce(&mut PhotocraftApp)) -> H {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "on"}})).unwrap();
    app.run("file.new", json!({"width": 1200, "height": 1800, "background": "white"})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.background_jobs = true;
    setup(&mut app);
    app.sync_views();
    let mut hh = Harness::builder().with_size(vec2(w, h)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    hh.run_steps(4);
    hh
}

/// One frame with these events, routed as the browser's are (`touch_ui::route`).
fn frame(h: &mut H, events: Vec<Event>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    raw.events.extend(events);
    let t = ctx.input(|i| i.time) + 1.0 / 30.0;
    raw.time = Some(t);
    crate::touch_ui::route(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn touch(id: u64, phase: TouchPhase, p: Pos2) -> Event {
    Event::Touch { device_id: TouchDeviceId(1), id: TouchId(id), phase, pos: p, force: None }
}

fn finger_drag(h: &mut H, pts: &[Pos2]) {
    frame(h, vec![touch(1, TouchPhase::Start, pts[0])]);
    for p in &pts[1..] {
        frame(h, vec![touch(1, TouchPhase::Move, *p)]);
    }
    frame(h, vec![touch(1, TouchPhase::End, *pts.last().unwrap())]);
    h.run_steps(3);
}

fn tap(h: &mut H, p: Pos2) {
    frame(h, vec![touch(7, TouchPhase::Start, p)]);
    frame(h, vec![touch(7, TouchPhase::End, p)]);
    h.run_steps(3);
}

/// Where control `label` is this frame, as `ui.inspect` reports it.
fn noted(h: &H, label: &str) -> Option<Rect> {
    controls(h).into_iter().find(|(l, _)| l == label).map(|(_, r)| r)
}

/// Frames through the touch router (it decides whether the modifier button shows).
fn settle(h: &mut H, n: usize) {
    for _ in 0..n {
        frame(h, vec![]);
    }
}

fn canvas_center(h: &H) -> Pos2 {
    h.state().last_canvas_rect.center()
}

fn start_typing(h: &mut H) {
    h.state_mut().ui.tool = Tool::Type;
    h.run_steps(2);
    let c = canvas_center(h);
    tap(h, c);
    frame(h, vec![Event::Text("Hola".into())]);
    h.run_steps(2);
    assert!(h.state().ui.text_edit.is_some(), "typing");
}

/// The floating controls drawn this frame, by label (the last place of each).
fn controls(h: &H) -> Vec<(String, Rect)> {
    let mut all: Vec<(String, Rect)> = Vec::new();
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    let s = crate::simple_ui::inspect(h.state(), &h.ctx);
    for r in t["rects"].as_array().into_iter().flatten().chain(s["buttons"].as_array().into_iter().flatten()) {
        let label = r["label"].as_str().unwrap_or_default().to_string();
        let v: [f32; 4] = serde_json::from_value(r["rect"].clone()).unwrap();
        let rect = Rect::from_min_max(pos2(v[0], v[1]), pos2(v[2], v[3]));
        all.retain(|(l, _)| *l != label);
        all.push((label, rect));
    }
    // Drawn over the canvas on purpose, for a moment, by the finger that asked for them; and the
    // selection bar's buttons, which scroll inside it (the bar itself is checked).
    all.retain(|(l, _)| {
        !matches!(l.as_str(), "brushPreview" | "loupe" | "sliderBubble" | "selectionBarMore") && !l.starts_with("sel:") && !l.starts_with("layer:")
    });
    all
}

/// `child` is drawn inside `parent` by design (a bar's buttons).
fn inside(child: &str, parent: &str) -> bool {
    (parent == "typeBar" && child.starts_with("type"))
        || (parent == "selectionBar" && (child.starts_with("sel:") || child == "selectionBarMore"))
        || (parent == "typeLayerBar" && child.starts_with("typeLayer:"))
}

/// No two controls overlap (but a bar and its own buttons) and none leaves the screen.
fn assert_clear(h: &H, what: &str) {
    let screen = h.ctx.content_rect();
    let all = controls(h);
    assert!(!all.is_empty(), "{what}: controls were noted");
    for (l, r) in &all {
        assert!(screen.expand(0.5).contains_rect(*r), "{what}: {l} {r:?} leaves the screen {screen:?}");
    }
    for (i, (a, ra)) in all.iter().enumerate() {
        for (b, rb) in &all[i + 1..] {
            if inside(a, b) || inside(b, a) {
                continue;
            }
            let o = ra.intersect(*rb);
            assert!(o.width() <= 0.5 || o.height() <= 0.5, "{what}: {a} {ra:?} overlaps {b} {rb:?}");
        }
    }
}

const PHONES: [(f32, f32); 5] = [(320.0, 640.0), (360.0, 780.0), (390.0, 844.0), (412.0, 915.0), (430.0, 932.0)];

/// A job of Remove with AI that runs until `release` is sent.
fn ai_job(h: &mut H) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    let rx = Mutex::new(rx);
    let id = photocraft_engine::remove_ai_cmds::ID;
    let params = json!({"strokes": [[[300, 400, 1], [600, 500, 1]]], "size": 40});
    let started = h
        .state_mut()
        .session
        .start_job(
            id,
            params.clone(),
            "Remove with AI",
            false,
            move |ctx| {
                ctx.progress(0.3, "Remove with AI (1 of 3)");
                rx.lock().unwrap().recv().ok();
                Ok(())
            },
            |_, ()| Ok(json!({})),
        )
        .unwrap();
    let Started::Job(job) = started else { panic!("a job") };
    crate::ai_progress::note_start(h.state_mut(), id, &params);
    crate::ai_progress::started(h.state_mut(), id, &params, job);
    h.run_steps(3);
    tx
}

/// Every floating control keeps clear of the others and of the screen's edges, at 320 to 430
/// points: idle, a stroke painting, typing (with the keyboard up too), an AI run, a selection,
/// the opt-in modifier button.
#[test]
fn no_floating_control_covers_another_or_leaves_the_screen() {
    for (w, hgt) in PHONES {
        // Idle with the Brush, and a status toast.
        let mut h = phone(w, hgt);
        h.state_mut().ui.tool = Tool::Brush;
        h.run_steps(3);
        assert_clear(&h, &format!("{w}: idle"));
        h.state_mut().ui.status = "Straight line: on".into();
        h.run_steps(2);
        assert!(noted(&h, "toast").is_some(), "{w}: a toast shows");
        assert_clear(&h, &format!("{w}: toast"));
        // Painting: the sliders step aside while the finger is down.
        let c = canvas_center(&h) + vec2(40.0, 0.0);
        frame(&mut h, vec![touch(1, TouchPhase::Start, c)]);
        frame(&mut h, vec![touch(1, TouchPhase::Move, c + vec2(30.0, 10.0))]);
        frame(&mut h, vec![touch(1, TouchPhase::Move, c + vec2(60.0, 20.0))]);
        assert!(noted(&h, "sizeSlider").is_none(), "{w}: the sliders hide while painting");
        assert_clear(&h, &format!("{w}: painting"));
        frame(&mut h, vec![touch(1, TouchPhase::End, c + vec2(60.0, 20.0))]);
        h.run_steps(3);
        assert!(noted(&h, "sizeSlider").is_some(), "{w}: the sliders come back");
        // A selection: its bar at the top, the sliders below it.
        h.state_mut().run("select.rect", json!({"x": 100, "y": 100, "width": 300, "height": 300})).unwrap();
        h.run_steps(3);
        assert!(noted(&h, "selectionBar").is_some(), "{w}: the selection bar shows");
        assert_clear(&h, &format!("{w}: selection"));
        h.state_mut().run("select.deselect", json!({})).unwrap();
        // An AI run: the one-line card, the sliders under it.
        let release = ai_job(&mut h);
        assert!(noted(&h, "aiCard").is_some(), "{w}: the AI card shows");
        assert_clear(&h, &format!("{w}: AI running"));
        release.send(()).ok();
        // Typing, then with the keyboard up (the window shrinks).
        let mut h = phone(w, hgt);
        start_typing(&mut h);
        assert_clear(&h, &format!("{w}: typing"));
        h.set_size(vec2(w, hgt - 300.0));
        h.run_steps(4);
        assert_clear(&h, &format!("{w}: typing, keyboard up"));
        // The modifier button, opted in.
        let mut h = phone_with(w, hgt, |a| {
            a.run("prefs.set", json!({"values": {"interface.simpleModifierButton": true}})).unwrap();
        });
        h.state_mut().ui.tool = Tool::Brush;
        settle(&mut h, 3);
        assert!(noted(&h, "modifier").is_some(), "{w}: the button shows when asked for");
        assert_clear(&h, &format!("{w}: modifier button"));
    }
}

/// The sliders' place: on the left edge, around the middle, below what floats at the top and
/// above the modifier button.
#[test]
fn the_quick_sliders_keep_to_the_left_edge_between_the_cards() {
    let area = Rect::from_min_max(pos2(0.0, 88.0), pos2(360.0, 658.0));
    let (s, o) = crate::touch_ui::quick_slider_rects(area, None, None);
    assert_eq!(s.left(), 4.0);
    assert!(s.width() <= 30.0, "slim");
    assert!(o.top() > s.bottom(), "one above the other");
    let mid = (s.top() + o.bottom()) / 2.0;
    assert!((mid - area.center().y).abs() < 2.0, "around the middle: {mid}");
    let card = Rect::from_min_max(pos2(30.0, 96.0), pos2(330.0, 400.0));
    let (s, o) = crate::touch_ui::quick_slider_rects(area, Some(card), Some(560.0));
    assert!(s.top() >= card.bottom(), "below the card");
    assert!(o.bottom() <= 560.0, "above the button");
}

/// The simple interface has no modifier button unless asked for; the desktop's touch screens
/// keep it.
#[test]
fn the_modifier_button_is_opt_in_on_the_phone() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::RectMarquee;
    settle(&mut h, 3);
    assert!(!h.state().touch.button_enabled && noted(&h, "modifier").is_none(), "no button by default");
    h.state_mut().run("prefs.set", json!({"values": {"interface.simpleModifierButton": true}})).unwrap();
    settle(&mut h, 3);
    assert!(noted(&h, "modifier").is_some(), "shown when asked for");
}

/// Every tool that reads Shift, Alt or Ctrl in a way a phone needs has its chips in the tool row.
#[test]
fn the_tools_that_read_modifiers_have_touch_chips() {
    let mut h = phone(360.0, 780.0);
    let cases: [(Tool, &[Lock]); 14] = [
        (Tool::RectMarquee, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::EllipseMarquee, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::Move, &[Lock::Duplicate, Lock::Axis]),
        (Tool::Brush, &[Lock::Straight]),
        (Tool::Pencil, &[Lock::Straight]),
        (Tool::Eraser, &[Lock::Straight]),
        (Tool::CloneStamp, &[Lock::SetSource]),
        (Tool::Healing, &[Lock::SetSource]),
        (Tool::Rectangle, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::EllipseShape, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::Polygon, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::Line, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::CustomShape, &[Lock::Constrain, Lock::FromCenter]),
        (Tool::Gradient, &[Lock::Angle45]),
    ];
    for (tool, want) in cases {
        h.state_mut().ui.tool = tool;
        h.run_steps(3);
        assert_eq!(crate::touch_locks::chips(h.state()), want.to_vec(), "{tool:?}");
        for lock in want {
            let r = noted(&h, &format!("lock:{}", lock.key())).unwrap_or_else(|| panic!("{tool:?}: the {lock:?} chip is in the row"));
            assert!(h.ctx.content_rect().contains_rect(r), "{tool:?}: {lock:?} on screen");
        }
    }
    // The Crop tool: its ratio presets in the row.
    h.state_mut().ui.tool = Tool::Crop;
    h.run_steps(3);
    use egui_kittest::kittest::Queryable;
    assert!(h.query_by_label("Aspect Ratio").is_some(), "the crop ratio presets");
    // Free Transform: Proportional (on) and From Center.
    h.state_mut().ui.tool = Tool::Move;
    h.state_mut().run("select.rect", json!({"x": 100, "y": 100, "width": 200, "height": 200})).unwrap();
    h.state_mut().run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(crate::touch_locks::chips(h.state()), vec![Lock::Proportional, Lock::FromCenter]);
    assert!(crate::touch_locks::is_on(h.state(), Lock::Proportional), "proportional by default on a phone");
}

fn selection_bounds(h: &mut H) -> [f64; 4] {
    let v = h.state_mut().run("document.inspect", json!({})).unwrap();
    serde_json::from_value(v["selectionBounds"].clone()).expect("a selection")
}

/// Square on: a marquee drag makes a square, and replaces the selection (the press is no Shift,
/// which would add); From Center on: the press is the middle.
#[test]
fn the_marquee_chips_make_a_square_from_the_center() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::RectMarquee;
    h.run_steps(3);
    let c = canvas_center(&h);
    finger_drag(&mut h, &[c + vec2(-120.0, -150.0), c + vec2(-100.0, -130.0), c + vec2(-80.0, -120.0)]);
    let at = noted(&h, "lock:constrain").expect("Square").center();
    tap(&mut h, at);
    assert!(h.state().touch.locks.constrain);
    finger_drag(&mut h, &[c, c + vec2(40.0, 10.0), c + vec2(80.0, 30.0)]);
    // `selectionBounds` is [x, y, width, height].
    let b = selection_bounds(&mut h);
    assert!((b[2] - b[3]).abs() <= 1.0, "a square: {b:?}");
    let press = crate::canvas::doc_point_at(h.state(), c).unwrap();
    assert!(b[0] >= press[0] - 1.0 && b[1] >= press[1] - 1.0, "the earlier selection was replaced, not added to: {b:?}");
    let at = noted(&h, "lock:fromCenter").expect("From Center").center();
    tap(&mut h, at);
    // (A drag that starts inside a selection moves it.)
    h.state_mut().run("select.deselect", json!({})).unwrap();
    finger_drag(&mut h, &[c, c + vec2(30.0, 30.0), c + vec2(50.0, 50.0)]);
    let b = selection_bounds(&mut h);
    let mid = [b[0] + b[2] / 2.0, b[1] + b[3] / 2.0];
    assert!((mid[0] - press[0]).abs() <= 2.0 && (mid[1] - press[1]).abs() <= 2.0, "drawn from the middle: {b:?} around {press:?}");
}

fn layer_count(h: &mut H) -> usize {
    h.state_mut().run("layer.list", json!({"flat": true})).unwrap()["layers"].as_array().map_or(0, Vec::len)
}

/// Duplicate on: the next Move drag moves a copy, once.
#[test]
fn the_move_duplicate_chip_copies_once() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().run("select.rect", json!({"x": 400, "y": 600, "width": 400, "height": 400})).unwrap();
    h.state_mut().run("edit.fill", json!({"color": "#3366ff"})).unwrap();
    h.state_mut().run("select.deselect", json!({})).unwrap();
    h.state_mut().ui.tool = Tool::Move;
    h.run_steps(3);
    let n = layer_count(&mut h);
    let at = noted(&h, "lock:duplicate").expect("Duplicate").center();
    tap(&mut h, at);
    let c = canvas_center(&h);
    finger_drag(&mut h, &[c, c + vec2(20.0, 0.0), c + vec2(40.0, 0.0)]);
    assert_eq!(layer_count(&mut h), n + 1, "a copy moved");
    assert!(!h.state().touch.locks.duplicate, "one gesture");
    finger_drag(&mut h, &[c, c + vec2(20.0, 0.0), c + vec2(40.0, 0.0)]);
    assert_eq!(layer_count(&mut h), n + 1, "the next drag moves without copying");
}

/// The chips act as their keys only for their tool, and a marquee's only once a drag has begun.
#[test]
fn the_chips_hold_their_keys_only_where_they_mean_it() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().touch.locks = crate::touch_locks::TouchLocks { constrain: true, from_center: true, straight: true, ..Default::default() };
    h.state_mut().ui.tool = Tool::RectMarquee;
    let m = crate::workspace_ui::sticky_mods(h.state(), egui::Modifiers::NONE);
    assert!(!m.shift && !m.alt, "no Shift at a marquee's press (it would add)");
    h.state_mut().ui.tool = Tool::Brush;
    assert!(crate::workspace_ui::sticky_mods(h.state(), egui::Modifiers::NONE).shift, "Straight Line holds Shift for the brush");
    h.state_mut().ui.tool = Tool::Lasso;
    assert_eq!(crate::workspace_ui::sticky_mods(h.state(), egui::Modifiers::NONE), egui::Modifiers::NONE, "nothing for other tools");
}

/// The phone's brush starts at about a 25th of the page's side in view.
#[test]
fn the_phone_brush_starts_at_a_size_for_the_page() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::Brush;
    h.run_steps(3);
    let canvas = h.state().last_canvas_rect;
    let want = crate::touch_ui::phone_brush_size(canvas, h.state().point_zoom(), [1200, 1800]);
    assert!((40.0..=56.0).contains(&want), "about 1200 / 25: {want}");
    assert_eq!(h.state().session.tools.brush.size, want);
    // A size set by hand stays the user's for the next page.
    h.state_mut().session.tools.brush.size = 13.0;
    h.state_mut().run("file.new", json!({"width": 3000, "height": 3000, "background": "white"})).unwrap();
    h.state_mut().sync_views();
    h.run_steps(4);
    assert_eq!(h.state().session.tools.brush.size, 13.0);
}

/// While typing nothing floats over the text: no Capi button, no sliders, no compass.
#[test]
fn typing_hides_the_floating_controls() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.views[0].rotation = 20.0;
    h.state_mut().ui.tool = Tool::Brush;
    h.run_steps(3);
    assert!(noted(&h, "capi").is_some() && noted(&h, "sizeSlider").is_some() && noted(&h, "compass").is_some());
    start_typing(&mut h);
    for l in ["capi", "sizeSlider", "opacitySlider", "compass", "modifier"] {
        assert!(noted(&h, l).is_none(), "{l} hides while typing");
    }
}

/// The view follows the caret above the type bar and the keyboard; a hand pan stops it until
/// the next edit.
#[test]
fn the_view_follows_the_caret_until_a_hand_moves_it() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::Type;
    h.run_steps(2);
    // Text low on the page, then the keyboard takes the bottom half.
    let c = canvas_center(&h);
    tap(&mut h, c + vec2(0.0, 200.0));
    frame(&mut h, vec![Event::Text("Hola".into())]);
    h.set_size(vec2(360.0, 460.0));
    h.run_steps(30);
    let caret = crate::type_tool::caret_screen_rect(h.state_mut()).expect("a caret");
    let bar = noted(&h, "typeBar").expect("the type bar");
    assert!(caret.bottom() < bar.top() && caret.top() > h.state().last_canvas_rect.top(), "the caret is in view above the bar: {caret:?} {bar:?}");
    // A hand moves the view: it stays there.
    let i = h.state().session.active_index().unwrap();
    h.state_mut().ui.views[i].center[1] += 1500.0;
    let moved = h.state().ui.views[i].center;
    h.run_steps(10);
    assert_eq!(h.state().ui.views[i].center, moved, "a manual move pauses the follow");
    // The next letter follows again.
    frame(&mut h, vec![Event::Text("!".into())]);
    h.run_steps(30);
    assert_ne!(h.state().ui.views[i].center, moved, "the next edit follows again");
}

/// A two-finger twist doesn't turn the view in the simple interface unless asked for; the
/// compass turns it back to 0 over a moment.
#[test]
fn twisting_needs_the_preference_and_the_compass_turns_back_smoothly() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::Hand;
    h.run_steps(2);
    let c = canvas_center(&h);
    let twist = |h: &mut H| {
        frame(h, vec![touch(1, TouchPhase::Start, c + vec2(-60.0, 0.0)), touch(2, TouchPhase::Start, c + vec2(60.0, 0.0))]);
        for k in 1..=8 {
            let a = k as f32 * 0.06;
            let d = vec2(60.0 * a.cos(), 60.0 * a.sin());
            frame(h, vec![touch(1, TouchPhase::Move, c - d), touch(2, TouchPhase::Move, c + d)]);
        }
        frame(h, vec![touch(1, TouchPhase::End, c), touch(2, TouchPhase::End, c)]);
        h.run_steps(3);
    };
    twist(&mut h);
    assert_eq!(h.state().ui.views[0].rotation, 0.0, "no turn by default");
    h.state_mut().run("prefs.set", json!({"values": {"interface.simpleTwoFingerRotate": true}})).unwrap();
    h.run_steps(2);
    twist(&mut h);
    let r = h.state().ui.views[0].rotation;
    assert!(r.abs() > 10.0, "the twist turns the view when asked for: {r}");
    let compass = noted(&h, "compass").expect("the compass shows when turned");
    frame(&mut h, vec![touch(7, TouchPhase::Start, compass.center())]);
    frame(&mut h, vec![touch(7, TouchPhase::End, compass.center())]);
    settle(&mut h, 2);
    let mid = h.state().ui.views[0].rotation;
    assert!(mid != 0.0 && mid.abs() < r.abs(), "on its way back: {mid}");
    settle(&mut h, 20);
    assert_eq!(h.state().ui.views[0].rotation, 0.0, "back to 0");
    assert!(noted(&h, "compass").is_none(), "hidden at 0");
}

/// The narrow type bar is one row (font, size, colour, "⋯", Done); "⋯" opens the second, and it
/// stays open for the next edit.
#[test]
fn the_type_bar_is_one_row_and_remembers_its_second() {
    let mut h = phone(360.0, 780.0);
    start_typing(&mut h);
    let bar = noted(&h, "typeBar").expect("the bar");
    for l in ["typeColor", "typeMore", "typeDone"] {
        let r = noted(&h, l).unwrap_or_else(|| panic!("{l}"));
        assert!(bar.contains_rect(r), "{l} in the bar");
    }
    let one = bar.height();
    assert!(one < 70.0, "one row: {one}");
    let at = noted(&h, "typeMore").unwrap().center();
    tap(&mut h, at);
    let two = noted(&h, "typeBar").unwrap().height();
    assert!(two > one + 30.0, "two rows: {two}");
    let at = noted(&h, "typeDone").unwrap().center();
    tap(&mut h, at);
    start_typing(&mut h);
    assert!(noted(&h, "typeBar").unwrap().height() > one + 30.0, "still open for the next edit");
}

/// On a phone the AI card is one thin line ("Removing… 3 s", a bar, ✕); a tap opens the full card.
#[test]
fn the_ai_card_is_one_line_until_tapped() {
    let mut h = phone(360.0, 780.0);
    let release = ai_job(&mut h);
    let card = crate::ai_progress::inspect(&h.ctx);
    assert_eq!(card["compact"], true);
    assert_eq!(card["text"], "Removing…");
    let r: [f32; 4] = serde_json::from_value(card["card"].clone()).unwrap();
    assert!(r[3] - r[1] <= crate::ai_progress::COMPACT_H + 0.5, "one thin line: {r:?}");
    tap(&mut h, pos2(r[0] + 60.0, (r[1] + r[3]) / 2.0));
    let card = crate::ai_progress::inspect(&h.ctx);
    assert_eq!(card["compact"], false, "the full card");
    let full: [f32; 4] = serde_json::from_value(card["card"].clone()).unwrap();
    assert!(full[3] - full[1] > r[3] - r[1] + 20.0, "taller: {full:?}");
    // ✕ on the full card cancels.
    use egui_kittest::kittest::Queryable;
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(crate::ai_progress::inspect(&h.ctx).is_null() || crate::ai_progress::inspect(&h.ctx)["kind"] != "progress", "cancelled");
    release.send(()).ok();
}

/// Set Source on: the next tap with the Clone Stamp picks its source (Alt-click), once.
#[test]
fn the_set_source_chip_picks_the_clone_source_once() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::CloneStamp;
    h.run_steps(3);
    let at = noted(&h, "lock:setSource").expect("Set Source").center();
    tap(&mut h, at);
    assert!(h.state().touch.locks.set_source);
    let c = canvas_center(&h);
    tap(&mut h, c);
    assert!(h.state().ui.clone_source.is_some(), "the tap set the source: {:?}", h.state().ui.status);
    assert!(!h.state().touch.locks.set_source, "one gesture");
}

/// Straight Line on: a brush tap after a stroke joins it with a straight line (Shift-click).
#[test]
fn the_straight_line_chip_joins_brush_taps() {
    let mut h = phone(360.0, 780.0);
    h.state_mut().ui.tool = Tool::Brush;
    h.run_steps(3);
    let c = canvas_center(&h);
    tap(&mut h, c + vec2(40.0, -80.0));
    let at = noted(&h, "lock:straight").expect("Straight Line").center();
    tap(&mut h, at);
    tap(&mut h, c + vec2(40.0, 80.0));
    // The pixel half way between the two taps is painted: a line joined them.
    let mid = crate::canvas::doc_point_at(h.state(), c + vec2(40.0, 0.0)).unwrap();
    let st = h.state().session.active().unwrap();
    let (x, y) = (mid[0] as i32, mid[1] as i32);
    let px = photocraft_compose::render(&st.doc, photocraft_geom::Rect { x0: x, y0: y, x1: x + 1, y1: y + 1 });
    let painted = px.px.first().is_some_and(|p| p[0] < 0.5);
    assert!(painted, "a straight line between the taps");
}
