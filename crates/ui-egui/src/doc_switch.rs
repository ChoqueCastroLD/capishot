//! Page switching for hosts that keep one visible document per page (Capikoma): the control
//! methods `editor.showDocument`, `document.prewarm`, `editor.setLayout`,
//! `editor.setMemoryBudget` and `document.memory`.
//!
//! A document is shown double-buffered: its canvas (the GPU canvas texture, or the CPU canvas's
//! tiles) is drawn first for the view it will have ([`crate::canvas::prewarm`]) while the document shown before stays on screen,
//! and the swap (the session's [`photocraft_engine::Session::show_document`]) happens at the
//! start of the frame that draws it, so no frame shows a blank, partial or half-switched canvas.
//! A document whose texture is current (prewarmed, or shown before and kept warm) swaps in the
//! frame of the request. The document shown before becomes a hidden helper and keeps its
//! textures, unless the memory budget evicts them (least recently shown first; the document's
//! parsed data stays).

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use photocraft_doc::DocId;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::control::{ControlRequest, Outcome};

/// How long `editor.showDocument` waits for the new document's canvas by default before it
/// swaps anyway (ms).
const SHOW_TIMEOUT_MS: f64 = 1000.0;
/// `document.prewarm`'s default wait (ms).
const PREWARM_TIMEOUT_MS: f64 = 10_000.0;
/// The longest wait either takes (ms).
const MAX_TIMEOUT_MS: f64 = 120_000.0;

/// The camera a shown or prewarmed document gets.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Viewport {
    /// Its own (fit to the canvas the first time).
    Own,
    /// The active document's zoom, centre and rotation.
    Keep,
    /// `zoom` (device pixels per document pixel) with document point (`x`, `y`) at the centre.
    At { zoom: f32, x: f32, y: f32 },
}

struct Waiting {
    id: DocId,
    reply: Sender<Value>,
    started_ms: f64,
    timeout_ms: f64,
    frames: u32,
    /// `editor.showDocument`: swap when ready, hiding the document shown before when set.
    show: Option<bool>,
    /// `document.prewarm {full: true}`: the whole document at full size, not just the view.
    full: bool,
}

/// What the methods are waiting for, the memory budget and when each document was last shown.
#[derive(Default)]
pub struct DocSwitch {
    waiting: Vec<Waiting>,
    /// `editor.setMemoryBudget`: the canvas textures every document may hold (bytes).
    budget: Option<u64>,
    /// Frame each document was last the active one in ([`PhotocraftApp::frame`]).
    shown: HashMap<DocId, u64>,
}

fn reply_ok(reply: &Sender<Value>, v: Value) {
    let _ = reply.send(json!({"ok": true, "result": v}));
}

fn reply_err(reply: &Sender<Value>, e: impl std::fmt::Display) {
    let _ = reply.send(json!({"ok": false, "error": e.to_string()}));
}

fn obj(p: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    match p {
        Value::Object(m) => Ok(m),
        _ => Err("params must be an object".into()),
    }
}

fn check_fields(m: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("unknown field `{k}` (expected {})", allowed.join(", "))),
        None => Ok(()),
    }
}

fn bool_param(m: &serde_json::Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

fn timeout_param(m: &serde_json::Map<String, Value>, default: f64) -> Result<f64, String> {
    match m.get("timeoutMs") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|t| t.is_finite() && *t >= 0.0).map(|t| t.min(MAX_TIMEOUT_MS)).ok_or_else(|| "`timeoutMs` must be a number ≥ 0".into()),
    }
}

fn doc_param(app: &PhotocraftApp, m: &serde_json::Map<String, Value>, key: &str) -> Result<DocId, String> {
    let id = m.get(key).and_then(Value::as_u64).map(DocId).ok_or_else(|| format!("`{key}` must be an open document's id"))?;
    app.session.index_of_document(id).map(|_| id).ok_or_else(|| format!("no open document {}", id.0))
}

