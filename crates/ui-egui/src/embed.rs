//! The editor's canvas inside another app's egui context and wgpu device (`capishot-host`'s
//! `CapishotView`, Capikoma request #31).
//!
//! The full app ([`PhotocraftApp`] run by eframe) owns its window: it restyles the whole
//! context, installs its fonts, runs `logic` and `raw_input_hook`, and draws menus, panels and
//! tabs. Embedded, none of that happens. [`frame`] is the one per-frame call: the logic the canvas
//! needs (views for new documents, background jobs, one-frame document switches, the shortcuts
//! the host lets through), then the active document's canvas in the rect the host gives, then
//! (optionally) the editor's own dialogs. It changes no global egui style, visuals, fonts or
//! options: Capishot's colours reach its own drawing through [`crate::theme::publish_tokens`], and
//! text is drawn with the host's fonts.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use egui::{Key, Rect};
use photocraft_engine::Session;
use photocraft_engine::jobs::JobEvent;
use serde_json::Value;

use crate::canvas::ViewXform;
use crate::state::Tool;
use crate::{ControlRequest, PhotocraftApp, Services, shortcuts, theme, type_tool};

pub use eframe::egui_wgpu::RenderState;

/// When the embedded editor takes keys (its shortcuts, undo/redo, the Type tool's typing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyRouting {
    /// Never: the host handles every key (it can still run commands through the API).
    Never,
    /// While the pointer is over the canvas, a canvas gesture is under way or the Type tool is
    /// editing, and no host text field has the keyboard (default).
    #[default]
    WhenActive,
    /// Every frame, unless a host text field has the keyboard.
    Always,
}

/// Which keyboard events the editor may take: `true` lets the event through.
pub type KeyFilter = Arc<dyn Fn(&egui::Event) -> bool + Send + Sync>;

/// The host's say over the keyboard.
#[derive(Clone, Default)]
pub struct Keys {
    /// When the editor takes keys at all.
    pub routing: KeyRouting,
    /// Which key events it may take; `None` is [`default_filter`] (every key but the app
    /// shell's: Open, New, Save, Close, Quit, Print, Preferences).
    pub filter: Option<KeyFilter>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys").field("routing", &self.routing).field("filter", &self.filter.as_ref().map(|_| "custom")).finish()
    }
}

/// The embedded frame's options.
#[derive(Clone, Debug)]
pub struct EmbedOptions {
    /// The host's say over the keyboard.
    pub keys: Keys,
    /// Draw the editor's own dialogs and cards (Remove with AI progress and model download
    /// consent, Rasterize prompts, command dialogs a shortcut opened, notices) as egui windows
    /// in the host's look. Off: a dialog a shortcut opens is closed again at once.
    pub dialogs: bool,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        EmbedOptions { keys: Keys::default(), dialogs: true }
    }
}

/// What [`frame`] did.
#[derive(Debug)]
pub struct EmbedFrame {
    /// A document was drawn (false: no active document; the rect shows the pasteboard).
    pub drawn: bool,
    /// The document ↔ screen mapping of the drawn canvas (inside its rulers), for host overlays.
    pub xform: Option<ViewXform>,
    /// Background jobs that ended this frame. The editor took them from the session, so the host
    /// API must report them (`capishot_host::Host`'s job events).
    pub jobs: Vec<JobEvent>,
    /// The editor took keys this frame.
    pub took_keys: bool,
}

/// The ctx data key marking [`install`] done.
fn installed_id() -> egui::Id {
    egui::Id::unique("capishot-embed-installed")
}

impl PhotocraftApp {
    /// An app for [`frame`]: nothing read from or written to Capishot's preferences on disk,
    /// no startup notices, one document shown at a time (no tabs, tiles or document windows).
    /// The session it holds is a placeholder until the host puts its own in (the host keeps the
    /// real one and lends it for each frame).
    pub fn new_embedded(services: Services) -> Self {
        let mut app = Self::bare(Session::new(), services);
        // The host's context is styled and its fonts are in: there is no setup frame to wait for.
        app.styled = true;
        app.fonts_ready = true;
        app.ui.view.single_document = true;
        app
    }
}

