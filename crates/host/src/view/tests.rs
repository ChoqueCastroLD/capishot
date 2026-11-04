//! The widget in a host's context, without a window or GPU (the CPU canvas path).

use egui::{Context, Pos2, RawInput, Rect, vec2};
use photocraft_ui_egui::Tool;
use serde_json::json;

use super::*;
use crate::{Event, OpenOptions};

fn input() -> RawInput {
    RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), focused: true, ..Default::default() }
}

fn frame(ctx: &Context, editor: &mut CapishotEditor) -> ViewOutput {
    let mut out = None;
    let mut o = ctx.run_ui(input(), |ui| {
        ui.label("host list");
        out = Some(editor.ui_in_rect(ui, Rect::from_min_size(Pos2::new(200.0, 0.0), vec2(600.0, 600.0))));
    });
    o.textures_delta.clear();
    out.expect("the frame ran")
}

/// A `w` × `h` PSD's bytes (what a host gets from its server).
fn psd(w: u32, h: u32) -> Vec<u8> {
    let mut maker = Host::new();
    maker.execute("file.new", json!({"width": w, "height": h})).expect("a document");
    let doc = maker.active_document().expect("active");
    maker.save_psd(doc).expect("PSD bytes")
}

fn editor_with_doc() -> (CapishotEditor, DocumentId) {
    let mut editor = CapishotEditor::new(None);
    editor.host.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
    let doc = editor.host.active_document().expect("active");
    (editor, doc)
}

#[test]
fn the_canvas_draws_in_the_host_context_and_leaves_its_look_alone() {
    let ctx = Context::default();
    ctx.set_visuals(egui::Visuals::light());
    // The host's own first frame (fonts exist from then on), then its look as it set it.
    ctx.run_ui(input(), |ui| {
        ui.label("host list");
    })
    .textures_delta
    .clear();
    let style = ctx.global_style();
    let fonts = ctx.fonts(|f| f.definitions().clone());
    let (mut editor, doc) = editor_with_doc();
    let out = frame(&ctx, &mut editor);
    assert_eq!(out.document, Some(doc), "the document the host made is drawn, with no sync call");
    assert!(out.transform.is_some() && out.overlay().is_some());
    let _ = frame(&ctx, &mut editor);
    assert_eq!(*ctx.global_style(), *style, "no global style change");
    assert_eq!(ctx.fonts(|f| f.definitions().clone()), fonts, "no font change");
    // The session is back with the host after the frame.
    assert_eq!(editor.host.documents().len(), 1);
    editor.host.execute("layer.new.layer", json!({"name": "Ink"})).expect("the host still runs commands");
}

#[test]
fn a_document_opened_between_frames_gets_its_view() {
    let ctx = Context::default();
    let mut editor = CapishotEditor::new(None);
    let out = frame(&ctx, &mut editor);
    assert_eq!(out.document, None);
    let doc = editor.host.open(psd(40, 30), OpenOptions::named("page.psd")).expect("opened").document;
    let out = frame(&ctx, &mut editor);
    assert_eq!(out.document, Some(doc));
    let t = out.transform.expect("a mapping");
    assert_eq!(t.document_size(), [40, 30]);
    let p = t.to_screen([20.0, 15.0]);
    let back = t.to_doc(p);
    assert!((back[0] - 20.0).abs() < 1e-3 && (back[1] - 15.0).abs() < 1e-3);
    assert!(t.canvas_rect().contains(p), "the image centre is on the canvas");
}

#[test]
fn tools_travel_both_ways() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    editor.host.set_tool("eraser").expect("a tool id");
    let _ = frame(&ctx, &mut editor);
    assert_eq!(editor.view.app().ui.tool, Tool::Eraser, "the host's tool reaches the editor");
    // The person picks the Brush on the canvas (a shortcut, the canvas menu).
    editor.view.app_mut().ui.tool = Tool::Brush;
    let out = frame(&ctx, &mut editor);
    assert_eq!(editor.host.tool(), "brush", "the editor's tool reaches the host");
    let out2 = frame(&ctx, &mut editor);
    let changed = |o: &ViewOutput| o.events.iter().any(|e| matches!(e, Event::ToolChanged { tool } if tool == "brush"));
    assert!(changed(&out) || changed(&out2), "and is reported as toolChanged");
}

