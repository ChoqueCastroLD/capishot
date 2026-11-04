//! The editor as an egui widget (Capikoma request #31).
//!
//! [`CapishotView`] draws the active document's canvas in a rect of the host's own eframe/egui
//! context, on the host's wgpu device (one device for the whole app), with Capishot's tools,
//! shortcuts (as far as the host lets them through) and the Type tool's IME. It changes no global
//! egui style, visuals, fonts or options: the host keeps its theme and fonts, and the canvas
//! chrome takes the host's colours ([`HostTheme`]). Its per-frame work is one call:
//!
//! ```no_run
//! # use capishot_host::{Host, view::CapishotView};
//! # fn frame(ui: &mut egui::Ui, host: &mut Host, view: &mut CapishotView) {
//! let out = view.ui(ui, host); // logic, jobs, events, canvas, dialogs
//! if let Some(overlay) = out.overlay() {
//!     overlay.doc_rect_stroke([10.0, 10.0, 200.0, 80.0], (2.0, egui::Color32::YELLOW));
//! }
//! for event in &out.events { /* layerPixelsChanged, toolChanged… */ }
//! # }
//! ```
//!
//! [`CapishotEditor`] bundles a [`Host`] and its view for hosts that want one object.
//!
//! **The session is lent.** The [`Host`] owns the documents. For the length of [`CapishotView::ui`]
//! the view borrows the host's session (a move, not a copy) and gives it back before returning,
//! also when the editor panics (the panic is caught and reported as [`ViewEvent::Failed`]). Every
//! [`Host`] call works between frames as usual; documents the host opens get a view (fit to the
//! canvas) on the next frame without any sync call.
//!
//! **Events.** With a view attached, the view polls the host every frame ([`ViewOutput::events`];
//! subscribers and channels get them too). Don't also call [`Host::poll`]: the editor takes the
//! ended jobs from the session to finish them on the canvas and hands them to the host itself.
//!
//! **Keys.** [`ViewOptions::keys`]: by default the editor takes keys while the pointer is over the
//! canvas, a canvas gesture is under way or the Type tool is editing, never while a host text
//! field has the keyboard, and never the app shell's shortcuts (Open, New, Save, Close, Quit,
//! Print, Preferences). A host filter decides key by key ([`Keys::filter`]); the keys the editor
//! doesn't take stay in egui's input for the host's widgets drawn after the view.

mod hooks;
mod overlay;
mod theme;

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use egui::{Rect, Sense};
use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::embed::{self, EmbedOptions};
use serde_json::{Value, json};

pub use overlay::{CanvasTransform, Overlay};
pub use photocraft_ui_egui::embed::{Camera, CameraChange, KeyFilter, KeyRouting, Keys, RenderState, default_filter};
pub use theme::HostTheme;

use crate::{DocumentId, Event, Host};
use hooks::{Awaited, Hooks, Queue, Request};

/// The camera a document gets when it is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DocumentCamera {
    /// Its own (fit to the canvas the first time it shows).
    #[default]
    Own,
    /// The camera of the document shown before it (zoom, centre, rotation): flipping pages at the
    /// same spot.
    Keep,
    /// `zoom` (device pixels per document pixel, `1.0` is 100%) with document point `center` in
    /// the middle of the canvas.
    At {
        /// Device pixels per document pixel.
        zoom: f32,
        /// The document point at the canvas centre.
        center: [f32; 2],
    },
}

/// The widget's options; change them any time ([`CapishotView::options_mut`]).
#[derive(Clone, Debug)]
pub struct ViewOptions {
    /// The host's say over the keyboard.
    pub keys: Keys,
    /// Draw the editor's own dialogs and cards (Remove with AI progress and model consent,
    /// prompts, notices) as egui windows in the host's look. Off: dialogs a shortcut opens are
    /// closed at once; the host runs commands through the API instead.
    pub dialogs: bool,
    /// The canvas chrome's colours; `None` follows the host's egui visuals every frame.
    pub theme: Option<HostTheme>,
    /// Rulers along the canvas (View › Rulers); `None` leaves them as the editor has them.
    pub rulers: Option<bool>,
}

impl Default for ViewOptions {
    fn default() -> Self {
        ViewOptions { keys: Keys::default(), dialogs: true, theme: None, rulers: Some(false) }
    }
}