/// Once per context (in the app creator, else [`frame`] does it on the first frame): the context
/// hooks the canvas needs, none of which changes how the host looks: the Type tool's IME pass
/// markers and the tool cursor's per-pass reset. Installed during a pass, they take effect on the
/// next one.
pub fn install(ctx: &egui::Context) {
    if ctx.data(|d| d.get_temp::<bool>(installed_id())).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(installed_id(), true));
    PhotocraftApp::install_type_reset_hooks(ctx);
    ctx.add_plugin(crate::tool_cursor::CursorLifecycle);
}

/// Every shortcut but the app shell's: the ones that open Capishot's own file dialogs, windows or
/// preferences, or close or quit (⌘O, ⌘N, ⌘S, ⇧⌘S, ⌥⌘S, ⌘W, ⌘Q, ⌘P, ⌘K, ⌘,).
pub fn default_filter(e: &egui::Event) -> bool {
    match e {
        egui::Event::Key { key, modifiers, .. } if modifiers.command => {
            !matches!(key, Key::O | Key::N | Key::S | Key::W | Key::Q | Key::P | Key::K | Key::Comma)
        }
        _ => true,
    }
}

/// The keyboard's events: what [`Keys::filter`] decides on. Pointer, wheel and touch events stay
/// where they are.
fn is_keyboard(e: &egui::Event) -> bool {
    matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) | egui::Event::Ime(_))
}

/// Run control-channel method `method` (`ui.set`, `editor.showDocument`, `document.prewarm`,
/// `editor.setMemoryBudget`, `ui.inspect`…; `docs/control-protocol.md`) on the app now. The reply
/// arrives on the receiver: at once, or a few frames later for a method that waits (a document
/// switch waits for its canvas). Call it during [`frame`]'s session loan (the host view does).
pub fn control(app: &mut PhotocraftApp, ctx: &egui::Context, method: &str, params: Value) -> Receiver<Value> {
    let (req, rx) = ControlRequest::new(method, params);
    app.dispatch_control(ctx, req);
    rx
}

/// The editor's tool now.
pub fn tool(app: &PhotocraftApp) -> Tool {
    app.ui.tool
}

/// A document view's camera: zoom (device pixels per document pixel, `1.0` is 100%), the
/// document point at the canvas centre and the rotation in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub zoom: f32,
    pub center: [f32; 2],
    pub rotation: f32,
}

/// How a host moves a document's camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraChange {
    /// Fit the document in the canvas (never past 100%).
    Fit,
    /// Fill the canvas with the document (may crop an edge).
    Fill,
    /// Any of zoom, centre and rotation; `None` keeps that part.
    Set { zoom: Option<f32>, center: Option<[f32; 2]>, rotation: Option<f32> },
}

/// The camera of the view of session document `index` (after [`frame`] made its view).
pub fn camera(app: &PhotocraftApp, index: usize) -> Option<Camera> {
    app.ui.views.get(index).map(|v| Camera { zoom: v.zoom, center: v.center, rotation: v.rotation })
}

/// Move the camera of session document `index` (no animation). Views are made for documents the
/// host opened since the last frame first. `Err` for a document without a view or a value that
/// isn't finite (zoom must be > 0); nothing changes then.
pub fn set_camera(app: &mut PhotocraftApp, index: usize, change: CameraChange) -> Result<(), String> {
    if app.ui.views.len() <= index {
        app.sync_views();
    }
    if let CameraChange::Set { zoom, center, rotation } = change {
        if zoom.is_some_and(|z| !z.is_finite() || z <= 0.0) {
            return Err("zoom must be a finite number above 0".into());
        }
        if center.is_some_and(|c| !c[0].is_finite() || !c[1].is_finite()) || rotation.is_some_and(|r| !r.is_finite()) {
            return Err("center and rotation must be finite".into());
        }
    }
    let Some(view) = app.ui.views.get_mut(index) else { return Err(format!("no view for document {index}")) };
    view.zoom_animating = false;
    view.zoom_anchor = None;
    view.display_camera = None;
    view.zoom_transition = None;
    match change {
        CameraChange::Fit => view.fit_pending = true,
        CameraChange::Fill => view.fill_pending = true,
        CameraChange::Set { zoom, center, rotation } => {
            if let Some(z) = zoom {
                view.zoom = z;
                view.fit_pending = false;
                view.fill_pending = false;
            }
            if let Some(c) = center {
                view.center = c;
            }
            if let Some(r) = rotation {
                // (−180, 180], as the Rotate View tool keeps it.
                let r = r.rem_euclid(360.0);
                view.rotation = if r > 180.0 { r - 360.0 } else { r };
            }
        }
    }
    Ok(())
}

