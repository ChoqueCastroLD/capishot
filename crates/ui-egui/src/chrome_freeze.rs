//! The chrome around the canvas (menu bar, options bar, status bar, toolbar, dock) holds still
//! while a tool is dragged on the canvas: nothing it shows changes until the gesture ends (the
//! stroke, move or selection commits on release). Laying it out every frame of the drag cost as
//! much as the tool itself on large layer stacks (the Layers, Properties and Color panels), so
//! while a canvas gesture runs, the frame repaints the shapes the chrome drew last time and
//! gives the canvas the area it had, instead of building the chrome again. The canvas area is a
//! child `Ui` with a fixed id, so the canvas's widget ids don't depend on what the chrome built.
//!
//! A frame builds the chrome for real (and keeps its shapes) whenever anything it shows could
//! have changed: no gesture, the pointer outside the canvas, any input besides pointer moves, a
//! different window size, scale, theme, tool, document, revision, layer selection or status
//! line, an open dialog or menu, or every [`REFRESH_FRAMES`] frames.

use egui::epaint::ClippedShape;
use egui::{LayerId, Rect};

use crate::PhotocraftApp;

/// A long gesture still rebuilds the chrome this often (frames), so nothing can stay stale for
/// long.
pub const REFRESH_FRAMES: u32 = 30;

/// What the chrome drew last, and what it depended on.
#[derive(Default)]
pub struct State {
    snap: Option<Snapshot>,
    /// Frames repainted from the snapshot in a row.
    reused: u32,
}

impl State {
    /// This frame repaints the chrome from its snapshot (a canvas gesture is under way).
    pub fn is_frozen(&self) -> bool {
        self.reused > 0
    }
}

struct Snapshot {
    key: u64,
    layer: LayerId,
    /// What the chrome's panels left for the canvas.
    area: Rect,
    shapes: Vec<ClippedShape>,
    /// Textures the chrome loaded through egui's loaders (icons): asked for again on every
    /// repainted frame, or the texture loader frees the ones no frame used.
    loads: Vec<Load>,
}

/// The identity of everything the chrome shows that a canvas gesture could change.
fn key(app: &PhotocraftApp, ctx: &egui::Context, ui: &egui::Ui, chrome: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    chrome.hash(&mut h);
    let screen = ctx.content_rect();
    for v in [screen.min.x, screen.min.y, screen.max.x, screen.max.y, ctx.pixels_per_point(), ui.max_rect().width(), ui.max_rect().height()] {
        v.to_bits().hash(&mut h);
    }
    format!("{:?}{:?}{:?}", app.ui.theme, app.ui.tool, app.tool_override).hash(&mut h);
    app.ui.status.hash(&mut h);
    app.ui.status_error.hash(&mut h);
    app.ui.dialogs.len().hash(&mut h);
    app.ui.windows.len().hash(&mut h);
    app.session.active_index().hash(&mut h);
    app.session.documents().len().hash(&mut h);
    if let Some(st) = app.session.active() {
        st.doc.id.0.hash(&mut h);
        st.revision.hash(&mut h);
        st.active_layer.map(|l| l.0).hash(&mut h);
        st.selected_layers().len().hash(&mut h);
    }
    app.session.tools.brush.size.to_bits().hash(&mut h);
    h.finish()
}

/// Whether this frame may repaint the chrome from its snapshot: a gesture on the canvas with
/// the pointer over it, and nothing but pointer moves as input.
fn may_reuse(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    if app.canvas_gesture_tool(ctx).is_none() || app.ui.transform.is_some() || app.ui.text_edit.is_some() {
        return false;
    }
    // Only a gesture that started on the canvas of an open document: a press on the chrome
    // (menus, toolbar, panels) must keep its widgets laid out every frame until the release,
    // or egui loses the pressed widget and the click never happens (a real mouse moves a few
    // pixels between press and release).
    if app.session.active().is_none() {
        return false;
    }
    let canvas = app.last_canvas_rect;
    if !canvas.is_finite() || canvas.width() <= 0.0 || canvas.height() <= 0.0 {
        return false;
    }
    ctx.input(|i| {
        i.pointer.primary_down()
            && i.pointer.press_origin().is_some_and(|p| canvas.contains(p))
            && i.pointer.latest_pos().is_some_and(|p| canvas.contains(p))
            && i.events.iter().all(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_)))
    }) && !ctx.any_popup_open()
        && !ctx.text_edit_focused()
}

