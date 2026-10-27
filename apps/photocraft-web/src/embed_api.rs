//! The embedding API v1 for host apps (Capistudio): the module's own exports, for a page that
//! loads the web build as an ES module and runs Capishot on a canvas of its own. Contract, types
//! and versioning rules: `embed/README.md` and `embed/capishot-embed.d.ts`.
//!
//! ```js
//! import init, * as capishot from "./photocraft-web-<hash>.js";
//! await init({ module_or_path: "./photocraft-web-<hash>_bg.wasm" });   // runs nothing yet
//! capishot.embedApiVersion();                                          // 1
//! await capishot.init({ container, mode: "embedded", locale: "es" });
//! const doc = await capishot.openDocument(bytes, "page.psd");
//! await capishot.setTool("brush", { size: 20, color: "#ff3366" });
//! const psd = await capishot.saveDocument(doc.id, "psd");               // Uint8Array
//! ```
//!
//! Every call that needs the app is queued and runs at the start of the next frame (the app
//! lives inside eframe's runner), so they all return promises. Control-channel calls (`execute`,
//! `request`, `setTool`, viewport, undo) go through the same channel as the desktop control
//! server and `window.photocraft.request`, with the same results and errors.
//! (`crate::embed`, upstream's `window.photocraft` API, stays for the app's own page; a hosted
//! app doesn't install it, so the host's page keeps its globals.)

use std::cell::{Cell, RefCell};
use std::sync::mpsc::{Receiver, Sender};

use photocraft_ui_egui::{ControlRequest, ControlResponse, ExportSettings, PhotocraftApp};
use serde_json::{Value, json};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::embed_api_core::{self as core, Chrome, DocSnap, Mode, Snapshot};

/// How a host starts the app (from `init`'s options).
pub struct Hosted {
    pub force_cpu: bool,
    pub force_webgl: bool,
}

type Settle = (js_sys::Function, js_sys::Function);
type Job = Box<dyn FnOnce(&mut PhotocraftApp, &egui::Context) -> Result<JsValue, String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Starting,
    Running,
}

struct Host {
    phase: Phase,
    mode: Mode,
    /// Options applied on the first frame.
    locale: Option<String>,
    theme: Option<String>,
    first_frame: bool,
    /// The full-mode chrome kept while embedded.
    saved_chrome: Option<Chrome>,
    last: Option<Snapshot>,
    /// Selection bounds by document, with the revision they were measured at.
    selections: Vec<(u64, u64, Option<[i32; 4]>)>,
    ready: Vec<Settle>,
    runner: Option<eframe::WebRunner>,
    /// A canvas `init` made inside the host's container (removed by `destroy`).
    own_canvas: Option<web_sys::HtmlCanvasElement>,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            mode: Mode::Embedded,
            locale: None,
            theme: None,
            first_frame: true,
            saved_chrome: None,
            last: None,
            selections: Vec::new(),
            ready: Vec::new(),
            runner: None,
            own_canvas: None,
        }
    }
}

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host::default());
    static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    static CONTROL: RefCell<Option<Sender<ControlRequest>>> = const { RefCell::new(None) };
    static JOBS: RefCell<Vec<(Job, Settle)>> = RefCell::new(Vec::new());
    static WAITING: RefCell<Vec<(Receiver<ControlResponse>, Settle)>> = const { RefCell::new(Vec::new()) };
    static LISTENER: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
    static LOGGER: Cell<bool> = const { Cell::new(false) };
    /// The view last reported, the latest one not reported yet, and the animation-frame
    /// callback that reports it (`viewChanged` at most once per frame).
    static VIEW: RefCell<(Option<core::ViewSnap>, Option<core::ViewSnap>)> = const { RefCell::new((None, None)) };
    static VIEW_FRAME: RefCell<Option<wasm_bindgen::closure::Closure<dyn FnMut()>>> = const { RefCell::new(None) };
    static VIEW_FRAME_ASKED: Cell<bool> = const { Cell::new(false) };
}