/// One embedded frame: the editor's logic, then the active document's canvas in `rect` (egui
/// points, inside `ui`), then the editor's dialogs when [`EmbedOptions::dialogs`]. `app.session`
/// must hold the host's session for the call. Never changes the context's style, fonts or options.
pub fn frame(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, opts: &EmbedOptions) -> EmbedFrame {
    let ctx = ui.ctx().clone();
    install(&ctx);
    // The host's fonts: Capishot's named weights don't exist in its context.
    let _fonts = crate::theme::HostFonts::enter();
    let (jobs, took_keys) = logic(app, &ctx, rect, opts);
    let drawn = draw(app, ui, rect, opts);
    let xform = if drawn { ViewXform::active(app) } else { None };
    EmbedFrame { drawn, xform, jobs, took_keys }
}

/// `logic_frame`'s part the canvas needs. Returns the ended jobs and whether keys were taken.
fn logic(app: &mut PhotocraftApp, ctx: &egui::Context, rect: Rect, opts: &EmbedOptions) -> (Vec<JobEvent>, bool) {
    crate::i18n::set_current(crate::i18n::Lang::from_pref(&app.session.prefs().interface.language));
    app.frame += 1;
    app.perf_counters.frames += 1;
    let now = ctx.input(|i| i.time);
    let dt = (now - app.last_frame_time) as f32;
    if dt > 0.0 {
        app.fps = app.fps * 0.9 + (1.0 / dt).min(240.0) * 0.1;
    }
    app.last_frame_time = now;
    // Documents the host opened, closed or reordered since the last frame get (or lose) their view.
    app.sync_views();
    crate::memory_warning::tick(app, now);
    app.check_gpu(ctx);
    // Documents waiting to be shown or prewarmed: a ready one swaps in before this frame draws.
    crate::doc_switch::tick(app, ctx);
    // Switching tools commits, also between the Type and Type Mask tools.
    if app.ui.text_edit.as_ref().is_some_and(|ed| !app.ui.tool.is_type() || ed.mask != app.ui.tool.is_type_mask()) {
        type_tool::commit(app);
    }
    type_tool::end_if_left(app);
    type_tool::smart_type_tick(app);
    if app.ui.pen.as_ref().is_some_and(|p| !p.drawn_with(app.ui.tool)) {
        crate::vector_ui::pen_commit(app, false);
    }
    crate::transform_tool::end_if_left(app);
    crate::shape_stroke_ui::sync_editor(app, ctx);
    let jobs = crate::jobs_ui::tick_events(app, ctx);
    if matches!(app.ui.tool, Tool::BubbleTextRemover | Tool::BubbleType) {
        crate::typer::warm_page(app, ctx);
    }
    // Fonts the host registered, and documents waiting for theirs (`fontsReady`).
    crate::user_fonts_ui::tick(app, Some(ctx));
    if crate::user_fonts_ui::checks_pending(app) {
        crate::user_fonts_ui::tick_checks(app, Some(now));
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    let took_keys = if type_tool::in_ui_pass(ctx) { keys(app, ctx, rect, &opts.keys) } else { false };
    if !opts.dialogs {
        // Nothing would draw a dialog a shortcut opened, and an open one blocks the canvas.
        app.ui.dialogs.clear();
    }
    (jobs, took_keys)
}

/// Whether the editor takes keys this frame under `routing`.
fn takes_keys(app: &PhotocraftApp, ctx: &egui::Context, rect: Rect, routing: KeyRouting) -> bool {
    // A host text field (or any focused host widget) keeps its keys.
    if ctx.text_edit_focused() || (ctx.egui_wants_keyboard_input() && app.ui.text_edit.is_none()) {
        return false;
    }
    match routing {
        KeyRouting::Never => false,
        KeyRouting::Always => true,
        KeyRouting::WhenActive => {
            let hovered = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)));
            hovered || app.drag.is_some() || app.ui.text_edit.is_some()
        }
    }
}

