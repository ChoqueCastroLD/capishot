//! Type › Perspective keeps type a type layer: the box opens in Perspective mode over the text,
//! the preview draws the glyphs through the box (sharp), OK stores the projective map on the type
//! layer (one step), Cancel leaves it as it was. The text stays editable in place: a tap puts the
//! caret where it was tapped and typing keeps the perspective. PSD files carry it as a smart
//! object Photoshop can show, which opens back as perspective type. Smart objects made by the old
//! route (and Edit › Transform › Perspective and Distort on smart objects and pixel layers) work
//! as before.

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

fn text_of(app: &PhotocraftApp, id: LayerId) -> photocraft_doc::TextLayer {
    match app.session.active().unwrap().doc.layer(id).map(|l| &l.content) {
        Some(LayerContent::Text(t)) => t.clone(),
        _ => panic!("not a type layer"),
    }
}

/// Type › Perspective with the top corners pulled in, committed.
fn perspective_type() -> (PhotocraftApp, LayerId) {
    let (mut app, id) = app_with_type();
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    pull_top_in(&mut app);
    super::commit(&mut app);
    (app, id)
}

#[test]
fn type_perspective_stays_a_type_layer_and_commits_one_step() {
    let (mut app, id) = app_with_type();
    let ctx = egui::Context::default();
    let past = app.session.active().unwrap().history.past_len();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    assert_eq!(kind(&app), "text", "still type while the box is open");
    let t = app.ui.transform.as_ref().expect("the box is open");
    assert_eq!(t.mode, TransformMode::Perspective);
    assert!(app.transform_preview.as_ref().is_some_and(|pv| pv.vector_type.is_some()), "the preview draws the text through the box");
    pull_top_in(&mut app);
    super::commit(&mut app);
    assert!(app.ui.transform.is_none());
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1, "one step");
    assert_eq!(kind(&app), "text", "kind=type after the commit");
    let t = text_of(&app, id);
    let q = t.perspective.expect("a perspective on the type layer").quad().expect("its quad");
    assert!(q[1][0] - q[0][0] < q[2][0] - q[3][0] - 10.0, "the top is narrower than the bottom: {q:?}");
    app.run("edit.undo", json!({})).unwrap();
    assert!(text_of(&app, id).perspective.is_none(), "one undo: flat type again");
}

#[test]
fn cancel_leaves_the_type_as_it_was() {
    let (mut app, id) = app_with_type();
    let ctx = egui::Context::default();
    let past = app.session.active().unwrap().history.past_len();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    pull_top_in(&mut app);
    super::cancel(&mut app);
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), past, "nothing left in the history");
    assert!(text_of(&app, id).perspective.is_none(), "no perspective");
}

/// The box shows the text redrawn through it as it moves (type, not resampled pixels).
#[test]
fn the_preview_redraws_the_type_through_the_box() {
    let (mut app, id) = app_with_type();
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    pull_top_in(&mut app);
    super::refresh_vector_type(&mut app);
    let pv = app.transform_preview.as_ref().unwrap();
    let Some(LayerContent::Text(t)) = pv.doc.layer(id).map(|l| &l.content) else { panic!("the preview has the type layer") };
    assert!(t.perspective.is_some() && pv.doc.layer(id).unwrap().visible, "drawn in perspective, in place");
    assert!(pv.vector_type.as_ref().unwrap().drawn >= 1);
}

/// A doc point of character `index`'s caret on perspective type (mid-line height).
fn caret_point(app: &mut PhotocraftApp, id: LayerId, index: usize) -> (f64, f64) {
    let (l, aff, text) = crate::type_tool::layout(app, id).unwrap();
    let byte = text.char_indices().nth(index).map_or(text.len(), |(b, _)| b);
    let (x, top, bot) = l.caret(byte);
    let (tx, ty) = l.to_text(x, (top + bot) / 2.0);
    let p = aff.apply(photocraft_geom::Point::new(f64::from(tx), f64::from(ty)));
    let persp = text_of(app, id).perspective.unwrap();
    persp.apply(p.x, p.y).unwrap()
}