fn to_js(v: &Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

fn from_js(v: &JsValue) -> Value {
    if v.is_undefined() || v.is_null() {
        return Value::Null;
    }
    if let Some(s) = v.as_string() {
        // A JSON string: `execute("x.y", '{"a":1}')`.
        return serde_json::from_str(&s).unwrap_or(Value::String(s));
    }
    js_sys::JSON::stringify(v).ok().and_then(|s| s.as_string()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

fn error(msg: &str) -> JsValue {
    js_sys::Error::new(msg).into()
}

fn js_message(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>().map_or_else(|| e.as_string().unwrap_or_else(|| format!("{e:?}")), |e| String::from(e.message()))
}

fn repaint() {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.request_repaint();
        }
    });
}

fn emit(event: &Value) {
    let Some(listener) = LISTENER.with(|l| l.borrow().clone()) else { return };
    if let Err(e) = listener.call1(&JsValue::NULL, &to_js(event)) {
        log::warn!("embed: the host's event listener threw: {}", js_message(&e));
    }
}

fn promise(run: impl FnOnce(Settle)) -> js_sys::Promise {
    let mut run = Some(run);
    js_sys::Promise::new(&mut |resolve, reject| {
        if let Some(run) = run.take() {
            run((resolve, reject));
        }
    })
}

/// Why the app can't take calls right now, if it can't.
fn unavailable() -> Option<String> {
    HOST.with(|h| {
        let h = h.borrow();
        if h.phase == Phase::Idle {
            return Some("Capishot isn't running: call init() first".into());
        }
        let panicked = h.runner.as_ref().filter(|r| r.has_panicked());
        panicked.map(|r| format!("Capishot crashed: {}", r.panic_summary().map(|s| s.message()).unwrap_or_default()))
    })
}

/// Runs `job` with the app at the start of the next frame.
fn queue(job: impl FnOnce(&mut PhotocraftApp, &egui::Context) -> Result<JsValue, String> + 'static) -> js_sys::Promise {
    if let Some(why) = unavailable() {
        return js_sys::Promise::reject(&error(&why));
    }
    let p = promise(|settle| JOBS.with(|j| j.borrow_mut().push((Box::new(job), settle))));
    repaint();
    p
}

/// A control-channel request (the desktop control protocol's methods).
fn control(method: &str, params: Value) -> js_sys::Promise {
    if let Some(why) = unavailable() {
        return js_sys::Promise::reject(&error(&why));
    }
    let params = if params.is_null() { json!({}) } else { params };
    let (req, rx) = ControlRequest::new(method, params);
    let sent = CONTROL.with(|c| c.borrow().as_ref().is_some_and(|tx| tx.send(req).is_ok()));
    if !sent {
        return js_sys::Promise::reject(&error("Capishot isn't running"));
    }
    repaint();
    promise(|settle| WAITING.with(|w| w.borrow_mut().push((rx, settle))))
}

/// The control channel for a hosted app (`web::run` hands it to the app).
pub fn install(ctx: egui::Context) -> Receiver<ControlRequest> {
    let (tx, rx) = std::sync::mpsc::channel();
    CONTROL.with(|c| *c.borrow_mut() = Some(tx));
    CTX.with(|c| *c.borrow_mut() = Some(ctx));
    rx
}