fn viewport_param(m: &serde_json::Map<String, Value>) -> Result<Viewport, String> {
    let num = |v: &serde_json::Map<String, Value>, k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).map(|x| x as f32);
    match m.get("viewport") {
        None | Some(Value::Null) => Ok(Viewport::Own),
        Some(Value::String(s)) if s == "keep" => Ok(Viewport::Keep),
        Some(Value::String(s)) if s == "own" => Ok(Viewport::Own),
        Some(Value::Object(v)) => {
            check_fields(v, &["zoom", "x", "y"]).map_err(|e| format!("viewport: {e}"))?;
            let zoom = num(v, "zoom").filter(|z| (0.001..=1000.0).contains(z)).ok_or("viewport `zoom` must be a number in 0.001..1000")?;
            let (x, y) = (num(v, "x").ok_or("viewport `x` must be a number")?, num(v, "y").ok_or("viewport `y` must be a number")?);
            Ok(Viewport::At { zoom, x, y })
        }
        Some(_) => Err("`viewport` must be \"keep\", \"own\" or {zoom, x, y}".into()),
    }
}

/// Give document `id` the camera `viewport` asks for, without an animation.
fn set_viewport(app: &mut PhotocraftApp, id: DocId, viewport: Viewport) {
    let Some(idx) = app.session.index_of_document(id) else { return };
    let camera = match viewport {
        Viewport::Own => None,
        Viewport::Keep => app.session.active_index().filter(|a| *a != idx).and_then(|a| app.ui.views.get(a)).map(|v| (v.zoom, v.center, v.rotation)),
        Viewport::At { zoom, x, y } => Some((zoom, [x, y], app.ui.views.get(idx).map_or(0.0, |v| v.rotation))),
    };
    let Some(view) = app.ui.views.get_mut(idx) else { return };
    if let Some((zoom, center, rotation)) = camera {
        view.zoom = zoom;
        view.center = center;
        view.rotation = rotation;
        view.fit_pending = false;
        view.fill_pending = false;
    }
    view.request_zoom_animation(false);
}

/// The control methods of this module: `Some` when `req` is one of them.
pub(crate) fn methods(app: &mut PhotocraftApp, ctx: &egui::Context, req: &ControlRequest) -> Option<Outcome> {
    let r = match req.method.as_str() {
        "editor.showDocument" => show(app, ctx, req),
        "document.prewarm" => prewarm(app, ctx, req),
        "editor.setLayout" => set_layout(app, &req.params),
        "editor.setMemoryBudget" => set_budget(app, &req.params),
        "document.memory" => obj(&req.params).and_then(|m| check_fields(m, &[])).map(|()| Some(memory(app))),
        _ => return None,
    };
    Some(match r {
        Ok(Some(v)) => Outcome::Done(json!({"ok": true, "result": v})),
        Ok(None) => Outcome::Pending,
        Err(e) => Outcome::Done(json!({"ok": false, "error": e})),
    })
}

fn show(app: &mut PhotocraftApp, ctx: &egui::Context, req: &ControlRequest) -> Result<Option<Value>, String> {
    let m = obj(&req.params)?;
    check_fields(m, &["id", "viewport", "hidePrevious", "timeoutMs"])?;
    let id = doc_param(app, m, "id")?;
    let viewport = viewport_param(m)?;
    let hide = bool_param(m, "hidePrevious")?.unwrap_or(true);
    let timeout_ms = timeout_param(m, SHOW_TIMEOUT_MS)?;
    // A later show replaces an earlier one still waiting: the host flipped on.
    for w in app.doc_switch.waiting.iter().filter(|w| w.show.is_some()) {
        reply_ok(&w.reply, json!({"id": w.id.0, "shown": false, "superseded": true}));
    }
    app.doc_switch.waiting.retain(|w| w.show.is_none());
    set_viewport(app, id, viewport);
    let started_ms = crate::gpu_canvas::now_ms();
    app.doc_switch.waiting.push(Waiting { id, reply: req.reply.clone(), started_ms, timeout_ms, frames: 0, show: Some(hide), full: false });
    // Ready now (its texture is current): swapped before this frame draws.
    tick(app, ctx);
    Ok(None)
}

