//! `CapishotView::ui_tiles` in a host's context, without a window or GPU (the CPU canvas path; the
//! GPU composite is tested in `photocraft-ui-egui`'s `canvas/tiles.rs`).

use egui::{Context, Pos2, RawInput, Rect, vec2};
use serde_json::json;

use super::*;

fn input() -> RawInput {
    RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), focused: true, ..Default::default() }
}

fn editor_with_doc() -> (CapishotEditor, DocumentId) {
    let mut editor = CapishotEditor::new(None);
    editor.host.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
    let doc = editor.host.active_document().expect("active");
    (editor, doc)
}

fn tiles_frame(ctx: &Context, editor: &mut CapishotEditor, tiles: &[Tile]) -> TilesOutput {
    let mut out = None;
    let mut o = ctx.run_ui(input(), |ui| {
        out = Some(editor.ui_tiles(ui, tiles));
    });
    o.textures_delta.clear();
    out.expect("the frame ran")
}

fn left() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(300.0, 400.0))
}

fn right() -> Rect {
    Rect::from_min_size(Pos2::new(400.0, 0.0), vec2(300.0, 400.0))
}

#[test]
fn tiles_draw_one_canvas_per_document_the_active_one_live() {
    let (mut editor, a) = editor_with_doc();
    editor.host.execute("file.new", json!({"width": 64, "height": 48})).expect("a second document");
    let b = editor.host.active_document().expect("active");
    let ctx = Context::default();
    let tiles = [Tile::new(a, left()), Tile::from((b, right()))];
    let out = tiles_frame(&ctx, &mut editor, &tiles);
    assert!(out.tiles.iter().all(|t| t.drawn && t.error.is_none()), "{:?}", out.tiles);
    assert!(!out.tiles[0].live && out.tiles[1].live);
    assert_eq!(out.view.document, Some(b));
    assert_eq!(out.tiles[0].transform.expect("a mapping").document(), a);
    assert!(left().contains(out.tiles[0].transform.expect("a mapping").canvas_rect().center()));
    assert!(out.tiles[0].overlay().is_some(), "host overlays per tile");
    assert_eq!(out.view.transform.expect("the live tile's").document(), b);
    // One camera by default: the host moves the active document's, every tile follows.
    editor.view.set_camera(b, CameraChange::Set { zoom: Some(4.0), center: Some([20.0, 20.0]), rotation: None });
    tiles_frame(&ctx, &mut editor, &tiles);
    let out = tiles_frame(&ctx, &mut editor, &tiles);
    let zoom = |o: &TileOutput| o.transform.expect("a mapping").zoom();
    assert_eq!(zoom(&out.tiles[0]), zoom(&out.tiles[1]));
    // Own cameras: the same move now reaches one tile.
    editor.view.options_mut().tile_camera = TileCamera::Own;
    editor.view.set_camera(b, CameraChange::Set { zoom: Some(8.0), center: None, rotation: None });
    tiles_frame(&ctx, &mut editor, &tiles);
    let out = tiles_frame(&ctx, &mut editor, &tiles);
    assert_ne!(zoom(&out.tiles[0]), zoom(&out.tiles[1]));
}

#[test]
fn a_tile_shows_a_named_view_without_touching_the_layers() {
    let (mut editor, doc) = editor_with_doc();
    let layer = editor.host.execute("layer.new.layer", json!({"name": "text"})).expect("a layer")["layer"].as_u64().expect("an id");
    editor.host.define_view(doc, "clean", json!({"hidden": [layer]})).expect("a view");
    let before = editor.host.inspect(doc, crate::InspectOptions::default()).expect("inspect");
    let revision = editor.host.documents().iter().find(|d| d.id == doc).map(|d| d.revision);
    let ctx = Context::default();
    let tiles = [Tile::new(doc, left()), Tile::new(doc, right()).with_view("clean")];
    let out = tiles_frame(&ctx, &mut editor, &tiles);
    assert!(out.tiles[0].live && !out.tiles[1].live);
    assert!(out.tiles[1].drawn, "{:?}", out.tiles[1].error);
    assert_eq!(editor.host.inspect(doc, crate::InspectOptions::default()).expect("inspect"), before, "the layers stay as they were");
    assert_eq!(editor.host.documents().iter().find(|d| d.id == doc).map(|d| d.revision), revision, "no edit");
    let out = tiles_frame(&ctx, &mut editor, &[Tile::new(doc, left()).with_view("nope")]);
    assert!(!out.tiles[0].drawn && out.tiles[0].error.as_deref().is_some_and(|e| e.contains("nope")));
    assert!(out.view.transform.is_none(), "no live tile");
}