/// Every frame of a hosted app, before its UI: first-frame setup, queued calls, control replies
/// and the mode. The events go out after the UI ([`report`]).
pub fn process(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let first = HOST.with(|h| std::mem::replace(&mut h.borrow_mut().first_frame, false));
    if first {
        first_frame(app, ctx);
    }
    for (job, (resolve, reject)) in JOBS.with(|j| std::mem::take(&mut *j.borrow_mut())) {
        match job(app, ctx) {
            Ok(v) => resolve.call1(&JsValue::NULL, &v).ok(),
            Err(e) => reject.call1(&JsValue::NULL, &error(&e)).ok(),
        };
    }
    WAITING.with(|w| {
        w.borrow_mut().retain(|(rx, (resolve, reject))| match rx.try_recv() {
            Ok(v) => {
                if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    resolve.call1(&JsValue::NULL, &to_js(v.get("result").unwrap_or(&Value::Null))).ok();
                } else {
                    let msg = v.get("error").and_then(Value::as_str).unwrap_or("error");
                    reject.call1(&JsValue::NULL, &error(msg)).ok();
                }
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                reject.call1(&JsValue::NULL, &error("request dropped")).ok();
                false
            }
        });
    });
    if HOST.with(|h| h.borrow().mode) == Mode::Embedded {
        core::hide_chrome(&mut app.ui.panels, &mut app.ui.view);
    }
    if first {
        let ready = HOST.with(|h| {
            let mut h = h.borrow_mut();
            h.phase = Phase::Running;
            std::mem::take(&mut h.ready)
        });
        let info = to_js(&core::version());
        for (resolve, _) in ready {
            resolve.call1(&JsValue::NULL, &info).ok();
        }
        emit(&json!({ "type": "ready", "version": core::version() }));
    }
}

/// The events of this frame, at most one of each kind: called once per frame, after the frame's
/// UI (where pans, zooms and rotations happen) and the calls [`process`] ran before it.
pub fn report(app: &PhotocraftApp) {
    let snap = snapshot(app);
    let events = HOST.with(|h| {
        let mut h = h.borrow_mut();
        if h.last.as_ref() == Some(&snap) {
            return Vec::new();
        }
        let events = core::events(h.last.as_ref(), &snap);
        h.last = Some(snap);
        events
    });
    for e in events {
        emit(&e);
    }
    let Some(view) = HOST.with(|h| h.borrow().last.as_ref().and_then(|s| s.view.clone())) else { return };
    let changed = VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let changed = v.0.as_ref() != Some(&view);
        v.1 = changed.then_some(view);
        changed
    });
    if changed && !VIEW_FRAME_ASKED.with(|a| a.replace(true)) {
        ask_view_frame();
    }
}

/// Reports the latest view on the next animation frame: a gesture that runs several app frames
/// in one browser frame still sends one `viewChanged`.
fn ask_view_frame() {
    let Some(window) = web_sys::window() else { return };
    VIEW_FRAME.with(|f| {
        let mut f = f.borrow_mut();
        let cb = f.get_or_insert_with(|| {
            wasm_bindgen::closure::Closure::new(|| {
                VIEW_FRAME_ASKED.with(|a| a.set(false));
                let events = VIEW.with(|v| {
                    let mut v = v.borrow_mut();
                    let Some(next) = v.1.take() else { return Vec::new() };
                    let events = core::view_events(v.0.as_ref(), &next);
                    v.0 = Some(next);
                    events
                });
                for e in events {
                    emit(&e);
                }
            })
        });
        if window.request_animation_frame(cb.as_ref().unchecked_ref()).is_err() {
            VIEW_FRAME_ASKED.with(|a| a.set(false));
        }
    });
}

fn first_frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let (locale, theme, mode) = HOST.with(|h| {
        let h = h.borrow();
        (h.locale.clone(), h.theme.clone(), h.mode)
    });
    if let Some(locale) = locale
        && let Err(e) = app.run("prefs.set", json!({ "path": "interface.language", "value": locale }))
    {
        log::warn!("embed: locale `{locale}`: {e}");
    }
    if let Some(kind) = theme.as_deref().and_then(photocraft_ui_egui::theme::ThemeKind::from_name) {
        app.set_theme(ctx, kind);
    }
    apply_mode(app, mode);
}