/// A tap on perspective type puts the caret where it was tapped (the hit goes back through the
/// perspective), and typing keeps the perspective.
#[test]
fn a_tap_edits_perspective_type_in_place_and_typing_keeps_the_perspective() {
    let (mut app, id) = perspective_type();
    let before = text_of(&app, id).perspective.unwrap();
    let (x, y) = caret_point(&mut app, id, 5);
    assert_eq!(crate::type_tool::offset_at(&mut app, id, x, y), 5, "the caret goes where the tap is");
    app.ui.tool = crate::state::Tool::Type;
    assert!(crate::type_tool::edit_at(&mut app, x, y), "a tap on the text edits it");
    assert_eq!(app.ui.text_edit.as_ref().map(|e| e.caret), Some(5));
    let width = |app: &PhotocraftApp| text_of(app, id).cache.unwrap().content_bounds().width();
    let w0 = width(&app);
    crate::type_tool::insert(&mut app, " Y MÁS");
    crate::type_tool::commit(&mut app);
    let t = text_of(&app, id);
    assert!(t.text.contains(" Y MÁS"));
    assert_eq!(t.perspective.map(|p| p.h), Some(before.h), "the same perspective");
    assert!(width(&app) > w0, "the longer text reaches further");
    assert_eq!(kind(&app), "text");
}

/// The fraction of a layer's edge pixels that are neither inside nor outside: blur spreads edges
/// over more partly covered pixels.
fn softness(s: &photocraft_raster::Surface) -> f64 {
    let r = s.content_bounds();
    let mut px = vec![[0u8; 4]; (r.width() * r.height()) as usize];
    s.read_rgba8_into(r, &mut px);
    let partial = px.iter().filter(|p| p[3] > 25 && p[3] < 230).count() as f64;
    let solid = px.iter().filter(|p| p[3] >= 230).count() as f64;
    partial / solid.max(1.0)
}

/// Perspective type is as sharp as flat type (its outlines go through the map), and sharper than
/// the old route's smart object, which resampled the rendered text.
#[test]
fn perspective_type_is_drawn_sharp() {
    // The same mild perspective three ways, at twice the size (a 2× zoom of the page).
    let make = || {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 1200, "height": 800, "background": "white"})).unwrap();
        app.sync_views();
        let id = LayerId(app.run("type.create", json!({"text": "PERSPECTIVA", "size": 120, "x": 200, "y": 400})).unwrap()["layer"].as_u64().unwrap());
        (app, id)
    };
    let quad = |app: &PhotocraftApp, id: LayerId| {
        let r = text_of(app, id).cache.unwrap().content_bounds();
        let (x0, y0, x1, y1) = (f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1));
        let w = x1 - x0;
        (json!([x0, y0, x1, y1]), json!([[x0 + w * 0.08, y0], [x1 - w * 0.08, y0], [x1, y1], [x0, y1]]))
    };
    let (flat_app, flat_id) = make();
    let flat = softness(&text_of(&flat_app, flat_id).cache.unwrap());
    let (mut app, id) = make();
    let (rect, q) = quad(&app, id);
    app.run("edit.transform", json!({"layer": id.0, "rect": rect, "quad": q})).unwrap();
    let ours = softness(&text_of(&app, id).cache.unwrap());
    let (mut old, oid) = make();
    let (rect, q) = quad(&old, oid);
    let smart = LayerId(old.run("layer.smartObjects.convertToSmartObject", json!({"layer": oid.0})).unwrap()["layer"].as_u64().unwrap());
    old.run("edit.transform", json!({"layer": smart.0, "rect": rect, "quad": q})).unwrap();
    let st = old.session.active().unwrap();
    let Some(LayerContent::Smart(sm)) = st.doc.layer(smart).map(|l| &l.content) else { panic!("smart") };
    let resampled = softness(sm.cache.as_ref().unwrap());
    assert!(ours <= flat * 1.35, "as sharp as flat type: {ours:.3} vs {flat:.3}");
    assert!(ours < resampled * 0.85, "sharper than the resampled smart object: {ours:.3} vs {resampled:.3}");
}

