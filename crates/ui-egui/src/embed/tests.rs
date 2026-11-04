//! The embedded canvas: draws in the host's context without restyling it, creates views for
//! documents the host opened, routes keys as the host says.

use egui::{Context, Event, Key, Modifiers, Pos2, RawInput, Rect, vec2};
use photocraft_engine::Session;
use serde_json::json;

use super::*;

fn session_with_doc() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
    s
}

fn area() -> Rect {
    Rect::from_min_size(Pos2::new(10.0, 20.0), vec2(320.0, 240.0))
}

fn input(events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
        focused: true,
        events,
        ..Default::default()
    }
}

/// One host frame: the host's own label, then the embedded canvas.
fn run(ctx: &Context, app: &mut PhotocraftApp, events: Vec<Event>, opts: &EmbedOptions) -> (EmbedFrame, Vec<Event>) {
    let mut out = None;
    let mut left = Vec::new();
    let mut o = ctx.run_ui(input(events), |ui| {
        ui.label("host");
        out = Some(frame(app, ui, area(), opts));
        left = ui.input(|i| i.events.clone());
    });
    o.textures_delta.clear();
    (out.expect("the frame ran"), left)
}

#[test]
fn the_host_style_and_fonts_stay_as_they_were() {
    let ctx = Context::default();
    ctx.set_visuals(egui::Visuals::light());
    let style_before = ctx.global_style();
    let fonts_before = ctx.fonts(|f| f.definitions().clone());
    let mut app = PhotocraftApp::new_embedded(Services::default());
    app.session = session_with_doc();
    for _ in 0..3 {
        let _ = run(&ctx, &mut app, Vec::new(), &EmbedOptions::default());
    }
    assert_eq!(*ctx.global_style(), *style_before, "the host's style is untouched");
    assert_eq!(ctx.fonts(|f| f.definitions().clone()), fonts_before, "the host's fonts are untouched");
}

#[test]
fn a_document_the_host_opened_gets_a_view_and_is_drawn() {
    let ctx = Context::default();
    let mut app = PhotocraftApp::new_embedded(Services::default());
    let (f, _) = run(&ctx, &mut app, Vec::new(), &EmbedOptions::default());
    assert!(!f.drawn, "no document yet");
    assert!(f.xform.is_none());
    app.session = session_with_doc();
    let (f, _) = run(&ctx, &mut app, Vec::new(), &EmbedOptions::default());
    assert!(f.drawn, "the document the host opened is drawn without any sync call");
    assert_eq!(app.ui.views.len(), 1);
    let xf = f.xform.expect("a mapping for host overlays");
    assert!(area().contains(xf.rect.center()), "the canvas is inside the host's rect");
    let p = xf.to_screen(32.0, 24.0);
    let back = xf.to_doc(p);
    assert!((back[0] - 32.0).abs() < 1e-3 && (back[1] - 24.0).abs() < 1e-3, "canvas ↔ screen round-trips");
}

#[test]
fn keys_stay_with_the_host_when_routing_is_never() {
    let ctx = Context::default();
    let mut app = PhotocraftApp::new_embedded(Services::default());
    app.session = session_with_doc();
    let opts = EmbedOptions { keys: Keys { routing: KeyRouting::Never, filter: None }, ..Default::default() };
    // Twice: the IME pass markers are installed on the first frame and work from the second.
    let _ = run(&ctx, &mut app, Vec::new(), &opts);
    let press = Event::Key { key: Key::B, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    let (f, left) = run(&ctx, &mut app, vec![press.clone()], &opts);
    assert!(!f.took_keys);
    assert!(left.contains(&press), "the host still sees its key");
    assert_ne!(app.ui.tool, Tool::Brush, "B didn't pick the Brush");
}

#[test]
fn the_default_filter_keeps_the_app_shells_shortcuts_out() {
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    assert!(!default_filter(&key(Key::O, Modifiers::COMMAND)), "Open is the host's");
    assert!(!default_filter(&key(Key::S, Modifiers::COMMAND | Modifiers::SHIFT)), "Save As is the host's");
    assert!(!default_filter(&key(Key::Q, Modifiers::COMMAND)), "Quit is the host's");
    assert!(default_filter(&key(Key::Z, Modifiers::COMMAND)), "undo goes to the editor");
    assert!(default_filter(&key(Key::B, Modifiers::NONE)), "tool keys go to the editor");
    assert!(default_filter(&Event::Text("é".into())), "typing goes to the editor");
}

#[test]
fn tool_ids_round_trip() {
    for t in [Tool::Brush, Tool::Eraser, Tool::CloneStamp, Tool::Move] {
        assert_eq!(parse_tool(&tool_id(t)), Some(t), "{t:?}");
    }
    assert_eq!(tool_id(Tool::CloneStamp), "cloneStamp");
    assert_eq!(parse_tool("noSuchTool"), None);
}

#[test]
fn published_tokens_reach_the_canvas_without_restyling() {
    let ctx = Context::default();
    let before = ctx.global_style();
    let mut t = tokens_for_visuals(&egui::Visuals::light());
    t.accent = egui::Color32::from_rgb(1, 2, 3);
    theme::publish_tokens(&ctx, t);
    assert_eq!(theme::Tokens::get(&ctx).accent, egui::Color32::from_rgb(1, 2, 3));
    assert_eq!(*ctx.global_style(), *before);
}

#[test]
fn control_methods_run_against_the_lent_session() {
    let ctx = Context::default();
    let mut app = PhotocraftApp::new_embedded(Services::default());
    app.session = session_with_doc();
    let _ = run(&ctx, &mut app, Vec::new(), &EmbedOptions::default());
    let rx = control(&mut app, &ctx, "ui.set", json!({"tool": "eraser"}));
    let reply = rx.try_recv().expect("ui.set replies at once");
    assert_eq!(reply["ok"], json!(true), "{reply}");
    assert_eq!(tool(&app), Tool::Eraser);
    // Bad params are an error reply, not a panic.
    let rx = control(&mut app, &ctx, "ui.set", json!({"tool": 3}));
    assert_eq!(rx.try_recv().expect("a reply")["ok"], json!(false));
}