fn apply_mode(app: &mut PhotocraftApp, mode: Mode) {
    HOST.with(|h| {
        let mut h = h.borrow_mut();
        match mode {
            Mode::Embedded => {
                // Keep the full chrome as it is now, unless it's already the embedded one.
                if h.mode == Mode::Full || h.saved_chrome.is_none() {
                    h.saved_chrome = Some(Chrome::of(&app.ui.panels, &app.ui.view));
                }
                core::hide_chrome(&mut app.ui.panels, &mut app.ui.view);
            }
            Mode::Full => core::show_chrome(h.saved_chrome.as_ref(), &mut app.ui.panels, &mut app.ui.view),
        }
        h.mode = mode;
    });
}

fn snapshot(app: &PhotocraftApp) -> Snapshot {
    let docs = app.session.documents();
    let active = app.session.active().map(|st| st.doc.id.0);
    let docs = HOST.with(|h| {
        let mut h = h.borrow_mut();
        let mut cache = std::mem::take(&mut h.selections);
        let snaps = docs
            .iter()
            .map(|st| {
                let id = st.doc.id.0;
                // Measuring a selection walks its tiles: once per revision, not per frame.
                let selection = match cache.iter().find(|(d, rev, _)| *d == id && *rev == st.revision) {
                    Some((_, _, b)) => *b,
                    None => {
                        let b = st.doc.selection.as_ref().map(|s| s.content_bounds()).filter(|r| !r.is_empty()).map(|r| [r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0]);
                        cache.retain(|(d, ..)| *d != id);
                        cache.push((id, st.revision, b));
                        b
                    }
                };
                DocSnap { id, revision: st.revision, dirty: st.revision != st.saved_revision, selection }
            })
            .collect::<Vec<_>>();
        cache.retain(|(d, ..)| snaps.iter().any(|s| s.id == *d));
        h.selections = cache;
        snaps
    });
    let view = app.session.active_index().zip(active).and_then(|(i, doc)| {
        let v = app.ui.views.get(i).filter(|v| !v.fit_pending && !v.fill_pending)?;
        Some(core::ViewSnap { doc, zoom: v.zoom, center: v.center, rotation: v.rotation })
    });
    Snapshot { active, docs, tool: core::tool_id(app.ui.tool), error: app.ui.status_error.then(|| app.ui.status.clone()), view }
}

fn index_of(app: &PhotocraftApp, id: Option<u64>) -> Result<usize, String> {
    match id {
        Some(id) => app.session.documents().iter().position(|st| st.doc.id.0 == id).ok_or_else(|| format!("no document {id}")),
        None => app.session.active_index().ok_or_else(|| "no document is open".into()),
    }
}

fn active_info(app: &PhotocraftApp) -> Result<Value, String> {
    app.session.active().map(|st| core::doc_info(st, true)).ok_or_else(|| "no document is open".into())
}

fn documents(app: &PhotocraftApp) -> Value {
    let active = app.session.active_index();
    Value::Array(app.session.documents().iter().enumerate().map(|(i, st)| core::doc_info(st, Some(i) == active)).collect())
}

fn doc_id(v: &JsValue) -> Result<Option<u64>, String> {
    if v.is_undefined() || v.is_null() {
        return Ok(None);
    }
    v.as_f64().filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0).map(|f| Some(f as u64)).ok_or_else(|| "a document id is a number".into())
}

/// The bytes of an ArrayBuffer, a typed array or a Blob.
async fn bytes_of(data: JsValue) -> Result<Vec<u8>, String> {
    if let Some(blob) = data.dyn_ref::<web_sys::Blob>() {
        let buf = JsFuture::from(blob.array_buffer()).await.map_err(|e| js_message(&e))?;
        return Ok(js_sys::Uint8Array::new(&buf).to_vec());
    }
    if data.is_instance_of::<js_sys::ArrayBuffer>() || js_sys::ArrayBuffer::is_view(&data) {
        return Ok(js_sys::Uint8Array::new(&data).to_vec());
    }
    Err("openDocument takes an ArrayBuffer, a typed array or a Blob".into())
}

fn opt_str(o: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(o, &key.into()).ok().and_then(|v| v.as_string())
}