/// Something the view reports (besides the host's [`Event`]s).
///
/// `#[non_exhaustive]`: new kinds may be added in a minor release; match with a `_` arm.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ViewEvent {
    /// [`CapishotView::show_document`] finished: the document is shown (in the frame its canvas
    /// was ready), or why not. The result is the control protocol's `editor.showDocument` reply
    /// (`{id, frames, ms, timedOut, shown, index, gpuBytes}`).
    Shown {
        /// The document.
        document: DocumentId,
        /// The reply.
        result: std::result::Result<Value, String>,
    },
    /// [`CapishotView::prewarm`] (or [`Host::prewarm`]) finished: its canvas is ready to show at
    /// once (`document.prewarm`'s reply).
    Prewarmed {
        /// The document.
        document: DocumentId,
        /// The reply.
        result: std::result::Result<Value, String>,
    },
    /// The reply to [`CapishotView::control`] `token`.
    Reply {
        /// What [`CapishotView::control`] returned.
        token: u64,
        /// The reply.
        result: std::result::Result<Value, String>,
    },
    /// The active document's camera moved (the person zoomed, panned or turned the view, or the
    /// host did).
    CameraChanged {
        /// The document.
        document: DocumentId,
        /// Its camera now.
        camera: Camera,
    },
    /// Something the view was asked failed, or the editor hit an internal error in this frame
    /// (caught; the documents are intact).
    Failed {
        /// What failed, for people.
        message: String,
    },
}

/// What one [`CapishotView::ui`] call did.
#[derive(Debug)]
pub struct ViewOutput {
    /// The host's events since the last frame (as [`Host::poll`] returns them).
    pub events: Vec<Event>,
    /// The view's own events.
    pub view_events: Vec<ViewEvent>,
    /// Hover over the widget's whole rect (canvas input is the editor's; use it for hover and
    /// context the host shows around the canvas).
    pub response: egui::Response,
    /// The document drawn, `None` without an active document.
    pub document: Option<DocumentId>,
    /// The canvas mapping, `None` without a drawn document.
    pub transform: Option<CanvasTransform>,
    /// The document point under the pointer, when it is over the canvas.
    pub hover: Option<[f64; 2]>,
    /// The editor took keys this frame.
    pub took_keys: bool,
    overlay: Option<Overlay>,
}

impl ViewOutput {
    /// A painter over the canvas with its mapping, for host overlays in document coordinates.
    /// Draw right after [`CapishotView::ui`]: it paints above the document.
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }
}

/// The editor's canvas as an egui widget. One per editor area; not `Send` (it lives with the UI).
pub struct CapishotView {
    app: Box<PhotocraftApp>,
    options: ViewOptions,
    queue: Queue,
    attached: bool,
    pending: Vec<(u64, Awaited, Receiver<Value>)>,
    /// The tool id the host and the editor last agreed on.
    synced_tool: Option<String>,
    /// Each document's camera as last reported.
    cameras: HashMap<DocumentId, Camera>,
    /// The active document and its camera after the last frame.
    last_active: Option<(DocumentId, Camera)>,
}

impl std::fmt::Debug for CapishotView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapishotView").field("options", &self.options).field("attached", &self.attached).finish()
    }
}

impl CapishotView {
    /// A view drawing on the host's wgpu device: `cc.wgpu_render_state.clone()` in the eframe app
    /// creator (or `frame.wgpu_render_state().cloned()`). `None` draws the canvas with egui
    /// textures (the CPU path).
    pub fn new(render_state: Option<RenderState>) -> Self {
        Self::with_services(render_state, photocraft_ui_egui::Services::default())
    }

    /// **Unstable.** [`Self::new`] with the editor's platform services (clipboard, beep, file
    /// pickers…); their shape may change in any release.
    pub fn with_services(render_state: Option<RenderState>, services: photocraft_ui_egui::Services) -> Self {
        let mut app = Box::new(PhotocraftApp::new_embedded(services));
        if let Some(rs) = render_state {
            app.set_wgpu(rs);
        }
        CapishotView {
            app,
            options: ViewOptions::default(),
            queue: Queue::default(),
            attached: false,
            pending: Vec::new(),
            synced_tool: None,
            cameras: HashMap::new(),
            last_active: None,
        }
    }

    /// Optional, in the eframe app creator: the context hooks the editor needs (the Type tool's IME
    /// pass markers, the tool cursor's reset). Without it the first frame installs them and they
    /// work from the second. Changes nothing of the host's look.
    pub fn install(ctx: &egui::Context) {
        embed::install(ctx);
    }

    /// The options.
    pub fn options(&self) -> &ViewOptions {
        &self.options
    }

