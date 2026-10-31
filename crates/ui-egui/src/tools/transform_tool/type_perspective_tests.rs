//! Type → Perspective: the type layer becomes a smart object (its text editable inside) and the
//! box opens in Perspective mode; OK is one Perspective step, Cancel puts the type back. Edit ›
//! Transform › Perspective and Distort on smart objects and pixel layers. A double-click edits
//! the type inside and saves it back.

use photocraft_doc::{LayerContent, LayerId};
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::TransformMode;

fn app_with_type() -> (PhotocraftApp, LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 600, "height": 400, "background": "white"})).unwrap();
    app.sync_views();
    let id = LayerId(app.run("type.create", json!({"text": "PERSPECTIVA", "size": 60, "x": 100, "y": 200})).unwrap()["layer"].as_u64().unwrap());
    (app, id)
}

fn kind(app: &PhotocraftApp) -> &'static str {
    let st = app.session.active().unwrap();
    match st.active_layer.and_then(|id| st.doc.layer(id)).map(|l| &l.content) {
        Some(LayerContent::Text(_)) => "text",
        Some(LayerContent::Smart(_)) => "smart",
        Some(_) => "other",
        None => "none",
    }
}

/// Pull the top corners in, a symmetric (perspective) change.
fn pull_top_in(app: &mut PhotocraftApp) {
    let t = app.ui.transform.as_mut().unwrap();
    let w = t.quad[1][0] - t.quad[0][0];
    t.quad[0][0] += w * 0.15;
    t.quad[1][0] -= w * 0.15;
}

#[test]
fn type_perspective_converts_opens_the_box_and_commits_one_step() {
    let (mut app, _) = app_with_type();
    let ctx = egui::Context::default();
    let past = app.session.active().unwrap().history.past_len();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    assert_eq!(kind(&app), "smart", "the type became a smart object");
    let t = app.ui.transform.as_ref().expect("the box is open");
    assert_eq!(t.mode, TransformMode::Perspective);
    // Free: each corner on its own.
    app.ui.transform.as_mut().unwrap().mode = TransformMode::Distort;
    pull_top_in(&mut app);
    super::commit(&mut app);
    assert!(app.ui.transform.is_none());
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1, "conversion and transform: one step");
    assert_eq!(st.history.entries().last().map(ToString::to_string).as_deref(), Some("Perspective"));
    let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
    let LayerContent::Smart(so) = &l.content else { panic!("not smart") };
    assert!(so.perspective.is_some(), "a projective placement");
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(kind(&app), "text", "one undo: the type layer again");
}

#[test]
fn cancel_puts_the_type_layer_back() {
    let (mut app, id) = app_with_type();
    let ctx = egui::Context::default();
    let past = app.session.active().unwrap().history.past_len();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    pull_top_in(&mut app);
    super::cancel(&mut app);
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), past, "nothing left in the history");
    assert!(matches!(st.doc.layer(id).map(|l| &l.content), Some(LayerContent::Text(_))), "the type layer is back");
}

#[test]
fn edit_transform_perspective_and_distort_work_on_smart_objects_and_pixel_layers() {
    let ctx = egui::Context::default();
    for smart in [false, true] {
        for cmd in ["edit.transform.perspective", "edit.transform.distort"] {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            app.run("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
            app.run("layer.new.layer", json!({})).unwrap();
            app.run("select.rect", json!({"x": 50, "y": 50, "width": 120, "height": 80})).unwrap();
            app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
            app.run("select.deselect", json!({})).unwrap();
            if smart {
                app.run("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
            }
            app.sync_views();
            assert!(crate::menus::is_enabled(&app, cmd), "{cmd} on {}", if smart { "smart" } else { "pixels" });
            crate::menus::invoke(&mut app, &ctx, cmd, json!({})).unwrap();
            assert_eq!(app.ui.transform.as_ref().unwrap().mode, TransformMode::for_command(cmd));
            let past = app.session.active().unwrap().history.past_len();
            pull_top_in(&mut app);
            super::commit(&mut app);
            assert_eq!(app.session.active().unwrap().history.past_len(), past + 1, "{cmd} smart={smart}: applied");
        }
    }
}

#[test]
fn a_double_click_edits_the_type_inside_and_saves_it_back() {
    let (mut app, _) = app_with_type();
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    pull_top_in(&mut app);
    super::commit(&mut app);
    let parent = app.session.active().unwrap().doc.id;
    let smart = app.session.active().unwrap().active_layer.unwrap();
    let past = app.session.active().unwrap().history.past_len();
    // Somewhere on the lettering (the box's centre line, a little in from its left).
    let list = app.run("layer.list", json!({"flat": true})).unwrap();
    let b = list["layers"].as_array().unwrap().iter().find(|l| l["id"] == json!(smart.0)).unwrap()["bounds"].clone();
    let (x, y) = (b[0].as_f64().unwrap() + 10.0, b[1].as_f64().unwrap() + b[3].as_f64().unwrap() / 2.0);
    let mut hit = false;
    for dx in 0..60 {
        if crate::type_tool::edit_smart_type_at(&mut app, x + f64::from(dx), y) {
            hit = true;
            break;
        }
    }
    assert!(hit, "the smart object's type opened for editing");
    assert!(app.ui.text_edit.is_some(), "the type edit started in its contents");
    assert_ne!(app.session.active().unwrap().doc.id, parent, "in the contents document");
    crate::type_tool::insert(&mut app, "!");
    crate::type_tool::commit(&mut app);
    crate::type_tool::smart_type_tick(&mut app);
    let st = app.session.active().unwrap();
    assert_eq!(st.doc.id, parent, "back on the page");
    assert_eq!(app.session.documents().len(), 1, "the contents document closed");
    assert_eq!(st.history.past_len(), past + 1, "the new text is one step on the page");
    assert!(matches!(st.doc.layer(smart).map(|l| &l.content), Some(LayerContent::Smart(_))), "still the smart object, with its perspective");
}

/// A drag on a corner of the box Type › Perspective opened moves that corner (the box stays).
#[test]
fn dragging_a_corner_of_the_type_perspective_box_moves_it() {
    let (mut app, _) = app_with_type();
    app.ui.tool = crate::state::Tool::Move;
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    let [x, y] = app.ui.transform.as_ref().unwrap().quad[1];
    let ev = |kind: &str, dx: f64, dy: f64| json!({"kind": kind, "x": x + dx, "y": y + dy});
    let events = json!([ev("down", 0.0, 0.0), ev("move", -10.0, -10.0), ev("move", -20.0, -30.0), ev("up", -20.0, -30.0)]);
    let (req, _rx) = crate::control::ControlRequest::new("ui.pointer", json!({"events": events}));
    let _ = crate::control::handle(&mut app, &ctx, &req);
    let t = app.ui.transform.as_ref().expect("the box is still open");
    assert_eq!(t.made, Some(super::MadeLayer::SmartType), "the same session");
    assert_ne!(t.quad[1], [x, y], "the corner moved: {:?}", t.quad);
}

/// The box stays open across frames (nothing in the frame loop ends it).
#[test]
fn the_type_perspective_box_survives_frames() {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let (mut app, _) = app_with_type();
        app.ui.tool = crate::state::Tool::Move;
        app
    });
    h.run_steps(5);
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    assert!(h.state().ui.transform.is_some());
    for k in 0..6 {
        h.run_steps(1);
        let t = h.state().ui.transform.as_ref();
        assert!(t.is_some_and(|t| t.made == Some(super::MadeLayer::SmartType)), "frame {k}: {:?} status {}", t.map(|t| (t.made, t.layer)), h.state().ui.status);
    }
}