fn opt_bool(o: &JsValue, key: &str) -> bool {
    js_sys::Reflect::get(o, &key.into()).ok().and_then(|v| v.as_bool()).unwrap_or(false)
}

// ---------- the exports ----------

/// The embedding API version (sync; check it before anything else).
#[wasm_bindgen(js_name = embedApiVersion)]
pub fn embed_api_version() -> u32 {
    core::EMBED_API_VERSION
}

/// `{embedApi, capishotCommit, buildHash, app, format, features}` (sync).
#[wasm_bindgen(js_name = version)]
pub fn version() -> JsValue {
    to_js(&core::version())
}

/// Starts Capishot. Options: `canvas` (an HTMLCanvasElement) or `container` (an element a
/// canvas filling it is made in), `mode` (`embedded`, the default, or `full`), `locale` (`es`,
/// `en`, `pt-br`…), `theme` (`pro`, `proMedium`, `studio`, `studioLight`, `classic`), `webgl`
/// and `cpu` (force those renderers). Resolves with `version()` once the first frame ran.
#[wasm_bindgen(js_name = init)]
pub fn init(options: JsValue) -> js_sys::Promise {
    if !LOGGER.with(|l| l.replace(true)) {
        eframe::WebLogger::init(log::LevelFilter::Info).ok();
        photocraft_ui_egui::gpu_canvas::set_clock(crate::frame_perf::now_ms);
    }
    if HOST.with(|h| h.borrow().phase) != Phase::Idle {
        return js_sys::Promise::reject(&error("Capishot is already running: call destroy() first"));
    }
    let mode = match opt_str(&options, "mode").as_deref().map(Mode::parse).transpose() {
        Ok(m) => m.unwrap_or_default(),
        Err(e) => return js_sys::Promise::reject(&error(&e)),
    };
    let canvas = js_sys::Reflect::get(&options, &"canvas".into()).ok().and_then(|c| c.dyn_into::<web_sys::HtmlCanvasElement>().ok());
    let (canvas, own) = match canvas {
        Some(c) => (c, false),
        None => match make_canvas(&options) {
            Ok(c) => (c, true),
            Err(e) => return js_sys::Promise::reject(&error(&e)),
        },
    };
    let hosted = Hosted { force_cpu: opt_bool(&options, "cpu"), force_webgl: opt_bool(&options, "webgl") };
    let p = promise(|settle| {
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            *h = Host { phase: Phase::Starting, mode, locale: opt_str(&options, "locale"), theme: opt_str(&options, "theme"), ..Host::default() };
            h.own_canvas = own.then(|| canvas.clone());
            h.ready.push(settle);
        });
    });
    wasm_bindgen_futures::spawn_local(async move {
        match crate::web::run(canvas, Some(hosted)).await {
            Ok(runner) => HOST.with(|h| h.borrow_mut().runner = Some(runner)),
            Err(e) => {
                let msg = format!("Capishot failed to start: {e}. A browser with WebGPU or WebGL2 is required.");
                let ready = HOST.with(|h| {
                    let mut h = h.borrow_mut();
                    if let Some(c) = h.own_canvas.take() {
                        c.remove();
                    }
                    h.phase = Phase::Idle;
                    std::mem::take(&mut h.ready)
                });
                for (_, reject) in ready {
                    reject.call1(&JsValue::NULL, &error(&msg)).ok();
                }
                emit(&json!({ "type": "error", "message": msg, "source": "init", "fatal": true }));
            }
        }
    });
    p
}

fn make_canvas(options: &JsValue) -> Result<web_sys::HtmlCanvasElement, String> {
    let container = js_sys::Reflect::get(options, &"container".into())
        .ok()
        .and_then(|c| c.dyn_into::<web_sys::HtmlElement>().ok())
        .ok_or("init needs a `canvas` or a `container` element")?;
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let canvas: web_sys::HtmlCanvasElement = document.create_element("canvas").map_err(|e| js_message(&e))?.dyn_into().map_err(|_| "not a canvas")?;
    canvas.style().set_css_text("display:block;width:100%;height:100%;outline:none;touch-action:none");
    container.append_child(&canvas).map_err(|e| js_message(&e))?;
    Ok(canvas)
}

