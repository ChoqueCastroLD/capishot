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
fn the_host_sets_the_move_auto_select() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    let _ = frame(&ctx, &mut editor);
    let options = |e: &CapishotEditor| (e.view.app().ui.tool_options.move_target.clone(), e.view.app().ui.tool_options.move_auto_select);
    assert_eq!(options(&editor), ("group".to_string(), true), "left alone by default");
    editor.view.options_mut().move_target = Some(MoveTarget::Layer);
    editor.view.options_mut().move_auto_select = Some(false);
    let _ = frame(&ctx, &mut editor);
    assert_eq!(options(&editor), ("layer".to_string(), false));
    // Applied when it changes: the person's change in the options bar then stands.
    editor.view.app_mut().ui.tool_options.move_target = "group".to_string();
    let _ = frame(&ctx, &mut editor);
    assert_eq!(options(&editor).0, "group");
    editor.view.options_mut().move_target = Some(MoveTarget::Group);
    let _ = frame(&ctx, &mut editor);
    editor.view.options_mut().move_target = Some(MoveTarget::Layer);
    let _ = frame(&ctx, &mut editor);
    assert_eq!(options(&editor).0, "layer");
    // None leaves it as it is, and the control channel takes the same fields.
    editor.view.options_mut().move_target = None;
    editor.view.options_mut().move_auto_select = None;
    let _ = frame(&ctx, &mut editor);
    assert_eq!(options(&editor), ("layer".to_string(), false));
    editor.view.control("ui.set", json!({"moveTarget": "group", "moveAutoSelect": true}));
    let _ = frame(&ctx, &mut editor);
    assert_eq!(options(&editor), ("group".to_string(), true));
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

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
}

fn frame_with(ctx: &Context, editor: &mut CapishotEditor, events: Vec<egui::Event>) -> (ViewOutput, Vec<egui::Event>) {
    let mut out = None;
    let mut left = Vec::new();
    let raw = RawInput { events, ..input() };
    let mut o = ctx.run_ui(raw, |ui| {
        out = Some(editor.ui_in_rect(ui, Rect::from_min_size(Pos2::new(200.0, 0.0), vec2(600.0, 600.0))));
        left = ui.ctx().input(|i| i.events.clone());
    });
    o.textures_delta.clear();
    (out.expect("the frame ran"), left)
}

#[cfg(feature = "full")]
#[test]
fn full_mode_draws_the_whole_editor_and_leaves_the_hosts_look_alone() {
    let ctx = Context::default();
    ctx.set_visuals(egui::Visuals::light());
    ctx.run_ui(input(), |ui| {
        ui.label("host list");
    })
    .textures_delta
    .clear();
    let style = ctx.global_style();
    let (light, dark) = (ctx.style_of(egui::Theme::Light), ctx.style_of(egui::Theme::Dark));
    let preference = ctx.options(|o| o.theme_preference);
    let fonts = ctx.fonts(|f| f.definitions().clone());
    let (mut editor, doc) = editor_with_doc();
    let embedded = frame(&ctx, &mut editor);
    assert_eq!(embedded.document, Some(doc));
    editor.set_mode(Mode::Full);
    assert_eq!(editor.mode(), Mode::Full);
    // What a host widget sees right after the editor drew in this same pass.
    let mut after = None;
    for _ in 0..3 {
        let mut o = ctx.run_ui(input(), |ui| {
            let out = editor.ui_in_rect(ui, Rect::from_min_size(Pos2::new(200.0, 0.0), vec2(600.0, 600.0)));
            assert_eq!(out.document, Some(doc), "the same document, not reopened");
            assert!(out.transform.is_some(), "the canvas is drawn inside the full interface");
            after = Some((ui.style().clone(), ui.ctx().global_style()));
        });
        o.textures_delta.clear();
    }
    let (ui_style, global) = after.expect("the frame ran");
    assert_eq!(*ui_style, *style, "the host's Ui keeps its style");
    assert_eq!(*global, *style, "the context's style is back right after the editor");
    assert_eq!(*ctx.global_style(), *style, "no global style change");
    assert_eq!(*ctx.style_of(egui::Theme::Light), *light);
    assert_eq!(*ctx.style_of(egui::Theme::Dark), *dark);
    assert_eq!(ctx.options(|o| o.theme_preference), preference, "the theme preference is the host's");
    assert_eq!(ctx.fonts(|f| f.definitions().clone()), fonts, "no font change");
    // And back: one document all along, nothing reopened.
    editor.set_mode(Mode::Embedded);
    let back = frame(&ctx, &mut editor);
    assert_eq!(back.document, Some(doc));
    assert_eq!(editor.host.documents().len(), 1);
    assert_eq!(*ctx.global_style(), *style);
}