fn prewarm(app: &mut PhotocraftApp, ctx: &egui::Context, req: &ControlRequest) -> Result<Option<Value>, String> {
    let m = obj(&req.params)?;
    check_fields(m, &["id", "viewport", "full", "timeoutMs"])?;
    let id = doc_param(app, m, "id")?;
    let viewport = viewport_param(m)?;
    let full = bool_param(m, "full")?.unwrap_or(false);
    let timeout_ms = timeout_param(m, PREWARM_TIMEOUT_MS)?;
    set_viewport(app, id, viewport);
    let started_ms = crate::gpu_canvas::now_ms();
    app.doc_switch.waiting.push(Waiting { id, reply: req.reply.clone(), started_ms, timeout_ms, frames: 0, show: None, full });
    tick(app, ctx);
    Ok(None)
}

fn set_layout(app: &mut PhotocraftApp, p: &Value) -> Result<Option<Value>, String> {
    let m = obj(p)?;
    check_fields(m, &["single"])?;
    let single = bool_param(m, "single")?.ok_or("missing `single` (true or false)")?;
    app.ui.view.single_document = single;
    Ok(Some(json!({"single": single})))
}

fn set_budget(app: &mut PhotocraftApp, p: &Value) -> Result<Option<Value>, String> {
    let m = obj(p)?;
    check_fields(m, &["mb"])?;
    let budget = match m.get("mb") {
        None => return Err("missing `mb` (megabytes, or null for no budget)".into()),
        Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().filter(|mb| mb.is_finite() && *mb >= 0.0 && *mb <= 1e7).ok_or("`mb` must be a number of megabytes ≥ 0")?),
    };
    app.doc_switch.budget = budget.map(|mb| (mb * 1024.0 * 1024.0) as u64);
    let evicted = enforce_budget(app);
    Ok(Some(json!({"mb": budget, "evicted": evicted.iter().map(|id| id.0).collect::<Vec<_>>()})))
}

/// Every frame, after the control requests: note the active document as shown, work on the
/// documents waiting to be shown or prewarmed (swapping a ready one in), keep within the budget.
pub(crate) fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if let Some(id) = app.session.active().map(|st| st.doc.id) {
        app.doc_switch.shown.insert(id, app.frame);
    }
    if !app.doc_switch.waiting.is_empty() {
        let now = crate::gpu_canvas::now_ms();
        let mut keep = Vec::new();
        for mut w in std::mem::take(&mut app.doc_switch.waiting) {
            let Some(idx) = app.session.index_of_document(w.id) else {
                reply_err(&w.reply, format!("document {} was closed", w.id.0));
                continue;
            };
            let drawn = crate::canvas::prewarm(app, ctx, idx, w.full);
            let ready = drawn && crate::user_fonts_ui::fonts_settled(app, w.id);
            let elapsed = now - w.started_ms;
            if !ready && elapsed < w.timeout_ms {
                w.frames += 1;
                keep.push(w);
                continue;
            }
            let mut out = json!({"id": w.id.0, "frames": w.frames, "ms": (elapsed * 10.0).round() / 10.0, "timedOut": !ready});
            if let Some(hide) = w.show {
                let Some(index) = app.session.show_document(w.id, hide) else {
                    reply_err(&w.reply, format!("document {} can't be shown", w.id.0));
                    continue;
                };
                app.sync_views();
                app.doc_switch.shown.insert(w.id, app.frame);
                out["shown"] = json!(true);
                out["index"] = json!(index);
            }
            out["gpuBytes"] = json!(canvas_bytes(app, w.id));
            reply_ok(&w.reply, out);
        }
        app.doc_switch.waiting.extend(keep);
        if !app.doc_switch.waiting.is_empty() {
            ctx.request_repaint();
        }
    }
    if app.doc_switch.budget.is_some() {
        enforce_budget(app);
    }
}

/// The canvas textures document `id` holds: its GPU canvas texture set and its CPU canvas tiles.
pub(crate) fn canvas_bytes(app: &PhotocraftApp, id: DocId) -> u64 {
    let pages = app.gpu.as_ref().map_or(0, |g| g.pages(crate::canvas::viewport_pages::stream_key(id.0)).1);
    let gpu = app.gpu.as_ref().and_then(|g| g.texture_info(id.0)).map_or(0, |(_, b)| b) + pages;
    let tiles: usize = app.canvases.iter().filter(|((d, _), _)| *d == id).filter_map(|(_, c)| c.tiles.as_ref()).map(|t| t.texture_bytes()).sum();
    gpu.saturating_add(tiles as u64).saturating_add(crate::canvas::tile_views_bytes(app, id))
}