/// Stops Capishot and frees its canvas listeners (a canvas `init` made is removed). Pending
/// calls are rejected. `init` can be called again afterwards.
#[wasm_bindgen(js_name = destroy)]
pub fn destroy() {
    let host = HOST.with(|h| std::mem::take(&mut *h.borrow_mut()));
    if let Some(runner) = host.runner {
        runner.destroy();
    }
    if let Some(c) = host.own_canvas {
        c.remove();
    }
    let gone = || error("Capishot was destroyed");
    for (_, (_, reject)) in JOBS.with(|j| std::mem::take(&mut *j.borrow_mut())) {
        reject.call1(&JsValue::NULL, &gone()).ok();
    }
    for (_, (_, reject)) in WAITING.with(|w| std::mem::take(&mut *w.borrow_mut())) {
        reject.call1(&JsValue::NULL, &gone()).ok();
    }
    for (_, reject) in host.ready {
        reject.call1(&JsValue::NULL, &gone()).ok();
    }
    CONTROL.with(|c| *c.borrow_mut() = None);
    CTX.with(|c| *c.borrow_mut() = None);
    VIEW.with(|v| *v.borrow_mut() = (None, None));
}

/// Sets the one event listener (`null` removes it). Events: `ready`, `documents`,
/// `documentChanged`, `dirty`, `selectionChanged`, `toolChanged`, `modeChanged`, `zoom`,
/// `viewChanged`, `error`.
#[wasm_bindgen(js_name = onEvent)]
pub fn on_event(listener: JsValue) {
    let f = listener.dyn_into::<js_sys::Function>().ok();
    LISTENER.with(|l| *l.borrow_mut() = f);
}

/// Opens a PSD, PSB, `.pcraft`, PNG, JPEG, WebP… (a flat image becomes a one-layer document) as
/// the active document → its `DocumentInfo` plus `warnings`.
#[wasm_bindgen(js_name = openDocument)]
pub fn open_document(data: JsValue, filename: Option<String>) -> js_sys::Promise {
    let name = filename.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "document".into());
    wasm_bindgen_futures::future_to_promise(async move {
        let bytes = bytes_of(data).await.map_err(|e| error(&e))?;
        JsFuture::from(queue(move |app, _| {
            let before: Vec<u64> = app.session.documents().iter().map(|st| st.doc.id.0).collect();
            let warnings = app.open_bytes(&name, &bytes).inspect_err(|e| app.open_failed(&name, e))?;
            app.sync_views();
            let opened = app.session.documents().iter().find(|st| !before.contains(&st.doc.id.0)).map(|st| st.doc.id.0);
            let Some(id) = opened else {
                return Err(format!("{name} didn't open as a document (brush, gradient and swatch files go to the preset libraries)"));
            };
            let index = index_of(app, Some(id))?;
            let st = app.session.documents().get(index).ok_or("the document closed")?;
            let mut info = core::doc_info(st, app.session.active_index() == Some(index));
            info["warnings"] = json!(warnings);
            Ok(to_js(&info))
        }))
        .await
    })
}

/// Creates a document: `{width, height, name?, background?, resolution?, mode?, depth?}`.
#[wasm_bindgen(js_name = newDocument)]
pub fn new_document(options: JsValue) -> js_sys::Promise {
    let params = match core::new_document_params(&from_js(&options)) {
        Ok(p) => p,
        Err(e) => return js_sys::Promise::reject(&error(&e)),
    };
    queue(move |app, _| {
        app.run("file.new", params)?;
        app.sync_views();
        active_info(app).map(|v| to_js(&v))
    })
}

