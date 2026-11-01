//! A phone's Move tool selects what is tapped and puts a box on it: a tap on a layer's glyphs or
//! pixels selects it (Auto-Select on, whatever the preference), a tap on an empty spot deselects;
//! the selected layer shows its box with finger-sized corners and a rotate handle, a corner drag
//! scales it proportionally and a pinch inside the box scales and turns it, each committed when
//! the fingers lift as one step; and an action bar acts on the selected layer.

use egui::{Event, Pos2, Rect, TouchDeviceId, TouchId, TouchPhase, pos2, vec2};
use egui_kittest::Harness;
use photocraft_doc::LayerId;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

type H = Harness<'static, PhotocraftApp>;

/// A 360 × 780 phone with a page and two type layers; the Move tool.
fn phone() -> (H, LayerId, LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "on"}})).unwrap();
    app.run("file.new", json!({"width": 1200, "height": 1800, "background": "white"})).unwrap();
    let a = LayerId(app.run("type.create", json!({"text": "PRIMERO", "size": 120, "x": 150, "y": 500})).unwrap()["layer"].as_u64().unwrap());
    let b = LayerId(app.run("type.create", json!({"text": "SEGUNDO", "size": 120, "x": 150, "y": 1200})).unwrap()["layer"].as_u64().unwrap());
    app.ui.tool = Tool::Move;
    app.sync_views();
    let mut h = Harness::builder().with_size(vec2(360.0, 780.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    for _ in 0..4 {
        frame(&mut h, vec![]);
    }
    (h, a, b)
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

fn settle(h: &mut H) {
    for _ in 0..4 {
        frame(h, vec![]);
    }
}

fn tap(h: &mut H, p: Pos2) {
    frame(h, vec![touch(7, TouchPhase::Start, p)]);
    frame(h, vec![touch(7, TouchPhase::End, p)]);
    settle(h);
}

fn drag(h: &mut H, pts: &[Pos2]) {
    frame(h, vec![touch(1, TouchPhase::Start, pts[0])]);
    for p in &pts[1..] {
        frame(h, vec![touch(1, TouchPhase::Move, *p)]);
    }
    frame(h, vec![touch(1, TouchPhase::End, *pts.last().unwrap())]);
    settle(h);
}

/// Layer `id`'s drawn bounds in document pixels.
fn bounds(h: &H, id: LayerId) -> photocraft_geom::Rect {
    let st = h.state().session.active().unwrap();
    match st.doc.layer(id).map(|l| &l.content) {
        Some(photocraft_doc::LayerContent::Text(t)) => t.cache.as_ref().unwrap().content_bounds(),
        _ => panic!("not type"),
    }
}

/// A document point on screen.
fn screen(h: &H, x: f64, y: f64) -> Pos2 {
    crate::canvas::ViewXform::active(h.state()).unwrap().to_screen(x as f32, y as f32)
}

fn centre(h: &H, id: LayerId) -> Pos2 {
    let b = bounds(h, id);
    screen(h, f64::from(b.x0 + b.x1) / 2.0, f64::from(b.y0 + b.y1) / 2.0)
}

fn selected(h: &H) -> Vec<LayerId> {
    h.state().session.active().unwrap().selected_layers()
}

fn noted(h: &H, label: &str) -> Option<Rect> {
    let t = crate::touch_ui::inspect(h.state(), &h.ctx);
    t["rects"].as_array()?.iter().rev().find(|r| r["label"] == label).map(|r| {
        let v: [f32; 4] = serde_json::from_value(r["rect"].clone()).unwrap();
        Rect::from_min_max(pos2(v[0], v[1]), pos2(v[2], v[3]))
    })
}

fn controls(h: &H) -> Option<Rect> {
    crate::canvas::transform_controls_rect_pub(h.state(), &crate::canvas::ViewXform::active(h.state()).unwrap())
}

/// A tap selects the type layer under the finger (of two), with its box and action bar; a tap on
/// an empty spot deselects and both go.
#[test]
fn a_tap_selects_the_layer_under_the_finger_and_an_empty_tap_deselects() {
    let (mut h, a, b) = phone();
    let pb = centre(&h, b);
    tap(&mut h, pb);
    assert_eq!(selected(&h), [b], "the tapped layer");
    assert!(controls(&h).is_some(), "its box shows");
    assert!(noted(&h, "layerBar").is_some(), "its actions show");
    assert!(noted(&h, "layer:type.editText").is_some(), "type actions for type");
    let pa = centre(&h, a);
    tap(&mut h, pa);
    assert_eq!(selected(&h), [a], "the other one");
    // An empty spot of the page.
    let empty = screen(&h, 1000.0, 900.0);
    tap(&mut h, empty);
    assert!(selected(&h).is_empty(), "deselected");
    assert!(controls(&h).is_none() && noted(&h, "layerBar").is_none(), "no box, no actions");
}

/// A corner of the box dragged out scales the layer, keeping its proportions, in one step.
#[test]
fn a_corner_drag_scales_proportionally_in_one_step() {
    let (mut h, a, _) = phone();
    let pa = centre(&h, a);
    tap(&mut h, pa);
    let past = h.state().session.active().unwrap().history.past_len();
    let b0 = bounds(&h, a);
    let r = controls(&h).expect("the box");
    let c = r.right_bottom();
    drag(&mut h, &[c, c + vec2(10.0, 4.0), c + vec2(30.0, 10.0), c + vec2(40.0, 14.0)]);
    assert!(h.state().ui.transform.is_none(), "committed when the finger lifted");
    let b1 = bounds(&h, a);
    assert!(b1.width() > b0.width() + 40, "bigger: {b0:?} → {b1:?}");
    let (k0, k1) = (f64::from(b0.width()) / f64::from(b0.height()), f64::from(b1.width()) / f64::from(b1.height()));
    assert!((k0 - k1).abs() / k0 < 0.06, "the same proportions: {k0:.3} → {k1:.3}");
    assert_eq!(h.state().session.active().unwrap().history.past_len(), past + 1, "one step");
}

/// The action bar's Delete removes the selected layer, in one step Undo takes back.
#[test]
fn the_delete_button_removes_the_layer_with_one_undo() {
    let (mut h, a, b) = phone();
    let pa = centre(&h, a);
    tap(&mut h, pa);
    // Delete is past the type actions: the bar scrolls to it.
    let del = noted(&h, "layer:layer.delete").expect("Delete in the bar");
    let bar = noted(&h, "layerBar").unwrap();
    if del.right() > bar.right() {
        let y = bar.center().y;
        drag(&mut h, &[pos2(bar.right() - 20.0, y), pos2(bar.right() - 120.0, y), pos2(bar.left() + 20.0, y)]);
    }
    let del = noted(&h, "layer:layer.delete").expect("Delete in the bar");
    assert!(del.right() <= bar.right() + 1.0, "in view: {del:?} {bar:?}");
    tap(&mut h, del.center());
    assert!(h.state().session.active().unwrap().doc.layer(a).is_none(), "deleted");
    assert!(h.state().session.active().unwrap().doc.layer(b).is_some(), "the other stays");
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert!(h.state().session.active().unwrap().doc.layer(a).is_some(), "one undo brings it back");
}

/// Two fingers spread inside the selected layer's box scale it (committed when they lift);
/// outside it they navigate.
#[test]
fn a_pinch_inside_the_box_scales_the_layer() {
    let (mut h, a, _) = phone();
    let pa = centre(&h, a);
    tap(&mut h, pa);
    let b0 = bounds(&h, a);
    let past = h.state().session.active().unwrap().history.past_len();
    let c = controls(&h).unwrap().center();
    frame(&mut h, vec![touch(1, TouchPhase::Start, c - vec2(20.0, 0.0)), touch(2, TouchPhase::Start, c + vec2(20.0, 0.0))]);
    for k in 1..=8 {
        let d = 20.0 + 6.0 * k as f32;
        frame(&mut h, vec![touch(1, TouchPhase::Move, c - vec2(d, 0.0)), touch(2, TouchPhase::Move, c + vec2(d, 0.0))]);
    }
    frame(&mut h, vec![touch(1, TouchPhase::End, c - vec2(68.0, 0.0)), touch(2, TouchPhase::End, c + vec2(68.0, 0.0))]);
    settle(&mut h);
    assert!(h.state().ui.transform.is_none(), "committed when the fingers lifted");
    let b1 = bounds(&h, a);
    assert!(b1.width() > b0.width() * 3 / 2, "scaled up: {b0:?} → {b1:?}");
    assert_eq!(h.state().session.active().unwrap().history.past_len(), past + 1, "one step");
}
