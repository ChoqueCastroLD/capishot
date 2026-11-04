//! Opening a file for both embedding APIs (`openDocument` and `window.photocraft.open`): from
//! bytes or a URL (streamed in as it downloads), shown or hidden, through the decoded-document
//! cache (`photocraft_engine::doc_cache`), settled once the document is in place, also when it
//! decodes in a background or stepped job.

use std::cell::RefCell;
use std::sync::Arc;

use photocraft_engine::jobs::JobId;
use photocraft_ui_egui::{Opening, PhotocraftApp};
use serde_json::Value;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::embed_api_core::{self as core, OpenOptions};

/// The document an open put in place: its `DocumentInfo` (`id`, `hidden`, `active`…) and the
/// import warnings; `cached` when it came from the decoded-document cache.
#[derive(Debug)]
pub struct Opened {
    pub info: Value,
    pub warnings: Vec<String>,
    pub cached: bool,
}

impl Opened {
    pub fn id(&self) -> Option<u64> {
        self.info.get("id").and_then(Value::as_u64)
    }
}

/// What to do once an open ended.
pub type Done = Box<dyn FnOnce(Result<Opened, String>)>;

/// An open whose job is still decoding: its cache key and version, and who waits for it.
type Pending = (JobId, Option<(String, String)>, Done);

thread_local! {
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
}

/// Opens `bytes` (`None`: from the cache only) as `name`. With `options.cache` a document cached
/// under that key and version opens at once, sharing its pixels with the cache (copy-on-write); a
/// document decoded from `bytes` is cached once in place. `done` runs this frame, or when the
/// background open ends ([`poll`]).
pub fn start(app: &mut PhotocraftApp, name: &str, bytes: Option<Vec<u8>>, options: &OpenOptions, done: Done) {
    if let Some((key, version)) = &options.cache
        && let Some((mut doc, warnings)) = app.session.doc_cache.get(key, version)
    {
        doc.name = name.to_string();
        let r = app.open_decoded(doc, options.hidden, options.quiet).and_then(|i| info_of(app, i)).map(|info| Opened { info, warnings, cached: true });
        return done(r);
    }
    let Some(bytes) = bytes else {
        let (key, version) = options.cache.clone().unwrap_or_default();
        return done(Err(format!("`{key}` isn't in the document cache at version `{version}`")));
    };
    let opening = if options.hidden { app.open_hidden_owned(name, bytes) } else { open_shown(app, name, bytes, options.quiet) };
    match opening {
        Ok(Opening::Job(job)) => {
            app.jobs.track_open(job);
            PENDING.with(|p| p.borrow_mut().push((job, options.cache.clone(), done)));
        }
        Ok(Opening::Opened(index, warnings)) => {
            let r = info_of(app, index).map(|info| Opened { info, warnings, cached: false });
            remember(app, options.cache.as_ref(), &r);
            done(r);
        }
        Err(e) => done(Err(e)),
    }
}

/// A shown document: opened as File › Open does (a PSD/PSB in a background or stepped job, its
/// merged image shown meanwhile; alone, with no opening card, when `quiet`).
fn open_shown(app: &mut PhotocraftApp, name: &str, bytes: Vec<u8>, quiet: bool) -> Result<Opening, String> {
    let before: Vec<u64> = app.session.documents().iter().map(|st| st.doc.id.0).collect();
    let pending = app.jobs.opens.len();
    let warnings = app.open_owned(name, bytes).inspect_err(|e| app.open_failed(name, e))?;
    if let Some(tab) = app.jobs.opens.get_mut(pending) {
        tab.quiet = quiet;
        return Ok(Opening::Job(tab.job));
    }
    app.sync_views();
    let index = app
        .session
        .documents()
        .iter()
        .position(|st| !before.contains(&st.doc.id.0))
        .ok_or_else(|| format!("{name} didn't open as a document (brush, gradient and swatch files go to the preset libraries)"))?;
    Ok(Opening::Opened(index, warnings))
}

fn info_of(app: &PhotocraftApp, index: usize) -> Result<Value, String> {
    let st = app.session.documents().get(index).ok_or("the document closed")?;
    Ok(core::doc_info(st, app.session.active_index() == Some(index)))
}