/// The document `id` (`null`: the active one) as `psd` (layered), `pcraft`, `png`, `jpg`, `webp`
/// or `tiff` bytes (a Uint8Array). A layered save marks the document saved (not dirty).
#[wasm_bindgen(js_name = saveDocument)]
pub fn save_document(id: JsValue, format: String) -> js_sys::Promise {
    let (id, format) = match (doc_id(&id), core::save_format(&format)) {
        (Ok(id), Ok(f)) => (id, f),
        (Err(e), _) | (_, Err(e)) => return js_sys::Promise::reject(&error(&e)),
    };
    queue(move |app, _| {
        let index = index_of(app, id)?;
        let st = app.session.documents().get(index).ok_or("no such document")?;
        let name = crate::embed_protocol::export_name(&st.doc.name, &format)?;
        let export = app.services.export.as_ref().ok_or("no exporter")?;
        let (bytes, warnings) = export(&st.doc, &name, &ExportSettings::default())?;
        for w in warnings {
            log::info!("embed: save {name}: {w}");
        }
        if photocraft_engine::file_cmds::saves_in_place(&name) {
            // The session hands out the active document mutably only.
            let shown = app.session.active_index();
            if shown != Some(index) {
                app.session.set_active(index);
            }
            if let Some(st) = app.session.active_mut() {
                st.saved_revision = st.revision;
            }
            if let Some(shown) = shown.filter(|s| *s != index) {
                app.session.set_active(shown);
            }
        }
        Ok(js_sys::Uint8Array::from(bytes.as_slice()).into())
    })
}

/// Closes document `id`. Unsaved changes refuse unless `force`.
#[wasm_bindgen(js_name = closeDocument)]
pub fn close_document(id: JsValue, force: Option<bool>) -> js_sys::Promise {
    let id = match doc_id(&id) {
        Ok(id) => id,
        Err(e) => return js_sys::Promise::reject(&error(&e)),
    };
    queue(move |app, _| {
        let index = index_of(app, id)?;
        let dirty = app.session.documents().get(index).is_some_and(|st| st.revision != st.saved_revision);
        if dirty && force != Some(true) {
            return Err("the document has unsaved changes (pass force to close it anyway)".into());
        }
        app.session.close(index).ok_or("no such document")?;
        app.sync_views();
        Ok(JsValue::UNDEFINED)
    })
}

/// Makes document `id` the active (shown) one.
#[wasm_bindgen(js_name = activateDocument)]
pub fn activate_document(id: JsValue) -> js_sys::Promise {
    let id = match doc_id(&id) {
        Ok(Some(id)) => id,
        Ok(None) => return js_sys::Promise::reject(&error("activateDocument needs a document id")),
        Err(e) => return js_sys::Promise::reject(&error(&e)),
    };
    queue(move |app, _| {
        let index = index_of(app, Some(id))?;
        app.session.set_active(index);
        app.sync_views();
        active_info(app).map(|v| to_js(&v))
    })
}

/// The open documents (`DocumentInfo[]`).
#[wasm_bindgen(js_name = listDocuments)]
pub fn list_documents() -> js_sys::Promise {
    queue(|app, _| Ok(to_js(&documents(app))))
}

/// `{mode, tool, active, documents}`.
#[wasm_bindgen(js_name = getState)]
pub fn get_state() -> js_sys::Promise {
    queue(|app, _| {
        let mode = HOST.with(|h| h.borrow().mode);
        Ok(to_js(&json!({
            "mode": mode.id(),
            "tool": core::tool_id(app.ui.tool),
            "active": app.session.active().map(|st| st.doc.id.0),
            "documents": documents(app),
        })))
    })
}

/// Runs a command by id (engine commands and the shell's View/Window commands) with `params`
/// (an object or a JSON string) → its JSON result. Same as the control protocol's `engine.execute`.
#[wasm_bindgen(js_name = execute)]
pub fn execute(command: String, params: JsValue) -> js_sys::Promise {
    let params = from_js(&params);
    let params = if params.is_null() { json!({}) } else { params };
    if !params.is_object() {
        return js_sys::Promise::reject(&error(&format!("{command}: params must be an object")));
    }
    control("engine.execute", json!({ "command": command, "params": params }))
}

