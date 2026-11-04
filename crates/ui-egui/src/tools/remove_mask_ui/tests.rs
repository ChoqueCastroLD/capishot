use std::sync::Arc;

use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use photocraft_ai::catalog::{LAMA_ID, model};
use photocraft_ai::models::FakeStore;
use photocraft_ai_lama::service::FakeInpainter;
use photocraft_geom::Rect;
use serde_json::json;

use super::*;
use crate::canvas::{ToolEvent, tool_event};

/// A page with two red blobs, Manga LaMa on this device (a fake store and inpainter), `tool`
/// painting a mask.
fn app(tool: Tool) -> PhotocraftApp {
    let store = Arc::new(FakeStore::default());
    store.put(model(LAMA_ID).unwrap());
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 96, "height": 48, "background": "white"})).unwrap();
    s.edit("blobs", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        surf.fill_rect(Rect::new(18, 18, 26, 28), &[1.0, 0.0, 0.0, 1.0]);
        surf.fill_rect(Rect::new(66, 18, 74, 28), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.set_ai_models(Some(store));
    s.set_inpainter(Some(Arc::new(FakeInpainter::default())));
    let mut app = PhotocraftApp::new(s, crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app.ui.tool = tool;
    app.session.tools.brush.size = 14.0;
    set_mask_mode(&mut app, tool, true);
    app
}

fn red(app: &PhotocraftApp, x: i32) -> bool {
    let st = app.session.active().unwrap();
    let v = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, 22);
    v[0] > 0.9 && v[1] < 0.1
}

fn past(app: &PhotocraftApp) -> usize {
    app.session.active().unwrap().history.past_len()
}

fn stroke(app: &mut PhotocraftApp, x: f64, mods: Modifiers) {
    tool_event(app, ToolEvent::Down { x, y: 22.0, pressure: 1.0 }, mods);
    tool_event(app, ToolEvent::Move { x: x + 6.0, y: 22.0, pressure: 1.0 }, mods);
    tool_event(app, ToolEvent::Up { x: x + 6.0, y: 22.0 }, mods);
}

/// Off by default (each stroke runs, as before); the Remove tool's switch is Photoshop's Remove
/// after each stroke off.
#[test]
fn off_by_default_and_each_tool_has_its_switch() {
    let d = ToolOptions::default();
    assert!(!mask_mode(&d, Tool::Remove) && !mask_mode(&d, Tool::RemoveAi) && !mask_mode(&d, Tool::Brush));
    let mut app = app(Tool::RemoveAi);
    set_mask_mode(&mut app, Tool::RemoveAi, false);
    let before = past(&app);
    stroke(&mut app, 19.0, Modifiers::NONE);
    assert_eq!(past(&app), before + 1, "off: the stroke runs on release");
    set_mask_mode(&mut app, Tool::Remove, true);
    assert!(!app.ui.tool_options.remove_after_each_stroke);
}

/// Remove with AI: three strokes (one with ⌥ erasing part of another) wait as a mask, Run fills
/// the whole mask once through `edit.removeAi` (Manga LaMa's fixed settings), one undo step that
/// Undo takes back in full.
#[test]
fn remove_ai_strokes_collect_and_run_once_as_one_undo_step() {
    let mut app = app(Tool::RemoveAi);
    let before = past(&app);
    let journal = app.session.journal.len();
    stroke(&mut app, 19.0, Modifiers::NONE);
    stroke(&mut app, 67.0, Modifiers::NONE);
    stroke(&mut app, 44.0, Modifiers::NONE);
    // ⌥ held: this stroke erases the middle one from the mask.
    stroke(&mut app, 44.0, Modifiers::ALT);
    assert_eq!(app.session.journal.len(), journal, "nothing ran yet");
    assert_eq!(past(&app), before, "mask strokes are not history steps");
    assert!(red(&app, 21) && red(&app, 70));
    assert_eq!(app.remove_mask.remove_ai.strokes.len(), 4);
    let v = inspect(&app);
    assert_eq!((v["removeAi"]["strokes"].as_u64(), v["removeAi"]["erased"].as_u64()), (Some(4), Some(1)), "{v}");
    assert!(live(&mut app));
    assert!(run(&mut app));
    assert!(!app.ui.status_error, "{}", app.ui.status);
    assert!(!red(&app, 21) && !red(&app, 70), "both blobs removed");
    assert_eq!(past(&app), before + 1, "one undo step");
    let (id, p) = app.session.journal.last().cloned().unwrap();
    assert_eq!(id, photocraft_engine::remove_ai_cmds::ID);
    let erase: Vec<bool> = p["strokes"].as_array().unwrap().iter().map(|e| e["erase"].as_bool() == Some(true)).collect();
    assert_eq!(erase, [false, false, false, true]);
    assert!(!live(&mut app) && app.remove_mask.remove_ai.strokes.is_empty(), "the mask is gone after Run");
    app.run("edit.undo", json!({})).unwrap();
    assert!(red(&app, 21) && red(&app, 70), "one Undo brings both back");
}

/// The Remove tool (content-aware): the eraser toggle takes a blob back out of the mask, so
/// Run leaves it; Clear mask drops a mask without running.
#[test]
fn remove_eraser_toggle_keeps_what_it_erased_and_clear_drops_the_mask() {
    let mut app = app(Tool::Remove);
    let before = past(&app);
    stroke(&mut app, 19.0, Modifiers::NONE);
    stroke(&mut app, 67.0, Modifiers::NONE);
    app.remove_mask.erase = true;
    assert!(erasing(&app, Modifiers::NONE) && !erasing(&app, Modifiers::ALT), "⌥ flips the toggle");
    stroke(&mut app, 67.0, Modifiers::NONE);
    stroke(&mut app, 63.0, Modifiers::NONE);
    stroke(&mut app, 71.0, Modifiers::NONE);
    let t = app.remove_mask.remove.trail.as_ref().unwrap();
    let k = t.scale() as usize;
    assert_eq!(t.coverage(70 / k, 22 / k), 0, "the erased blob's tint is gone");
    assert!(t.coverage(22 / k, 22 / k) > 0, "the other's stays");
    assert!(run(&mut app));
    assert!(!app.ui.status_error, "{}", app.ui.status);
    assert!(!red(&app, 21), "painted: removed");
    assert!(red(&app, 70), "erased from the mask: kept");
    assert_eq!(past(&app), before + 1);
    assert!(!app.remove_mask.erase, "Run goes back to painting");
    stroke(&mut app, 67.0, Modifiers::NONE);
    assert!(clear(&mut app) && !live(&mut app));
    assert_eq!(past(&app), before + 1, "Clear mask runs nothing");
    assert!(red(&app, 70));
    // Erasing from no mask does nothing; a mask erased to nothing reports it rather than running.
    add(&mut app, Tool::Remove, &[[60.0, 22.0, 1.0]], true);
    assert!(app.remove_mask.remove.strokes.is_empty());
    add(&mut app, Tool::Remove, &[[60.0, 22.0, 1.0]], false);
    add(&mut app, Tool::Remove, &[[60.0, 22.0, 1.0]], true);
    assert!(run(&mut app) && app.ui.status_error);
    assert_eq!(past(&app), before + 1);
}

/// Each tool keeps its own mask, kept while the other is in use; switching the mode off drops it.
#[test]
fn each_tool_keeps_its_own_mask() {
    let mut app = app(Tool::RemoveAi);
    set_mask_mode(&mut app, Tool::Remove, true);
    stroke(&mut app, 19.0, Modifiers::NONE);
    app.ui.tool = Tool::Remove;
    assert!(!live(&mut app), "the Remove tool has no mask");
    stroke(&mut app, 67.0, Modifiers::NONE);
    assert_eq!((app.remove_mask.remove.strokes.len(), app.remove_mask.remove_ai.strokes.len()), (1, 1));
    app.ui.tool = Tool::RemoveAi;
    assert!(live(&mut app));
    set_mask_mode(&mut app, Tool::RemoveAi, false);
    assert!(!live(&mut app) && app.remove_mask.remove_ai.strokes.is_empty());
    assert_eq!(app.remove_mask.remove.strokes.len(), 1);
}

/// The switch is remembered between launches (a tool option, Preferences › tool options).
#[test]
fn the_switch_is_remembered() {
    let mut app = app(Tool::RemoveAi);
    crate::tool_memory::persist(&mut app, &egui::Context::default());
    let saved = app.session.prefs().tool_options.clone();
    assert_eq!(saved["remove_ai_paint_mask"], json!(true));
    let mut restored = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    restored.session.prefs.edit(|p| p.tool_options = saved.clone());
    crate::tool_memory::restore(&mut restored);
    assert!(mask_mode(&restored.ui.tool_options, Tool::RemoveAi));
    // Reset Tool turns it off again.
    assert!(crate::tool_reset::option_keys(Tool::RemoveAi).contains(&"remove_ai_paint_mask"));
}

/// The control channel: read the masks and their strokes, set the eraser, run and clear.
#[test]
fn control_reads_runs_and_clears() {
    let mut app = app(Tool::RemoveAi);
    stroke(&mut app, 19.0, Modifiers::NONE);
    let v = control(&mut app, &json!({"action": "strokes"})).unwrap();
    assert_eq!(v["current"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(control(&mut app, &json!({"erase": true})).unwrap()["erase"], true);
    assert!(control(&mut app, &json!({"erase": 3})).is_err());
    assert!(control(&mut app, &json!({"action": "nope"})).is_err());
    assert!(control(&mut app, &json!({"action": 5})).is_err());
    assert!(control(&mut app, &json!({"on": "x"})).is_err());
    assert_eq!(control(&mut app, &json!({"on": false})).unwrap()["removeAi"]["on"], false, "off drops the mask");
    assert_eq!(control(&mut app, &json!({"on": true})).unwrap()["removeAi"]["on"], true);
    app.ui.tool = Tool::Brush;
    assert!(control(&mut app, &json!({"on": true})).is_err());
    app.ui.tool = Tool::RemoveAi;
    assert_eq!(control(&mut app, &json!({"erase": false})).unwrap()["erase"], false);
    stroke(&mut app, 19.0, Modifiers::NONE);
    assert_eq!(control(&mut app, &json!({"action": "clear"})).unwrap()["done"], true);
    assert_eq!(control(&mut app, &json!({"action": "run"})).unwrap()["done"], false, "nothing to run");
    stroke(&mut app, 19.0, Modifiers::NONE);
    assert_eq!(control(&mut app, &json!({"action": "run"})).unwrap()["done"], true);
    assert!(!red(&app, 21));
}

type H = Harness<'static, PhotocraftApp>;

fn frame(h: &mut H, events: Vec<Event>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    raw.events.extend(events);
    raw.time = Some(ctx.input(|i| i.time) + 1.0 / 30.0);
    crate::touch_ui::route(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn key(k: Key) -> Vec<Event> {
    let e = |pressed| Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Modifiers::NONE };
    vec![e(true), e(false)]
}

/// ↩ runs the waiting mask and Esc clears it, only while one waits and never while text has the
/// keyboard; J still cycles the tools and the mask comes back with its tool.
#[test]
fn enter_and_escape_only_with_a_mask_waiting() {
    let mut app = app(Tool::RemoveAi);
    app.sync_views();
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 860.0)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    for _ in 0..4 {
        frame(&mut h, vec![]);
    }
    let before = past(h.state());
    stroke(h.state_mut(), 19.0, Modifiers::NONE);
    // A text field has the keyboard: Enter is not the mask's.
    h.state_mut().external_text_focus = true;
    frame(&mut h, key(Key::Enter));
    assert!(red(h.state(), 21) && live(h.state_mut()));
    h.state_mut().external_text_focus = false;
    // J steps to the next tool of the group (the mask doesn't take the letter): the mask waits,
    // hidden, and Enter doesn't run it meanwhile; ⇧J comes back to it.
    frame(&mut h, key(Key::J));
    assert_eq!(h.state().ui.tool, Tool::BubbleTextRemover);
    frame(&mut h, key(Key::Enter));
    assert!(red(h.state(), 21), "another tool: Enter leaves the mask");
    h.state_mut().ui.tool = Tool::RemoveAi;
    assert!(live(h.state_mut()), "back with its tool");
    frame(&mut h, key(Key::Escape));
    assert!(!live(h.state_mut()), "Esc cleared it");
    assert_eq!(past(h.state()), before);
    stroke(h.state_mut(), 19.0, Modifiers::NONE);
    frame(&mut h, key(Key::Enter));
    for _ in 0..4 {
        frame(&mut h, vec![]);
    }
    assert!(!red(h.state(), 21), "Enter ran it");
    assert_eq!(past(h.state()), before + 1);
}