#[test]
fn an_unknown_tool_is_reported_not_a_panic() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    editor.host.set_tool("noSuchTool").expect("a well-formed id");
    let out = frame(&ctx, &mut editor);
    assert!(out.view_events.iter().any(|e| matches!(e, ViewEvent::Failed { .. })), "{:?}", out.view_events);
}

#[test]
fn the_host_moves_the_camera_and_hears_about_it() {
    let ctx = Context::default();
    let (mut editor, doc) = editor_with_doc();
    let _ = frame(&ctx, &mut editor);
    editor.view.set_camera(doc, CameraChange::Set { zoom: Some(3.0), center: Some([5.0, 6.0]), rotation: None });
    let out = frame(&ctx, &mut editor);
    let c = editor.view.camera(doc).expect("a camera");
    assert_eq!((c.zoom, c.center), (3.0, [5.0, 6.0]));
    assert!(out.view_events.iter().any(|e| matches!(e, ViewEvent::CameraChanged { document, .. } if *document == doc)));
    // A bad camera is an event, not a panic, and changes nothing.
    editor.view.set_camera(doc, CameraChange::Set { zoom: Some(f32::INFINITY), center: None, rotation: None });
    let out = frame(&ctx, &mut editor);
    assert!(out.view_events.iter().any(|e| matches!(e, ViewEvent::Failed { .. })));
    assert_eq!(editor.view.camera(doc).map(|c| c.zoom), Some(3.0));
}

#[test]
fn a_hidden_document_is_shown_in_one_switch() {
    let ctx = Context::default();
    let (mut editor, first) = editor_with_doc();
    let second = editor.host.open(psd(32, 32), OpenOptions { name: Some("next.psd".into()), hidden: true }).expect("opened").document;
    let out = frame(&ctx, &mut editor);
    assert_eq!(out.document, Some(first), "a hidden document doesn't take the canvas");
    editor.view.prewarm(second, false, DocumentCamera::Own);
    editor.view.show_document(second, true, DocumentCamera::Keep);
    let mut shown = None;
    let mut prewarmed = false;
    // The switch waits for the prewarm's background work: frames until it replies (bounded).
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(20) {
        let out = frame(&ctx, &mut editor);
        for e in out.view_events {
            match e {
                ViewEvent::Shown { document, result } => shown = Some((document, result)),
                ViewEvent::Prewarmed { document, .. } if document == second => prewarmed = true,
                _ => {}
            }
        }
        if shown.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (document, result) = shown.expect("the switch replied");
    assert_eq!(document, second);
    assert_eq!(result.expect("shown")["shown"], json!(true));
    assert!(prewarmed, "the prewarm replied too");
    assert_eq!(editor.host.active_document(), Some(second));
    let out = frame(&ctx, &mut editor);
    assert_eq!(out.document, Some(second));
}

#[test]
fn host_prewarm_reaches_the_view() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    let hidden = editor.host.open(psd(16, 16), OpenOptions { name: Some("h.psd".into()), hidden: true }).expect("opened").document;
    assert!(editor.host.prewarm(hidden, &json!({})).expect("queued"), "the view takes the prewarm");
    assert!(editor.host.prewarm(hidden, &json!(3)).is_err(), "bad options are an error");
    let mut done = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(20) {
        let out = frame(&ctx, &mut editor);
        done |= out.view_events.iter().any(|e| matches!(e, ViewEvent::Prewarmed { document, result: Ok(_) } if *document == hidden));
        if done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(done);
}

#[test]
fn control_replies_carry_their_token() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    let ok = editor.view.control("ui.set", json!({"tool": "brush"}));
    let bad = editor.view.control("ui.noSuchMethod", json!({}));
    let out = frame(&ctx, &mut editor);
    let reply = |t: u64| out.view_events.iter().find_map(|e| match e {
        ViewEvent::Reply { token, result } if *token == t => Some(result.clone()),
        _ => None,
    });
    assert!(reply(ok).expect("a reply").is_ok());
    assert!(reply(bad).expect("a reply").is_err());
}

#[test]
fn host_theme_colours_reach_the_tokens() {
    let v = egui::Visuals::dark();
    let mut theme = HostTheme::from_visuals(&v);
    theme.accent = egui::Color32::from_rgb(9, 8, 7);
    theme.pasteboard = egui::Color32::from_rgb(1, 1, 1);
    let t = theme.tokens(&v);
    assert_eq!(t.accent, egui::Color32::from_rgb(9, 8, 7));
    assert_eq!(t.canvas, egui::Color32::from_rgb(1, 1, 1));
    assert!(t.dark);
}