/// Any control-protocol method (`ui.inspect`, `ui.set`, `ui.pointer`…): the escape hatch for what
/// the typed calls don't cover yet.
#[wasm_bindgen(js_name = request)]
pub fn request(method: String, params: JsValue) -> js_sys::Promise {
    control(&method, from_js(&params))
}

/// Every command: `{id, label, menu, shortcut, params, enabled, kind: "engine" | "ui"}`.
#[wasm_bindgen(js_name = listCommands)]
pub fn list_commands() -> js_sys::Promise {
    queue(|app, _| {
        let mut list = match app.run("command.list", json!({}))? {
            Value::Array(a) => a,
            _ => Vec::new(),
        };
        for c in &mut list {
            c["kind"] = json!("engine");
        }
        for &(id, label, menu, shortcut) in photocraft_ui_egui::menus::UI_COMMANDS {
            list.push(json!({ "id": id, "label": label, "menu": menu, "shortcut": shortcut, "params": "{}", "enabled": true, "kind": "ui" }));
        }
        Ok(to_js(&Value::Array(list)))
    })
}

/// Every tool id (`brush`, `eraser`, `cloneStamp`…), sync.
#[wasm_bindgen(js_name = listTools)]
pub fn list_tools() -> JsValue {
    to_js(&json!(core::tools()))
}

/// `embedded` (only the canvas) or `full` (the whole Capishot UI), live on the same documents.
#[wasm_bindgen(js_name = setMode)]
pub fn set_mode(mode: String) -> js_sys::Promise {
    let mode = match Mode::parse(&mode) {
        Ok(m) => m,
        Err(e) => return js_sys::Promise::reject(&error(&e)),
    };
    queue(move |app, _| {
        let before = HOST.with(|h| h.borrow().mode);
        apply_mode(app, mode);
        if before != mode {
            emit(&json!({ "type": "modeChanged", "mode": mode.id() }));
        }
        Ok(JsValue::from_str(mode.id()))
    })
}

/// Picks a tool, with optional `{size, color, opacity, flow, hardness, brush}`.
#[wasm_bindgen(js_name = setTool)]
pub fn set_tool(tool: String, options: JsValue) -> js_sys::Promise {
    let steps = match core::set_tool_requests(&tool, &from_js(&options)) {
        Ok(s) => s,
        Err(e) => return js_sys::Promise::reject(&error(&e)),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        for (method, params) in steps {
            JsFuture::from(control(method, params)).await?;
        }
        Ok(JsValue::from_str(&core::tool_id(core::parse_tool(&tool).map_err(|e| error(&e))?)))
    })
}

/// Fits the active document in the view.
#[wasm_bindgen(js_name = fit)]
pub fn fit() -> js_sys::Promise {
    control("ui.set", json!({ "fit": true }))
}

/// Zooms to a factor (1 = 100%) or `"in"`, `"out"`, `"fit"`, `"actual"`.
#[wasm_bindgen(js_name = zoom)]
pub fn zoom(level: JsValue) -> js_sys::Promise {
    match core::zoom_request(&from_js(&level)) {
        Ok((method, params)) => control(method, params),
        Err(e) => js_sys::Promise::reject(&error(&e)),
    }
}

/// Edit › Undo on the active document.
#[wasm_bindgen(js_name = undo)]
pub fn undo() -> js_sys::Promise {
    control("engine.execute", json!({ "command": "edit.undo", "params": {} }))
}

/// Edit › Redo on the active document.
#[wasm_bindgen(js_name = redo)]
pub fn redo() -> js_sys::Promise {
    control("engine.execute", json!({ "command": "edit.redo", "params": {} }))
}