    /// The options, to change.
    pub fn options_mut(&mut self) -> &mut ViewOptions {
        &mut self.options
    }

    /// Whether the canvas draws on the GPU (false on the CPU path or after a device loss).
    pub fn gpu_active(&self) -> bool {
        self.app.gpu_active()
    }

    /// **Unstable.** The editor app behind the widget (its UI state, caches). Its session is a
    /// placeholder outside [`Self::ui`]. Its API may change in any release.
    pub fn app(&self) -> &PhotocraftApp {
        &self.app
    }

    /// **Unstable.** [`Self::app`], mutably.
    pub fn app_mut(&mut self) -> &mut PhotocraftApp {
        &mut self.app
    }

    /// Make `host`'s [`Host::show`] and [`Host::prewarm`] reach this view. [`Self::ui`] does it on
    /// its first call; call it earlier to prewarm before the first frame.
    pub fn attach(&mut self, host: &mut Host) {
        if !self.attached {
            host.set_view_hooks(Box::new(Hooks { queue: self.queue.clone() }));
            self.attached = true;
        }
    }

    /// Show `document` in one step, double-buffered: the document shown now stays on screen until
    /// the new one's canvas is drawn, then they swap in a single frame (no blank or half-drawn
    /// frame). A hidden document becomes shown; with `hide_previous` the one shown before becomes
    /// hidden (it keeps its canvas until the memory budget needs it). Answered by
    /// [`ViewEvent::Shown`]. ([`Host::show`] swaps at once instead.)
    pub fn show_document(&mut self, document: DocumentId, hide_previous: bool, camera: DocumentCamera) {
        let mut p = json!({"id": document.0, "hidePrevious": hide_previous});
        if let Some(v) = viewport(camera) {
            p["viewport"] = v;
        }
        self.queue.control("editor.showDocument", p, Awaited::Show(document));
    }

    /// Draw `document`'s canvas ahead (its view, or the whole image at full size with `full`) so
    /// that showing it later takes one frame. Answered by [`ViewEvent::Prewarmed`].
    pub fn prewarm(&mut self, document: DocumentId, full: bool, camera: DocumentCamera) {
        let mut p = json!({"id": document.0, "full": full});
        if let Some(v) = viewport(camera) {
            p["viewport"] = v;
        }
        self.queue.control("document.prewarm", p, Awaited::Prewarm(document));
    }

    /// The camera [`Host::show`] gives the document it shows.
    pub fn set_show_camera(&mut self, camera: DocumentCamera) {
        self.queue.set_show_camera(camera);
    }

    /// The canvas textures every open document may hold together, in megabytes (`None`: no
    /// budget). Over it, the least recently shown documents' textures are dropped (the documents
    /// stay open and are drawn again when shown).
    pub fn set_memory_budget(&mut self, mb: Option<f64>) {
        self.queue.control("editor.setMemoryBudget", json!({"mb": mb}), Awaited::Reply);
    }

    /// Move `document`'s camera on the next frame (zoom and pan control from the host).
    pub fn set_camera(&mut self, document: DocumentId, change: CameraChange) {
        self.queue.push(Request::Camera { document, change });
    }

    /// `document`'s camera as of the last frame.
    pub fn camera(&self, document: DocumentId) -> Option<Camera> {
        self.cameras.get(&document).copied()
    }

    /// **Unstable.** Run any control-protocol UI method (`ui.set`, `ui.inspect`, `document.memory`,
    /// `editor.setLayout`…; `docs/control-protocol.md`) on the next frame. Returns the token its
    /// [`ViewEvent::Reply`] carries.
    pub fn control(&mut self, method: &str, params: Value) -> u64 {
        self.queue.control(method, params, Awaited::Reply)
    }