/// Shows the chrome with `draw`, or repaints it from the last frame's shapes while a canvas
/// gesture runs (see the module docs). `chrome` is whether it is shown at all (screen modes).
/// Returns the area left for the canvas.
pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, chrome: bool, draw: impl FnOnce(&mut PhotocraftApp, &mut egui::Ui)) -> Rect {
    let ctx = ui.ctx().clone();
    let key = key(app, &ctx, ui, chrome);
    let layer = ui.layer_id();
    if may_reuse(app, &ctx)
        && app.chrome_freeze.reused < REFRESH_FRAMES
        && let Some(s) = app.chrome_freeze.snap.as_ref().filter(|s| s.key == key && s.layer == layer)
    {
        let area = s.area;
        let shapes = s.shapes.clone();
        for (uri, options, hint) in &s.loads {
            let _ = ctx.try_load_texture(uri, *options, *hint);
        }
        ctx.graphics_mut(|g| {
            let list = g.entry(layer);
            for c in shapes {
                list.add(c.clip_rect, c.shape);
            }
        });
        app.chrome_freeze.reused += 1;
        app.perf_counters.chrome_reused += 1;
        return area;
    }
    let start = ctx.graphics(|g| g.get(layer).map_or(0, |l| l.all_entries().len()));
    install_recorder(&ctx);
    recording(|r| *r = Some(Vec::new()));
    draw(app, ui);
    let mut loads = recording(Option::take).unwrap_or_default();
    loads.sort_by(|a, b| a.0.cmp(&b.0));
    loads.dedup();
    let shapes: Vec<ClippedShape> = ctx.graphics(|g| g.get(layer).map(|l| l.all_entries().skip(start).cloned().collect()).unwrap_or_default());
    app.chrome_freeze.reused = 0;
    let area = ui.available_rect_before_wrap();
    app.chrome_freeze.snap = Some(Snapshot { key, layer, area, shapes, loads });
    area
}

type Load = (String, egui::TextureOptions, egui::load::SizeHint);

fn recorder() -> &'static std::sync::Mutex<Option<Vec<Load>>> {
    static R: std::sync::OnceLock<std::sync::Mutex<Option<Vec<Load>>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

fn recording<R>(f: impl FnOnce(&mut Option<Vec<Load>>) -> R) -> R {
    f(&mut recorder().lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// A texture loader that loads nothing: it notes the loads asked for while the chrome is built
/// and passes them on to the real loaders.
struct Recorder;

impl egui::load::TextureLoader for Recorder {
    fn id(&self) -> &str {
        "photocraft::chrome_freeze::Recorder"
    }
    fn load(&self, _ctx: &egui::Context, uri: &str, options: egui::TextureOptions, hint: egui::load::SizeHint) -> egui::load::TextureLoadResult {
        recording(|r| {
            if let Some(v) = r.as_mut() {
                v.push((uri.to_string(), options, hint));
            }
        });
        Err(egui::load::LoadError::NotSupported)
    }
    fn forget(&self, _uri: &str) {}
    fn forget_all(&self) {}
    fn byte_size(&self) -> usize {
        0
    }
}

fn install_recorder(ctx: &egui::Context) {
    let id = egui::Id::new("photocraft-chrome-recorder");
    if ctx.data(|d| d.get_temp::<bool>(id)).is_none() {
        ctx.add_texture_loader(std::sync::Arc::new(Recorder));
        ctx.data_mut(|d| d.insert_temp(id, true));
    }
}

/// The canvas area's `Ui`: a child of `ui` over `area` with a fixed id (see the module docs).
pub fn canvas_ui(ui: &mut egui::Ui, area: Rect) -> egui::Ui {
    ui.new_child(egui::UiBuilder::new().id(egui::Id::new("photocraft-canvas-area")).max_rect(area).layout(*ui.layout()))
}
