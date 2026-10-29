use std::sync::Arc;

use egui_kittest::kittest::Queryable;
use photocraft_ai::catalog::{LAMA_ID, model};
use photocraft_ai::models::{FakeStore, ModelStore, Status};
use photocraft_ai_lama::service::FakeInpainter;
use photocraft_geom::Rect;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

/// A page with a red blob, a fake model store (`lama`: Manga LaMa on this device) and inpainter.
fn app(lama: bool) -> (PhotocraftApp, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::default());
    if lama {
        store.put(model(LAMA_ID).unwrap());
    }
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    s.edit("blob", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(28, 18, 36, 28), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.set_ai_models(Some(store.clone()));
    s.set_inpainter(Some(Arc::new(FakeInpainter::default())));
    let mut app = PhotocraftApp::new(s, crate::Services::default());
    app.ui.tool = Tool::RemoveAi;
    app.session.tools.brush.size = 14.0;
    (app, store)
}

fn red(app: &PhotocraftApp) -> bool {
    let st = app.session.active().unwrap();
    let v = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(31, 22);
    v[0] > 0.9 && v[1] < 0.1
}

#[test]
fn a_stroke_removes_with_manga_lama_in_one_step() {
    let (mut app, _) = app(true);
    let past = app.session.active().unwrap().history.past_len();
    crate::remove_ai_ui::finish_stroke(&mut app, &[[29.0, 22.0, 1.0], [35.0, 22.0, 1.0]]);
    assert!(!red(&app), "removed");
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1);
    assert_eq!(st.history.entries().last().map(String::as_str), Some("Remove with AI"));
    assert!(!crate::remove_ai_ui::is_open(&app));
}

#[test]
fn a_tap_with_a_selection_fills_the_selection() {
    let (mut app, _) = app(true);
    app.run("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
    crate::remove_ai_ui::finish_stroke(&mut app, &[[5.0, 5.0, 1.0]]);
    assert!(!red(&app), "the selection is filled, not the tap's spot");
}

/// The first use: the modal opens and the download starts by itself; when it is done the stroke
/// is applied without another click. Cancel drops it.
#[test]
fn the_first_use_downloads_then_applies_and_cancel_drops_it() {
    let (app0, store) = app(false);
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            crate::remove_ai_ui::show(app, ui.ctx());
        },
        app0,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    crate::remove_ai_ui::finish_stroke(h.state_mut(), &[[29.0, 22.0, 1.0], [35.0, 22.0, 1.0]]);
    h.run_steps(2);
    assert!(crate::remove_ai_ui::is_open(h.state()), "the modal is open");
    h.get_by_label_contains("Manga LaMa model");
    assert!(matches!(store.status(model(LAMA_ID).unwrap()), Status::Downloading { .. } | Status::Queued), "downloading by itself");
    assert!(red(h.state()), "nothing applied yet");
    while store.step() {}
    h.run_steps(2);
    let app = h.into_state();
    assert!(!crate::remove_ai_ui::is_open(&app), "closed once downloaded");
    assert!(!red(&app), "the waiting stroke was applied");

    // Cancel: nothing applied, the download stops.
    let (app0, store) = self::app(false);
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(390.0, 780.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            crate::remove_ai_ui::show(app, ui.ctx());
        },
        app0,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    crate::remove_ai_ui::finish_stroke(h.state_mut(), &[[29.0, 22.0, 1.0], [35.0, 22.0, 1.0]]);
    h.run_steps(2);
    let cancel = h.get_by_label("Cancel");
    let r = cancel.rect();
    assert!(r.max.x <= 390.0 && r.max.y <= 780.0, "fits a phone: {r:?}");
    cancel.click();
    h.run_steps(2);
    assert!(!crate::remove_ai_ui::is_open(h.state()));
    assert!(!matches!(store.status(model(LAMA_ID).unwrap()), Status::Downloading { .. } | Status::Queued));
    while store.step() {}
    h.run_steps(2);
    assert!(red(h.state()), "nothing applied after Cancel");
}

/// The tool sits after Remove in the J group and J / ⇧J cycle to it; the Remove tool keeps its
/// options exactly.
#[test]
fn the_tool_group_and_the_remove_tool_untouched() {
    let j: Vec<Tool> = Tool::ALL.iter().copied().filter(|t| t.key() == 'J').collect();
    assert_eq!(&j[..2], &[Tool::Remove, Tool::RemoveAi]);
    assert_eq!(crate::tool_reset::option_keys(Tool::Remove), &["remove_new_layer", "remove_after_each_stroke", "remove_ai"]);
    let d = crate::state::ToolOptions::default();
    assert_eq!((d.remove_new_layer, d.remove_after_each_stroke, d.remove_ai, d.sample_all.remove), (false, true, false, false));
    assert_eq!(crate::tool_reset::sample_all_key(Tool::RemoveAi).as_deref(), Some("remove_ai"));
}