    /// The widget for this frame in all the space `ui` has left. See [`Self::ui_in_rect`].
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &mut Host) -> ViewOutput {
        let rect = ui.available_rect_before_wrap();
        self.ui_in_rect(ui, rect, host)
    }

    /// The widget for this frame in `rect`: the editor's logic (views for new documents, jobs,
    /// document switches, the shortcuts the host lets through), the active document's canvas,
    /// the editor's dialogs; then the host's events. The one call a host makes per frame.
    pub fn ui_in_rect(&mut self, ui: &mut egui::Ui, rect: Rect, host: &mut Host) -> ViewOutput {
        self.attach(host);
        let ctx = ui.ctx().clone();
        let tokens = match &self.options.theme {
            Some(t) => t.tokens(ui.visuals()),
            None => embed::tokens_for_visuals(ui.visuals()),
        };
        photocraft_ui_egui::theme::publish_tokens(&ctx, tokens);
        let host_tool = host.tool().to_string();
        // Lend the host's session to the editor for this frame.
        std::mem::swap(host.session_mut(), &mut self.app.session);
        let lent = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.lent_frame(ui, rect, &host_tool)));
        std::mem::swap(host.session_mut(), &mut self.app.session);
        let (frame, mut view_events, tool) = match lent {
            Ok(r) => r,
            Err(payload) => {
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "internal error".into());
                (Lent::default(), vec![ViewEvent::Failed { message: format!("the editor failed this frame: {message}") }], None)
            }
        };
        // The person picked another tool on the canvas: the host learns it (`toolChanged`).
        if let Some(t) = tool
            && let Err(e) = host.set_tool(&t)
        {
            view_events.push(ViewEvent::Failed { message: e.to_string() });
        }
        let events = host.poll_from_view(frame.jobs);
        let response = ui.interact(rect, ui.scope_id().with("capishot-view"), Sense::hover());
        let hover = frame.transform.and_then(|t| response.hover_pos().and_then(|p| t.doc_at(p)));
        let overlay = frame.transform.map(|t| Overlay::new(ui.painter_at(t.canvas_rect()), t));
        ViewOutput {
            events,
            view_events,
            response,
            document: frame.document,
            transform: frame.transform,
            hover,
            took_keys: frame.took_keys,
            overlay,
        }
    }

    /// The frame while the editor holds the session. Returns what it did, the view's events and
    /// the tool the person picked (to tell the host once the session is back).
    fn lent_frame(&mut self, ui: &mut egui::Ui, rect: Rect, host_tool: &str) -> (Lent, Vec<ViewEvent>, Option<String>) {
        let ctx = ui.ctx().clone();
        let mut events = Vec::new();
        if let Some(rulers) = self.options.rulers {
            self.app.ui.extras.rulers = rulers;
        }
        // The host's tool, when the host changed it since the last frame.
        if self.synced_tool.as_deref() != Some(host_tool) {
            self.synced_tool = Some(host_tool.to_string());
            match photocraft_ui_egui::Tool::from_name(host_tool) {
                Some(t) if t == embed::tool(&self.app) => {}
                Some(_) => {
                    let reply = embed::control(&mut self.app, &ctx, "ui.set", json!({"tool": host_tool}));
                    if let Some(Err(e)) = reply.try_recv().ok().map(reply_result) {
                        events.push(ViewEvent::Failed { message: format!("tool `{host_tool}`: {e}") });
                    }
                }
                None => events.push(ViewEvent::Failed { message: format!("unknown tool `{host_tool}`") }),
            }
        }
        self.run_requests(&ctx, &mut events);
        let opts = EmbedOptions { keys: self.options.keys.clone(), dialogs: self.options.dialogs };
        let f = embed::frame(&mut self.app, ui, rect, &opts);
        // Replies that arrived (a document switch replies in the frame it swaps).
        self.pending.retain(|(token, awaited, rx)| match rx.try_recv() {
            Ok(v) => {
                events.push(view_event(*token, *awaited, reply_result(v)));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                events.push(view_event(*token, *awaited, Err("the editor dropped the request".into())));
                false
            }
        });
        // Cameras, for `camera()` and `CameraChanged`.
        let active = self.app.session.active_index();
        let ids: Vec<DocumentId> = self.app.session.documents().iter().map(|st| st.doc.id.into()).collect();
        self.cameras.retain(|d, _| ids.contains(d));
        for (i, id) in ids.iter().enumerate() {
            if let Some(c) = embed::camera(&self.app, i) {
                let moved = self.cameras.insert(*id, c) != Some(c);
                if moved && Some(i) == active {
                    events.push(ViewEvent::CameraChanged { document: *id, camera: c });
                }
            }
        }
        let document = active.and_then(|i| ids.get(i).copied());
        self.last_active = document.and_then(|d| self.cameras.get(&d).map(|c| (d, *c)));
        let size = active.and_then(|i| self.app.session.documents().get(i)).map(|st| [st.doc.size.width, st.doc.size.height]);
        let transform = match (f.xform, document, size) {
            (Some(xf), Some(d), Some(size)) => Some(CanvasTransform::new(xf, d, size)),
            _ => None,
        };
        // The person's tool, when it changed on the canvas.
        let now = embed::tool(&self.app).id();
        let tool = (self.synced_tool.as_deref() != Some(now.as_str())).then(|| {
            self.synced_tool = Some(now.clone());
            now
        });
        (Lent { jobs: f.jobs, document, transform, took_keys: f.took_keys }, events, tool)
    }

    /// The queued requests, against the lent session.
    fn run_requests(&mut self, ctx: &egui::Context, events: &mut Vec<ViewEvent>) {
        for r in self.queue.take() {
            match r {
                Request::Control { token, method, params, awaited } => {
                    let rx = embed::control(&mut self.app, ctx, &method, params);
                    self.pending.push((token, awaited, rx));
                }
                Request::Camera { document, change } => {
                    let r = match self.app.session.index_of_document(document.into()) {
                        Some(i) => embed::set_camera(&mut self.app, i, change),
                        None => Err(format!("no open document {}", document.0)),
                    };
                    if let Err(e) = r {
                        events.push(ViewEvent::Failed { message: format!("camera: {e}") });
                    }
                }
                Request::Shown { document, camera } => {
                    let change = match camera {
                        DocumentCamera::Own => None,
                        DocumentCamera::Keep => self
                            .last_active
                            .filter(|(d, _)| *d != document)
                            .map(|(_, c)| CameraChange::Set { zoom: Some(c.zoom), center: Some(c.center), rotation: Some(c.rotation) }),
                        DocumentCamera::At { zoom, center } => Some(CameraChange::Set { zoom: Some(zoom), center: Some(center), rotation: None }),
                    };
                    if let Some(change) = change
                        && let Some(i) = self.app.session.index_of_document(document.into())
                        && let Err(e) = embed::set_camera(&mut self.app, i, change)
                    {
                        events.push(ViewEvent::Failed { message: format!("camera: {e}") });
                    }
                }
            }
        }
    }
}