/// Keeps an opened document in the cache under `cache`'s key and version.
fn remember(app: &mut PhotocraftApp, cache: Option<&(String, String)>, opened: &Result<Opened, String>) {
    let (Some((key, version)), Ok(o)) = (cache, opened) else { return };
    let Some(doc) = o.id().and_then(|id| app.session.documents().iter().find(|st| st.doc.id.0 == id)).map(|st| Arc::clone(&st.doc)) else { return };
    app.session.doc_cache.put(key, version, doc, o.warnings.clone());
}

/// Every frame: settles the opens whose background job ended.
pub fn poll(app: &mut PhotocraftApp) {
    for (job, outcome) in app.jobs.take_open_outcomes() {
        let Some((cache, done)) = PENDING.with(|p| {
            let mut p = p.borrow_mut();
            let i = p.iter().position(|(j, ..)| *j == job)?;
            let (_, cache, done) = p.remove(i);
            Some((cache, done))
        }) else {
            continue;
        };
        let r = outcome.and_then(|(id, warnings)| {
            let index = app.session.documents().iter().position(|st| st.doc.id == id).ok_or("the document closed")?;
            info_of(app, index).map(|info| Opened { info, warnings, cached: false })
        });
        remember(app, cache.as_ref(), &r);
        done(r);
    }
}

/// Fails every open still decoding with `why` (the app is going away).
pub fn abandon(why: &str) {
    for (_, _, done) in PENDING.with(|p| std::mem::take(&mut *p.borrow_mut())) {
        done(Err(why.to_string()));
    }
}

/// A document name from `url`: its last path segment, else `document`.
pub fn name_of_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let last = path.rsplit('/').next().unwrap_or_default();
    let name = js_sys::decode_uri_component(last).ok().and_then(|s| s.as_string()).unwrap_or_else(|| last.to_string());
    if name.trim().is_empty() { "document".into() } else { name }
}

fn js_err(e: JsValue) -> String {
    e.dyn_ref::<js_sys::Error>().map_or_else(|| format!("{e:?}"), |e| String::from(e.message()))
}

/// The bytes at `url`, fetched as the page fetches and read from the response's stream as they
/// arrive: chunk by chunk into one buffer sized from `Content-Length`, so the page keeps drawing
/// and nothing is held twice (no whole-file ArrayBuffer copied in afterwards).
pub async fn fetch_streamed(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_str(url)).await.map_err(js_err)?.dyn_into().map_err(js_err)?;
    if !resp.ok() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }
    let mut out = Vec::new();
    // A compressed response's length is the compressed size: a hint, never a limit.
    if let Some(n) = resp.headers().get("content-length").ok().flatten().and_then(|n| n.trim().parse::<usize>().ok()) {
        out.try_reserve_exact(n).map_err(|_| "not enough memory to read the file".to_string())?;
    }
    let body = js_sys::Reflect::get(&resp, &"body".into()).unwrap_or(JsValue::NULL);
    let reader = (!body.is_null() && !body.is_undefined())
        .then(|| js_sys::Reflect::get(&body, &"getReader".into()).ok()?.dyn_into::<js_sys::Function>().ok()?.call0(&body).ok())
        .flatten();
    let Some(reader) = reader else {
        // No streams: the whole body at once.
        let buf = JsFuture::from(resp.array_buffer().map_err(js_err)?).await.map_err(js_err)?;
        return Ok(crate::web::read_array(&js_sys::Uint8Array::new(&buf)).await);
    };
    let read: js_sys::Function = js_sys::Reflect::get(&reader, &"read".into()).map_err(js_err)?.dyn_into().map_err(|_| "no stream reader".to_string())?;
    loop {
        let promise: js_sys::Promise = read.call0(&reader).map_err(js_err)?.dyn_into().map_err(|_| "no stream reader".to_string())?;
        let chunk = JsFuture::from(promise).await.map_err(js_err)?;
        if js_sys::Reflect::get(&chunk, &"done".into()).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            break;
        }
        let Ok(value) = js_sys::Reflect::get(&chunk, &"value".into()).and_then(|v| v.dyn_into::<js_sys::Uint8Array>()) else {
            return Err("the response stream gave something other than bytes".into());
        };
        let at = out.len();
        let n = value.length() as usize;
        out.try_reserve(n).map_err(|_| "not enough memory to read the file".to_string())?;
        out.resize(at + n, 0);
        if let Some(dst) = out.get_mut(at..) {
            value.copy_to(dst);
        }
    }
    Ok(out)
}