/// Saved as PSD: a smart object Photoshop can show (placed through the perspective, our block on
/// it); opened again in Capishot: the type layer in perspective, editable.
#[test]
fn perspective_type_round_trips_through_psd() {
    let (app, id) = perspective_type();
    let before = text_of(&app, id);
    let doc = app.session.active().unwrap().doc.clone();
    let out = photocraft_io::export(&doc, "page.psd", &Default::default()).unwrap();
    let file = photocraft_psd::PsdFile::from_bytes(&out.bytes).unwrap();
    let name = doc.layer(id).unwrap().name.clone();
    let rec = file.layer_info.as_ref().unwrap().layers.iter().find(|l| String::from_utf8_lossy(&l.name) == name).expect("the layer record");
    let keys: Vec<[u8; 4]> = rec.blocks.iter().map(|b| b.key).collect();
    assert!(keys.iter().any(|k| k == b"SoLd" || k == b"PlLd"), "a placed smart object for Photoshop: {keys:?}");
    assert!(keys.iter().any(|k| k == b"CpPT"), "marked as type in perspective");
    let back = photocraft_io::import("page.psd", &out.bytes).unwrap().document;
    let l = back.walk().into_iter().map(|(_, _, l)| l.clone()).find(|l| l.name == doc.layer(id).unwrap().name).expect("the layer");
    let LayerContent::Text(t) = &l.content else { panic!("opens as type, not {}", l.content.kind_name()) };
    assert_eq!(t.text, before.text);
    let (qa, qb) = (before.perspective.unwrap().quad().unwrap(), t.perspective.expect("in perspective").quad().unwrap());
    let (ca, cb) =
        (qa.iter().fold([0.0, 0.0], |s, q| [s[0] + q[0] / 4.0, s[1] + q[1] / 4.0]), qb.iter().fold([0.0, 0.0], |s, q| [s[0] + q[0] / 4.0, s[1] + q[1] / 4.0]));
    assert!((ca[0] - cb[0]).abs() < 2.0 && (ca[1] - cb[1]).abs() < 2.0, "the same place: {qa:?} {qb:?}");
}

/// A type layer read from a PSD, restyled and put in perspective: the new style and the perspective
/// come back, not the file's first `TySh`.
#[test]
fn a_type_layer_from_a_psd_keeps_its_new_style_in_perspective() {
    let (app, id) = app_with_type();
    let doc = app.session.active().unwrap().doc.clone();
    let name = doc.layer(id).unwrap().name.clone();
    let first = photocraft_io::export(&doc, "page.psd", &Default::default()).unwrap();
    let opened = photocraft_io::import("page.psd", &first.bytes).unwrap().document;
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.add_document(opened, None);
    app.sync_views();
    let find = |d: &photocraft_doc::Document| d.walk().into_iter().map(|(_, _, l)| l.clone()).find(|l| l.name == name).expect("the layer");
    let id = find(&app.session.active().unwrap().doc).id;
    app.run("layer.select", json!({"layer": id.0})).unwrap();
    assert!(find(&app.session.active().unwrap().doc).psd_blocks.iter().any(|(k, _)| k == b"TySh"), "as read from a file");
    app.run("type.setStyle", json!({"layer": id.0, "color": "#ff0000"})).unwrap();
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, super::TYPE_PERSPECTIVE, json!({})).unwrap();
    pull_top_in(&mut app);
    super::commit(&mut app);
    let styled = text_of(&app, id);
    let saved = photocraft_io::export(&app.session.active().unwrap().doc, "page.psd", &Default::default()).unwrap();
    let back = find(&photocraft_io::import("page.psd", &saved.bytes).unwrap().document);
    let LayerContent::Text(t) = &back.content else { panic!("opens as type, not {}", back.content.kind_name()) };
    assert!(t.perspective.is_some(), "in perspective");
    assert_eq!(t.color, styled.color, "the new colour");
}

/// A host opens Distort on type by its menu id, and changes an open box's mode the same way; it
/// stays type and is drawn as vectors.
#[test]
fn edit_transform_distort_opens_on_type_and_a_menu_id_changes_the_mode() {
    let (mut app, id) = app_with_type();
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "edit.transform.distort", json!({})).unwrap();
    assert_eq!(app.ui.transform.as_ref().unwrap().mode, TransformMode::Distort);
    assert_eq!(kind(&app), "text");
    assert!(app.transform_preview.as_ref().is_some_and(|pv| pv.vector_type.is_some()), "drawn as vectors");
    crate::menus::invoke(&mut app, &ctx, "edit.transform.perspective", json!({})).unwrap();
    assert_eq!(app.ui.transform.as_ref().unwrap().mode, TransformMode::Perspective);
    let t = app.ui.transform.as_mut().unwrap();
    t.quad[0][0] += 20.0;
    t.quad[1][0] -= 20.0;
    super::commit(&mut app);
    assert!(text_of(&app, id).perspective.is_some(), "the commit keeps it type, in perspective");
}