/// What the lent frame did.
#[derive(Default)]
struct Lent {
    jobs: Vec<photocraft_engine::jobs::JobEvent>,
    document: Option<DocumentId>,
    transform: Option<CanvasTransform>,
    took_keys: bool,
}

/// `editor.showDocument` / `document.prewarm`'s `viewport` for a camera (`None`: its own).
fn viewport(camera: DocumentCamera) -> Option<Value> {
    match camera {
        DocumentCamera::Own => None,
        DocumentCamera::Keep => Some(json!("keep")),
        DocumentCamera::At { zoom, center } => Some(json!({"zoom": zoom, "x": center[0], "y": center[1]})),
    }
}

/// A control reply (`{ok, result}` / `{ok: false, error}`) as a result.
fn reply_result(v: Value) -> std::result::Result<Value, String> {
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(v.get("error").map_or_else(|| "failed".to_string(), |e| e.as_str().map_or_else(|| e.to_string(), str::to_string)))
    }
}

fn view_event(token: u64, awaited: Awaited, result: std::result::Result<Value, String>) -> ViewEvent {
    match awaited {
        Awaited::Show(document) => ViewEvent::Shown { document, result },
        Awaited::Prewarm(document) => ViewEvent::Prewarmed { document, result },
        Awaited::Reply => ViewEvent::Reply { token, result },
    }
}

/// A [`Host`] and its [`CapishotView`]: the one object a host keeps for an editor area. Every
/// [`Host`] call goes through [`Self::host`]; [`Self::ui`] is the per-frame call.
#[derive(Debug)]
pub struct CapishotEditor {
    /// The engine: documents, commands, history, jobs, events, live sync.
    pub host: Host,
    /// The canvas widget.
    pub view: CapishotView,
}

impl CapishotEditor {
    /// An editor with no documents, drawing on the host's wgpu device (see [`CapishotView::new`]).
    pub fn new(render_state: Option<RenderState>) -> Self {
        let mut host = Host::new();
        let mut view = CapishotView::new(render_state);
        view.attach(&mut host);
        CapishotEditor { host, view }
    }

    /// The editor for this frame in all the space `ui` has left ([`CapishotView::ui`]).
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ViewOutput {
        self.view.ui(ui, &mut self.host)
    }

    /// The editor for this frame in `rect` ([`CapishotView::ui_in_rect`]).
    pub fn ui_in_rect(&mut self, ui: &mut egui::Ui, rect: Rect) -> ViewOutput {
        self.view.ui_in_rect(ui, rect, &mut self.host)
    }
}

#[cfg(test)]
mod tests;