#[cfg(feature = "full")]
#[test]
fn window_actions_become_host_events() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    editor.set_mode(Mode::Full);
    let titled = (0..3).flat_map(|_| frame(&ctx, &mut editor).view_events).any(|e| matches!(e, ViewEvent::Window(WindowRequest::Title(t)) if t.contains("Capishot")));
    assert!(titled, "the title reaches the host, not the OS window");
    editor.view.control("app.quit", json!({}));
    let out = frame(&ctx, &mut editor);
    assert!(out.view_events.iter().any(|e| *e == ViewEvent::Window(WindowRequest::Close)), "{:?}", out.view_events);
    let sent = ctx.viewport(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Close | egui::ViewportCommand::Title(_))));
    assert!(!sent, "nothing went to the host's own window");
}

#[test]
fn the_host_chooses_the_keys_in_both_modes() {
    let modes = [
        Mode::Embedded,
        #[cfg(feature = "full")]
        Mode::Full,
    ];
    for mode in modes {
        let ctx = Context::default();
        let (mut editor, doc) = editor_with_doc();
        let layer = editor.host.execute("layer.new.layer", json!({"name": "Ink"})).expect("a layer")["layer"].as_u64().expect("its id");
        editor.set_mode(mode);
        // The Move tool: the digits set the layer's opacity (a painting tool's are its own).
        editor.host.set_tool("move").expect("the move tool");
        editor.view.options_mut().keys = Keys::deny(Shortcut::opacity_digits()).routing(KeyRouting::Always);
        for _ in 0..3 {
            let _ = frame(&ctx, &mut editor);
        }
        let opacity = |editor: &CapishotEditor| {
            let tree = editor.host.inspect(doc, crate::InspectOptions::default()).expect("inspect");
            tree["layers"].as_array().expect("layers").iter().find(|l| l["id"] == json!(layer)).expect("the layer")["opacity"].as_f64().expect("opacity")
        };
        // Taps: each press is released in the next frame, or egui reads the next press as a repeat.
        let release = |k: egui::Key| egui::Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE };
        let press = key(egui::Key::Num5, egui::Modifiers::NONE);
        let before = opacity(&editor);
        let (_, left) = frame_with(&ctx, &mut editor, vec![press.clone()]);
        let _ = frame_with(&ctx, &mut editor, vec![release(egui::Key::Num5)]);
        assert_eq!(opacity(&editor), before, "{mode:?}: denied");
        assert!(left.contains(&press), "{mode:?}: the host still has its key: {left:?}");
        // Without the filter the same key is the editor's: Photoshop's 5 = 50% layer opacity.
        editor.view.options_mut().keys = Keys::default().routing(KeyRouting::Always);
        let (out, left) = frame_with(&ctx, &mut editor, vec![press.clone()]);
        let _ = frame_with(&ctx, &mut editor, vec![release(egui::Key::Num5)]);
        assert!(out.took_keys && !left.contains(&press), "{mode:?}: let through: {left:?}");
        assert!((opacity(&editor) - 0.5).abs() < 0.01, "{mode:?}: 50%: {}", opacity(&editor));
        // A tool letter: denied it stays the host's, let through it picks the tool.
        let b = key(egui::Key::B, egui::Modifiers::NONE);
        editor.view.options_mut().keys = Keys::deny([Shortcut::key(egui::Key::B)]).routing(KeyRouting::Always);
        let (_, left) = frame_with(&ctx, &mut editor, vec![b.clone()]);
        let _ = frame_with(&ctx, &mut editor, vec![release(egui::Key::B)]);
        assert!(left.contains(&b), "{mode:?}");
        assert_eq!(editor.host.tool(), "move", "{mode:?}: denied B");
        editor.view.options_mut().keys = Keys::default().routing(KeyRouting::Always);
        let _ = frame_with(&ctx, &mut editor, vec![b]);
        let _ = frame_with(&ctx, &mut editor, vec![release(egui::Key::B)]);
        let _ = frame(&ctx, &mut editor);
        assert_eq!(editor.host.tool(), "brush", "{mode:?}: B let through");
        // Never: no key at all.
        editor.view.options_mut().keys = Keys::default().routing(KeyRouting::Never);
        let (out, _) = frame_with(&ctx, &mut editor, vec![key(egui::Key::B, egui::Modifiers::NONE)]);
        assert!(!out.took_keys, "{mode:?}");
    }
}

#[test]
fn shortcuts_match_exact_modifiers() {
    let s = Shortcut::command(egui::Key::S).shift();
    assert!(s.matches(&key(egui::Key::S, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)));
    assert!(!s.matches(&key(egui::Key::S, egui::Modifiers::COMMAND)));
    assert!(!s.matches(&egui::Event::Text("s".into())));
    let only = Keys::allow_only([Shortcut::key(egui::Key::B)]);
    let f = only.filter.expect("a filter");
    assert!(f(&key(egui::Key::B, egui::Modifiers::NONE)));
    assert!(!f(&key(egui::Key::V, egui::Modifiers::NONE)));
    assert!(f(&egui::Event::Text("v".into())), "typing still gets through");
}