/// A document made by the old route (type converted to a smart object, then put in perspective)
/// still opens as that smart object, through .pcraft and PSD.
#[test]
fn old_perspective_smart_objects_still_open() {
    let (mut app, id) = app_with_type();
    let smart = LayerId(app.run("layer.smartObjects.convertToSmartObject", json!({"layer": id.0})).unwrap()["layer"].as_u64().unwrap());
    app.run("edit.transform", json!({"layer": smart.0, "rect": [80, 130, 520, 230], "quad": [[120, 130], [480, 130], [520, 230], [80, 230]]})).unwrap();
    let doc = app.session.active().unwrap().doc.clone();
    let pcraft = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let back = photocraft_format::load_from_bytes(&pcraft).unwrap();
    assert!(matches!(back.layer(smart).map(|l| &l.content), Some(LayerContent::Smart(s)) if s.perspective.is_some()), "a .pcraft keeps it");
    let psd = photocraft_io::export(&doc, "old.psd", &Default::default()).unwrap();
    let back = photocraft_io::import("old.psd", &psd.bytes).unwrap().document;
    assert!(back.walk().iter().any(|(_, _, l)| matches!(&l.content, LayerContent::Smart(_))), "a PSD keeps it a smart object");
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

/// The old route (documents made before type stayed type): type converted to a smart object,
/// then put in perspective.
fn old_route() -> (PhotocraftApp, LayerId) {
    let (mut app, id) = app_with_type();
    let smart = LayerId(app.run("layer.smartObjects.convertToSmartObject", json!({"layer": id.0})).unwrap()["layer"].as_u64().unwrap());
    let r = match app.session.active().unwrap().doc.layer(smart).map(|l| &l.content) {
        Some(LayerContent::Smart(sm)) => sm.cache.as_ref().unwrap().content_bounds(),
        _ => panic!("smart"),
    };
    let (x0, y0, x1, y1) = (f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1));
    let w = x1 - x0;
    app.run("edit.transform", json!({"layer": smart.0, "rect": [x0, y0, x1, y1], "quad": [[x0 + w * 0.15, y0], [x1 - w * 0.15, y0], [x1, y1], [x0, y1]]}))
        .unwrap();
    (app, smart)
}

/// Old route: a double click edits the type inside the smart object and saves it back.
#[test]
fn a_double_click_edits_the_type_inside_and_saves_it_back() {
    let (mut app, _) = old_route();
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
    let left = app.ui.transform.as_ref().unwrap().quad[0];
    let ev = |kind: &str, dx: f64, dy: f64| json!({"kind": kind, "x": x + dx, "y": y + dy});
    let events = json!([ev("down", 0.0, 0.0), ev("move", -10.0, -10.0), ev("move", -20.0, -30.0), ev("up", -20.0, -30.0)]);
    let (req, _rx) = crate::control::ControlRequest::new("ui.pointer", json!({"events": events}));
    let _ = crate::control::handle(&mut app, &ctx, &req);
    let t = app.ui.transform.as_ref().expect("the box is still open");
    assert!(app.transform_preview.as_ref().is_some_and(|pv| pv.vector_type.is_some()), "the same session");
    assert_ne!(t.quad[1], [x, y], "the corner moved: {:?}", t.quad);
    let q = t.quad;
    let _ = left;
    assert!((q[0][1] - q[1][1]).abs() > 5.0 || (q[0][0] - q[3][0]).abs() > 5.0, "Perspective, not a scale (no longer a rectangle): {q:?}");
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
        assert!(t.is_some() && h.state().transform_preview.as_ref().is_some_and(|pv| pv.vector_type.is_some()), "frame {k}: status {}", h.state().ui.status);
    }
}

/// Old route: longer text typed into the smart object's type isn't clipped: it reaches further.
#[test]
fn longer_text_typed_in_grows_the_perspective_type() {
    let (mut app, _) = old_route();
    let smart = app.session.active().unwrap().active_layer.unwrap();
    let bounds = |app: &mut PhotocraftApp| {
        let list = app.run("layer.list", json!({"flat": true})).unwrap();
        let b = list["layers"].as_array().unwrap().iter().find(|l| l["id"] == json!(smart.0)).unwrap()["bounds"].clone();
        b.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>()
    };
    let b0 = bounds(&mut app);
    let (x, y) = (b0[0] + 10.0, b0[1] + b0[3] / 2.0);
    assert!((0..60).any(|dx| crate::type_tool::edit_smart_type_at(&mut app, x + f64::from(dx), y)));
    // The whole text is selected when the edit starts: this replaces it.
    crate::type_tool::insert(&mut app, "PERSPECTIVA Y MÁS TEXTO");
    crate::type_tool::commit(&mut app);
    crate::type_tool::smart_type_tick(&mut app);
    let b1 = bounds(&mut app);
    assert!(b1[2] > b0[2] * 1.5, "wider, not clipped: {b0:?} → {b1:?}");
}