/// Evict the canvas textures of the least recently shown documents until every document's fit
/// the budget; named views no tile shows go first, and the active document, those a host's tiles
/// show and those being shown or prewarmed keep theirs. The documents stay open: the next frame
/// that shows one draws it again.
fn enforce_budget(app: &mut PhotocraftApp) -> Vec<DocId> {
    let Some(budget) = app.doc_switch.budget else { return Vec::new() };
    let active = app.session.active().map(|st| st.doc.id);
    let total_now = |app: &PhotocraftApp| -> u64 { app.session.documents().iter().map(|st| canvas_bytes(app, st.doc.id)).sum() };
    let over = total_now(app).saturating_sub(budget);
    if over == 0 {
        return Vec::new();
    }
    crate::canvas::free_idle_tile_views(app, over);
    let mut held: Vec<(DocId, u64)> =
        app.session.documents().iter().map(|st| (st.doc.id, canvas_bytes(app, st.doc.id))).filter(|(_, b)| *b > 0).collect();
    let mut total: u64 = held.iter().map(|(_, b)| b).sum();
    if total <= budget {
        return Vec::new();
    }
    let shown = &app.doc_switch.shown;
    held.sort_by_key(|(id, _)| shown.get(id).copied().unwrap_or(0));
    let mut evicted = Vec::new();
    for (id, bytes) in held {
        if total <= budget {
            break;
        }
        if Some(id) == active || crate::canvas::tiles_show(app, id) || app.doc_switch.waiting.iter().any(|w| w.id == id) {
            continue;
        }
        total = total.saturating_sub(bytes);
        evicted.push(id);
    }
    for id in &evicted {
        evict(app, *id);
    }
    evicted
}

/// Frees document `id`'s canvas textures and caches (it stays open: showing it draws it again).
pub(crate) fn evict(app: &mut PhotocraftApp, id: DocId) {
    if let Some(gpu) = app.gpu.as_ref() {
        gpu.evict(id.0);
        gpu.evict(crate::canvas::viewport_pages::stream_key(id.0));
    }
    app.streams.remove(&id);
    app.canvases.retain(|(d, _), _| *d != id);
    crate::canvas::free_document_tile_views(app, id);
}

/// `document.memory`: what each open document holds, on the GPU and in memory.
fn memory(app: &PhotocraftApp) -> Value {
    let mb = |b: u64| (b as f64 / 1048.576).round() / 1000.0;
    let active = app.session.active().map(|st| st.doc.id);
    let gpu = app.gpu.as_ref();
    let mut gpu_total = 0;
    let documents: Vec<Value> = app
        .session
        .documents()
        .iter()
        .map(|st| {
            let gpu_bytes = canvas_bytes(app, st.doc.id);
            gpu_total += gpu_bytes;
            let pixel_bytes = st.history.pixel_bytes(&st.doc) as u64;
            json!({
                "id": st.doc.id.0,
                "hidden": st.hidden,
                "active": Some(st.doc.id) == active,
                "width": st.doc.size.width,
                "height": st.doc.size.height,
                "gpuMB": mb(gpu_bytes),
                "warm": gpu_bytes > 0,
                "pixelMB": mb(pixel_bytes),
                "framesSinceShown": app.doc_switch.shown.get(&st.doc.id).map(|f| app.frame.saturating_sub(*f)),
            })
        })
        .collect();
    let (compositor, caches) = gpu.and_then(|g| g.compositor_bytes()).unwrap_or((0, 0));
    json!({
        "documents": documents,
        "gpuMB": mb(gpu_total),
        "compositorMB": mb(compositor.saturating_add(caches as u64)),
        "budgetMB": app.doc_switch.budget.map(mb),
        "gpu": gpu.is_some(),
    })
}