#[test]
fn a_shown_document_reports_ready_each_time() {
    let ctx = Context::default();
    let mut editor = CapishotEditor::new(None);
    let a = editor.host.open(psd(32, 32), OpenOptions::named("a.psd")).expect("opened").document;
    let b = editor.host.open(psd(32, 32), OpenOptions { name: Some("b.psd".into()), hidden: true }).expect("opened").document;
    // The canvas renders on worker threads: give a loaded machine time, frame by frame.
    let until_ready = |editor: &mut CapishotEditor, doc: DocumentId| {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(20) {
            let events = frame(&ctx, editor).view_events;
            if events.iter().any(|e| matches!(e, ViewEvent::DocumentReady { document, first_frame_ms, final_frame_ms } if *document == doc && *final_frame_ms >= *first_frame_ms)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    };
    assert!(until_ready(&mut editor, a), "a is drawn");
    editor.host.show(b, false).expect("shown");
    assert!(until_ready(&mut editor, b), "b is drawn");
    editor.host.show(a, false).expect("shown");
    assert!(until_ready(&mut editor, a), "a again: new each time it is shown");
}

#[cfg(feature = "full")]
#[test]
fn full_mode_reports_job_ends_to_the_host() {
    let ctx = Context::default();
    let (mut editor, _) = editor_with_doc();
    editor.set_mode(Mode::Full);
    let _ = frame(&ctx, &mut editor);
    let started = editor.host.start("filter.blur.gaussianBlur", json!({"radius": 1.0}), None);
    if let Ok(crate::Started::Job(job)) = started {
        let mut ended = false;
        for _ in 0..2000 {
            ended |= frame(&ctx, &mut editor).events.iter().any(|e| matches!(e, Event::JobFinished { job: j, .. } if *j == job));
            if ended {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ended, "the job's end reached the host");
    }
}

#[test]
fn zoom_by_moves_the_camera_and_bad_factors_are_reported() {
    let ctx = Context::default();
    let (mut editor, doc) = editor_with_doc();
    let _ = frame(&ctx, &mut editor);
    let before = editor.view.camera(doc).expect("a camera").zoom;
    editor.view.set_camera(doc, CameraChange::ZoomBy(2.0));
    let _ = frame(&ctx, &mut editor);
    let after = editor.view.camera(doc).expect("a camera").zoom;
    assert!((after - before * 2.0).abs() < 1e-3, "{before} → {after}");
    editor.view.set_camera(doc, CameraChange::ZoomBy(f32::NAN));
    assert!(frame(&ctx, &mut editor).view_events.iter().any(|e| matches!(e, ViewEvent::Failed { .. })));
}

#[cfg(feature = "full")]
#[test]
fn file_pickers_and_writes_go_to_the_host() {
    let ctx = Context::default();
    let (mut editor, doc) = editor_with_doc();
    editor.set_mode(Mode::Full);
    let _ = frame(&ctx, &mut editor);
    let picker = |events: Vec<ViewEvent>| {
        events.into_iter().find_map(|e| match e {
            ViewEvent::FileDialog(d) => Some(d),
            _ => None,
        })
    };
    // File › Open: the host's picker, its bytes, a new document.
    editor.view.control("ui.menu.invoke", json!({"command": "file.open"}));
    let mut dialog = None;
    for _ in 0..4 {
        dialog = dialog.or(picker(frame(&ctx, &mut editor).view_events));
    }
    let dialog = dialog.expect("the host is asked");
    assert!(matches!(dialog.request(), FileRequest::Open { multiple: true, .. }), "{:?}", dialog.request());
    dialog.contents(vec![("two.psd".into(), psd(20, 10))]);
    let mut opened = false;
    for _ in 0..200 {
        let _ = frame(&ctx, &mut editor);
        opened = editor.host.documents().iter().any(|d| d.name == "two.psd");
        if opened {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(opened, "opened: {:?}", editor.host.documents());
    // File › Save As: the host's picker, then the bytes come to the host.
    let second = editor.host.active_document().expect("active");
    assert_ne!(second, doc);
    editor.view.control("ui.menu.invoke", json!({"command": "file.saveAs"}));
    let mut dialog = None;
    for _ in 0..4 {
        dialog = dialog.or(picker(frame(&ctx, &mut editor).view_events));
    }
    let dialog = dialog.expect("the host is asked where");
    assert!(matches!(dialog.request(), FileRequest::Save { .. }), "{:?}", dialog.request());
    dialog.save_to("out.psd");
    let mut written = None;
    for _ in 0..400 {
        for e in frame(&ctx, &mut editor).view_events {
            if let ViewEvent::WriteFile { name, bytes } = e {
                written = Some((name, bytes.len()));
            }
        }
        if written.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (name, len) = written.expect("the file reached the host");
    assert_eq!(name, "out.psd");
    assert!(len > 0);
}