/// The editor's shortcuts on the keyboard events the host lets through; the rest, and what the
/// editor left, stay in the input for the host's widgets after this one. Returns whether keys
/// were taken.
fn keys(app: &mut PhotocraftApp, ctx: &egui::Context, rect: Rect, keys: &Keys) -> bool {
    if !app.fonts_ready || !takes_keys(app, ctx, rect, keys.routing) {
        type_tool::controls_only(app, ctx);
        return false;
    }
    let allow = |e: &egui::Event| keys.filter.as_ref().map_or_else(|| default_filter(e), |f| f(e));
    let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
    let (mine, withheld): (Vec<egui::Event>, Vec<egui::Event>) = events.into_iter().partition(|e| !is_keyboard(e) || allow(e));
    // ⌘C / ⌘X / ⌘V as the editor reads them (the full app does this in `raw_input_hook`).
    let typing = app.ui.text_edit.is_some();
    let mut raw = egui::RawInput { events: mine, ..Default::default() };
    shortcuts::clipboard_keys(ctx, typing, &mut raw);
    ctx.input_mut(|i| i.events = raw.events);
    shortcuts::handle(app, ctx);
    ctx.input_mut(|i| {
        let left = std::mem::take(&mut i.events);
        i.events = withheld;
        i.events.extend(left);
    });
    true
}

/// `ui_frame`'s part the canvas needs: the canvas in `rect`, the editor's windows, the IME tail.
fn draw(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, opts: &EmbedOptions) -> bool {
    let ctx = ui.ctx().clone();
    if !app.ui.dialogs.is_empty() || app.ui.text_edit.is_none() || ctx.text_edit_focused() || ctx.egui_wants_keyboard_input() {
        type_tool::controls_only(app, &ctx);
    }
    app.hover_layer = None;
    crate::eyedrop::begin_frame(app, &ctx);
    // Each painting tool keeps its own brush: switch the active tool's in before anything reads it.
    crate::paint_mouse::sync_tool_brush(app);
    crate::render_path::frame(app);
    let clip = rect.intersect(ui.clip_rect());
    let mut canvas_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("capishot-embedded-canvas"));
    canvas_ui.set_clip_rect(clip);
    let drawn = crate::canvas::embedded_area(app, &mut canvas_ui, rect);
    if opts.dialogs {
        windows(app, &ctx);
    }
    crate::shape_stroke_ui::finish_frame(app, &ctx);
    // Whether an overlay is showing, so the next press that closes it stays off the canvas.
    crate::overlay_guard::end_frame(app, &ctx);
    crate::jobs_ui::keep_polling(app, &ctx);
    // A device lost while drawing this frame: switch to the CPU canvas before the next one.
    crate::gpu_status::check(app, &ctx);
    // The Type tool's IME descriptor: the last writer of the editor's part of the pass.
    type_tool::sync_ime(app, &ctx);
    app.finish_type_reset_pass(&ctx);
    ui.advance_cursor_after_rect(rect);
    drawn
}

/// The editor's own windows over the host's UI, in the host's look.
fn windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    crate::dialogs::show(app, ctx);
    crate::remove_ai_ui::show(app, ctx);
    crate::rasterize_prompt::run_pending(app, ctx);
    crate::jobs_ui::dialog(app, ctx);
    crate::ai_progress::show(app, ctx);
    crate::ai_models_ui::window(app, ctx);
    crate::bubble_type_ui::picker(app, ctx);
    crate::notices::show(app, ctx);
    crate::tool_toast::draw(app, ctx);
}

/// Capishot's colour tokens for a host's look: the built-in Studio theme (dark or light as the
/// host's visuals) with the host's colours where they matter on and around the canvas.
pub fn tokens_for_visuals(v: &egui::Visuals) -> theme::Tokens {
    let kind = if v.dark_mode { theme::ThemeKind::Studio } else { theme::ThemeKind::StudioLight };
    let mut t = theme::Tokens::for_kind(kind);
    t.dark = v.dark_mode;
    t.chrome = v.panel_fill;
    t.canvas = v.extreme_bg_color;
    t.card = v.window_fill;
    t.card_border = v.window_stroke.color;
    t.field = v.extreme_bg_color;
    t.text = v.text_color();
    t.text_dim = v.weak_text_color();
    t.accent = v.selection.bg_fill;
    t.accent_text = v.selection.stroke.color;
    t.hover = v.widgets.hovered.bg_fill;
    t.pressed = v.widgets.active.bg_fill;
    t.separator = v.widgets.noninteractive.bg_stroke.color;
    t.danger = v.error_fg_color;
    t.warning = v.warn_fg_color;
    t
}

#[cfg(test)]
mod tests;