#[test]
fn bad_tiles_report_and_the_rest_draw() {
    let (mut editor, doc) = editor_with_doc();
    let ctx = Context::default();
    let nowhere = Rect::from_min_max(Pos2::new(f32::NAN, 0.0), Pos2::new(f32::INFINITY, -1.0));
    let tiles = [Tile::new(DocumentId(987_654), left()), Tile::new(doc, nowhere), Tile::new(doc, right())];
    let out = tiles_frame(&ctx, &mut editor, &tiles);
    assert!(out.tiles[0].error.is_some() && out.tiles[1].error.is_some());
    assert!(out.tiles[2].drawn && out.tiles[2].live);
    assert!(out.view.view_events.iter().all(|e| !matches!(e, ViewEvent::Failed { .. })), "{:?}", out.view.view_events);
    assert!(tiles_frame(&ctx, &mut editor, &[]).tiles.is_empty(), "no tiles is a frame");
}

#[cfg(feature = "full")]
#[test]
fn full_mode_reports_every_tile_instead_of_drawing_it() {
    let (mut editor, doc) = editor_with_doc();
    editor.set_mode(Mode::Full);
    let ctx = Context::default();
    let out = tiles_frame(&ctx, &mut editor, &[Tile::new(doc, left())]);
    assert!(!out.tiles[0].drawn && out.tiles[0].error.is_some());
}

/// A frame with these pointer events.
fn pointer_frame(ctx: &Context, editor: &mut CapishotEditor, tiles: &[Tile], events: Vec<egui::Event>) {
    let raw = RawInput { events, ..input() };
    let mut o = ctx.run_ui(raw, |ui| {
        editor.ui_tiles(ui, tiles);
    });
    o.textures_delta.clear();
}

/// Each tile's scrollbars are its own: a drag on the live tile's moves the page the way it goes,
/// with another tile of the document beside it (they shared their widget ids: the wrong way).
#[test]
fn a_drag_on_a_tiles_scrollbar_goes_the_right_way() {
    let mut editor = CapishotEditor::new(None);
    editor.host.execute("file.new", json!({"width": 760, "height": 1200})).expect("a document");
    let doc = editor.host.active_document().expect("active");
    let layer = editor.host.execute("layer.new.layer", json!({"name": "text"})).expect("a layer")["layer"].as_u64().expect("an id");
    editor.host.define_view(doc, "other", json!({"hidden": [layer]})).expect("a view");
    let ctx = Context::default();
    let tiles = [Tile::new(doc, left()), Tile::new(doc, right()).with_view("other")];
    tiles_frame(&ctx, &mut editor, &tiles);
    editor.view.set_camera(doc, CameraChange::Set { zoom: Some(2.0), center: Some([380.0, 400.0]), rotation: None });
    tiles_frame(&ctx, &mut editor, &tiles);
    tiles_frame(&ctx, &mut editor, &tiles);
    let y = |e: &CapishotEditor| e.view.camera(doc).expect("a camera").center[1];
    let before = y(&editor);
    // On the thumb of the left tile's vertical bar, 60 points down.
    let x = left().right() - 6.0;
    let thumb = left().top() + left().height() * before / 1200.0;
    let at = |dy: f32| Pos2::new(x, thumb + dy);
    let press = |pressed| egui::Event::PointerButton { pos: at(if pressed { 0.0 } else { 60.0 }), button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    pointer_frame(&ctx, &mut editor, &tiles, vec![egui::Event::PointerMoved(at(0.0))]);
    pointer_frame(&ctx, &mut editor, &tiles, vec![press(true)]);
    for dy in [20.0, 40.0, 60.0] {
        pointer_frame(&ctx, &mut editor, &tiles, vec![egui::Event::PointerMoved(at(dy))]);
    }
    pointer_frame(&ctx, &mut editor, &tiles, vec![press(false)]);
    assert!(y(&editor) > before + 40.0, "{before} -> {}", y(&editor));
}
