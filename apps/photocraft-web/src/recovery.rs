//! Crash recovery in the browser: autosave of unsaved documents and rescue after a crash.
//!
//! Checkpoints use the desktop's recovery format (`photocraft_format::ObjectAutosaver`: the
//! document's compressed tiles and blobs by content hash, then a descriptor) and live in the
//! origin-private file system, or IndexedDB where the browser can't write files there
//! (`recovery_store.js`). A checkpoint after a small edit stores only the tiles it changed.
//!
//! * **Autosave** (`Services::autosave`, every Preferences › File Handling › Autosave minutes,
//!   3 by default): a document with unsaved changes is encoded on the worker pool (threads build;
//!   on the page's thread in the single-threaded build), then written by the browser
//!   asynchronously, so the page never waits on storage. Saving or closing a document removes its
//!   checkpoint (`Services::discard_autosave`).
//! * **Rescue**: each page holds a Web Lock for its session and marks it closed cleanly on
//!   `pagehide`. On load, checkpoints of a session that is neither alive nor closed cleanly are
//!   offered in the "Recover N documents" dialog (`photocraft_ui_egui::recovery_ui`); File ›
//!   Recover Documents… offers every checkpoint no open tab owns.
//! * **Emergency**: a GPU device loss checkpoints at once (`gpu_status`); a panic hands any
//!   checkpoint already encoded to the browser (its write finishes without the app) and marks the
//!   session crashed, so the next load offers the documents.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, PoisonError};

// Locks the worker pool also takes spin on the page's thread instead of blocking (where
// `Atomics.wait` throws).
use wasm_sync::Mutex;

use photocraft_doc::Document;
use photocraft_format::{ObjectAutosaver, ObjectCheckpoint, RecoveryInfo};
use photocraft_ui_egui::recovery_ui::{RecoveryAnswer, RecoveryOfferEntry};
use photocraft_ui_egui::{AutosaveCompletion, PhotocraftApp, Recoverable, RecoveredDocument, Services};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/recovery_store.js")]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn rcv_write(key: &str, names: js_sys::Array, datas: js_sys::Array, descriptor: js_sys::Uint8Array, thumb: JsValue, live: js_sys::Array) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn rcv_list() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn rcv_read(key: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn rcv_remove(key: &str) -> Result<JsValue, JsValue>;
    async fn rcv_persist() -> JsValue;
    fn rcv_session_start(id: &str);
    fn rcv_mark_crashed(id: &str);
    fn rcv_clean_sessions() -> js_sys::Array;
    fn rcv_set_clean_sessions(list: js_sys::Array);
    #[wasm_bindgen(catch)]
    async fn rcv_live_sessions() -> Result<JsValue, JsValue>;
}

/// Longest side of the dialog's thumbnails, in pixels.
const THUMB_SIDE: u32 = 112;

/// One document's checkpoint key and the writer that knows what its storage holds.
struct Slot {
    key: String,
    saver: Arc<Mutex<ObjectAutosaver>>,
    /// Encoding or writing a checkpoint.
    busy: bool,
    /// The newest checkpoint asked for while busy.
    pending: Option<Job>,
    /// Being removed: no checkpoint is accepted.
    retiring: bool,
}

struct Job {
    doc: Arc<Document>,
    history: photocraft_ops::HistoryCheckpoint,
    revision: u64,
    path: Option<String>,
    context: serde_json::Value,
}

/// An encoded checkpoint and its thumbnail (a PNG).
type Checkpointed = (ObjectCheckpoint, Option<Vec<u8>>);
/// A checkpoint encoded off the page's thread, waiting to be written.
type Encoded = Arc<Mutex<Option<Result<Checkpointed, String>>>>;

#[derive(Default)]
struct Inner {
    session: String,
    next_key: u64,
    slots: HashMap<u64, Slot>,
    /// Encodes running on the worker pool: document, revision, result slot.
    encoding: Vec<(u64, u64, Encoded)>,
    completions: Vec<AutosaveCompletion>,
    /// What to offer the user (entries, after a crash).
    offer: Option<(Vec<RecoveryOfferEntry>, bool)>,
    /// Recovered documents read from storage, for the app to open.
    recovered: Vec<Recoverable>,
    /// Messages for the user (titles of error notices).
    errors: Vec<String>,
    persist_asked: bool,
    /// Open what is offered without asking (the page reloaded after its graphics context was
    /// lost: gpu_lost.rs).
    auto_accept: bool,
}

/// The browser's recovery manager, shared by the services and the shell.
#[derive(Clone)]
pub struct Recovery(Rc<RefCell<Inner>>);

thread_local! {
    /// For the panic hook, which gets no reference to the app.
    static CURRENT: RefCell<Option<std::rc::Weak<RefCell<Inner>>>> = const { RefCell::new(None) };
}

/// Undo history a checkpoint keeps: the last 20 steps, within 150 MB (50 MB without threads; 5
/// steps within 16 MB on a phone, a window under 600 px on its short side), so Undo works after
/// Recover.
fn history_limits() -> photocraft_format::HistoryLimits {
    static PHONE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // Measured once, on the page's thread (`kick`).
    let phone = *PHONE.get_or_init(|| {
        web_sys::window()
            .and_then(|w| Some(w.inner_width().ok()?.as_f64()?.min(w.inner_height().ok()?.as_f64()?)))
            .is_some_and(|short| short < 600.0)
    });
    // Without threads a checkpoint is encoded on the page's thread: the phone's cap keeps the
    // first one (which encodes every kept step) short there too.
    let small = phone || cfg!(not(target_feature = "atomics"));
    // A phone keeps 5 steps within 16 MB: each checkpoint encodes and copies what it keeps, and
    // on a phone that peak is what kills the tab.
    if phone {
        return photocraft_format::HistoryLimits { max_states: 5, max_bytes: 16 << 20 };
    }
    photocraft_format::HistoryLimits { max_states: 20, max_bytes: if small { 50 << 20 } else { 150 << 20 } }
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// A fresh session id: letters and digits only (it ends up in checkpoint keys and lock names).
fn session_id() -> String {
    let r = (js_sys::Math::random() * 1e12) as u64;
    format!("{:x}{:x}", now_ms() as u64, r)
}

/// The session part of a checkpoint key `web-<session>-<n>`.
fn session_of(key: &str) -> Option<&str> {
    key.strip_prefix("web-")?.split('-').next()
}

/// A user-facing reason for a storage failure.
fn storage_error(e: &JsValue) -> String {
    let name = js_sys::Reflect::get(e, &"name".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
    if name == "QuotaExceededError" {
        return photocraft_ui_egui::i18n::t(
            "The browser's storage for this site is full, so unsaved changes can't be kept for recovery. Save your work (File › Save), or free space in the browser's site settings.",
        )
        .to_string();
    }
    let message = js_sys::Reflect::get(e, &"message".into()).ok().and_then(|v| v.as_string()).unwrap_or_else(|| format!("{e:?}"));
    if message.is_empty() { name } else { message }
}

impl Recovery {
    /// Starts this page's session and the services that autosave through it.
    pub fn start() -> Self {
        let inner = Inner { session: session_id(), ..Default::default() };
        rcv_session_start(&inner.session);
        let me = Recovery(Rc::new(RefCell::new(inner)));
        CURRENT.with(|c| *c.borrow_mut() = Some(Rc::downgrade(&me.0)));
        me
    }

    /// This page is leaving without closing cleanly (its graphics context was lost and it
    /// reloads): the next load offers its checkpoints.
    pub fn mark_crashed(&self) {
        rcv_mark_crashed(&self.0.borrow().session);
    }

    /// Open the documents the next offer lists without asking.
    pub fn auto_accept(&self) {
        self.0.borrow_mut().auto_accept = true;
    }

    /// The autosave services for [`Services`].
    pub fn services(&self) -> Services {
        let (queue, discard, poll) = (self.clone(), self.clone(), self.clone());
        Services {
            autosave: Some(Box::new(move |doc, history, revision, path, key, context| queue.queue(doc, history, revision, path, key, context))),
            discard_autosave: Some(Box::new(move |id, key| discard.discard(id, key))),
            poll_autosave: Some(Box::new(move || poll.poll())),
            autosave_unsaved_only: true,
            recovery_menu: true,
            ..Default::default()
        }
    }

    fn queue(&self, doc: &Arc<Document>, history: photocraft_ops::HistoryCheckpoint, revision: u64, path: Option<&str>, key: Option<&str>, context: serde_json::Value) -> Result<(), String> {
        {
            let mut inner = self.0.borrow_mut();
            let slot = inner.claim(doc.id.0, key)?;
            if slot.retiring {
                return Err("Recovery cleanup is still finishing".into());
            }
            slot.pending = Some(Job { doc: doc.clone(), history, revision, path: path.map(str::to_string), context });
        }
        self.kick(doc.id.0);
        Ok(())
    }

    fn discard(&self, id: u64, key: Option<&str>) -> Result<(), String> {
        let start = {
            let mut inner = self.0.borrow_mut();
            let slot = inner.claim(id, key)?;
            if slot.retiring {
                return Ok(());
            }
            slot.retiring = true;
            slot.pending = None;
            (!slot.busy).then(|| slot.key.clone())
        };
        if let Some(key) = start {
            self.remove(id, key);
        }
        Ok(())
    }

    /// Removes `key`'s checkpoint, then reports document `id` retired.
    fn remove(&self, id: u64, key: String) {
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = rcv_remove(&key).await.map(|_| ()).map_err(|e| storage_error(&e));
            let mut inner = me.0.borrow_mut();
            if result.is_ok() {
                inner.slots.remove(&id);
            } else if let Some(slot) = inner.slots.get_mut(&id) {
                slot.retiring = false;
            }
            inner.completions.push(AutosaveCompletion { document_id: id, revision: 0, result, retired: true });
        });
    }

    /// Starts the next checkpoint of document `id` unless one is under way.
    fn kick(&self, id: u64) {
        let (job, key, saver) = {
            let mut inner = self.0.borrow_mut();
            let Some(slot) = inner.slots.get_mut(&id) else { return };
            if slot.busy || slot.retiring {
                return;
            }
            let Some(job) = slot.pending.take() else { return };
            slot.busy = true;
            (job, slot.key.clone(), slot.saver.clone())
        };
        let info = RecoveryInfo {
            key,
            document_name: job.doc.name.clone(),
            original_path: job.path.clone(),
            saved_at: (now_ms() / 1000.0) as u64,
            revision: job.revision,
        };
        let revision = job.revision;
        // On the page's thread (a worker has no window to measure).
        let limits = history_limits();
        let encode = move || -> Result<Checkpointed, String> {
            let checkpoint = saver
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .checkpoint_with_history(&job.doc, Some(&job.history), limits, info, job.context)
                .map_err(|e| e.to_string())?;
            Ok((checkpoint, thumbnail(&job.doc)))
        };
        let slot: Encoded = Arc::default();
        // The threads build encodes on the worker pool; the page keeps drawing meanwhile.
        #[cfg(target_feature = "atomics")]
        {
            let out = slot.clone();
            rayon::spawn(move || {
                let r = encode();
                *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
            });
        }
        #[cfg(not(target_feature = "atomics"))]
        {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(encode());
        }
        self.0.borrow_mut().encoding.push((id, revision, slot));
    }

    /// Hands finished encodes to the browser and returns the checkpoints written or removed.
    fn poll(&self) -> Vec<AutosaveCompletion> {
        let ready: Vec<(u64, u64, Result<Checkpointed, String>)> = {
            let mut inner = self.0.borrow_mut();
            let mut ready = Vec::new();
            inner.encoding.retain(|(id, revision, slot)| match slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                Some(r) => {
                    ready.push((*id, *revision, r));
                    false
                }
                None => true,
            });
            ready
        };
        for (id, revision, r) in ready {
            match r {
                Ok((checkpoint, thumb)) => self.write(id, revision, checkpoint, thumb),
                Err(e) => self.finish(id, revision, Err(e), None),
            }
        }
        std::mem::take(&mut self.0.borrow_mut().completions)
    }

    /// Writes an encoded checkpoint. The bytes are copied to the browser before this returns, so
    /// the write completes even if the app stops meanwhile.
    fn write(&self, id: u64, revision: u64, checkpoint: ObjectCheckpoint, thumb: Option<Vec<u8>>) {
        let Some(key) = self.0.borrow().slots.get(&id).map(|s| s.key.clone()) else { return };
        let (names, datas) = (js_sys::Array::new(), js_sys::Array::new());
        for (name, data) in &checkpoint.objects {
            names.push(&JsValue::from_str(name));
            datas.push(&js_sys::Uint8Array::from(data.as_slice()));
        }
        let live: js_sys::Array = checkpoint.live.iter().map(|n| JsValue::from_str(n)).collect();
        let descriptor = js_sys::Uint8Array::from(checkpoint.descriptor.as_slice());
        let thumb = thumb.map_or(JsValue::NULL, |t| js_sys::Uint8Array::from(t.as_slice()).into());
        let live_names = checkpoint.live;
        drop(checkpoint.objects);
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = rcv_write(&key, names, datas, descriptor, thumb, live).await.map(|_| ()).map_err(|e| storage_error(&e));
            me.finish(id, revision, result, Some(live_names));
            me.ask_persist().await;
        });
    }

    /// A checkpoint of document `id` ended: report it and start the next (or the removal).
    fn finish(&self, id: u64, revision: u64, result: Result<(), String>, live: Option<Vec<String>>) {
        let retire = {
            let mut inner = self.0.borrow_mut();
            let Some(slot) = inner.slots.get_mut(&id) else { return };
            slot.busy = false;
            {
                let mut saver = slot.saver.lock().unwrap_or_else(PoisonError::into_inner);
                match (&result, live) {
                    (Ok(()), Some(live)) => saver.committed(live),
                    _ => saver.forget(),
                }
            }
            let retire = slot.retiring.then(|| slot.key.clone());
            if retire.is_none() {
                inner.completions.push(AutosaveCompletion { document_id: id, revision, result, retired: false });
            }
            retire
        };
        match retire {
            Some(key) => self.remove(id, key),
            None => self.kick(id),
        }
    }

    async fn ask_persist(&self) {
        let ask = !std::mem::replace(&mut self.0.borrow_mut().persist_asked, true);
        if ask {
            let _ = rcv_persist().await;
        }
    }

    /// Lists the checkpoints to offer: after a crash (`crashed`: only those of sessions that died
    /// without closing cleanly, on load) or every one no open tab owns (File › Recover Documents…).
    pub fn discover(&self, crashed: bool) {
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let list = match rcv_list().await {
                Ok(list) => list,
                Err(e) => {
                    let mut inner = me.0.borrow_mut();
                    inner.errors.push(storage_error(&e));
                    if !crashed {
                        inner.offer = Some((Vec::new(), false));
                    }
                    return;
                }
            };
            let live: Option<Vec<String>> = rcv_live_sessions().await.ok().filter(|v| !v.is_null()).map(|v| js_sys::Array::from(&v).iter().filter_map(|s| s.as_string()).collect());
            let clean: Vec<String> = rcv_clean_sessions().iter().filter_map(|s| s.as_string()).collect();
            let (own, owned): (String, Vec<String>) = {
                let inner = me.0.borrow();
                (inner.session.clone(), inner.slots.values().map(|s| s.key.clone()).collect())
            };
            let mut entries = Vec::new();
            let mut sessions_with_checkpoints = std::collections::HashSet::new();
            for item in js_sys::Array::from(&list).iter() {
                let item = js_sys::Array::from(&item);
                let Some(key) = item.get(0).as_string() else { continue };
                let Some(session) = session_of(&key).map(str::to_string) else { continue };
                sessions_with_checkpoints.insert(session.clone());
                let alive = session == own || live.as_ref().is_some_and(|l| l.contains(&session));
                if alive || owned.contains(&key) {
                    continue;
                }
                let descriptor = item.get(1);
                let status = item.get(3).as_string().unwrap_or_default();
                if status == "incomplete" || descriptor.is_null() || descriptor.is_undefined() {
                    // A first write that never finished in a session that is gone: nothing to offer.
                    if status != "damaged" {
                        let _ = rcv_remove(&key).await;
                        continue;
                    }
                }
                if crashed && clean.contains(&session) {
                    continue;
                }
                // A checkpoint with nothing whole left isn't offered after a crash; File ›
                // Recover Documents… still lists it, marked damaged.
                let damaged = status == "damaged";
                if damaged && crashed {
                    continue;
                }
                let bytes = if descriptor.is_null() || descriptor.is_undefined() { Vec::new() } else { js_sys::Uint8Array::new(&descriptor).to_vec() };
                let info = photocraft_format::object_checkpoint_info(&bytes).ok();
                if info.is_none() && !damaged {
                    continue;
                }
                let thumb = item.get(2);
                let thumbnail = (!thumb.is_null() && !thumb.is_undefined()).then(|| js_sys::Uint8Array::new(&thumb).to_vec()).and_then(|png| decode_png(&png));
                let mut name = info.as_ref().map_or_else(|| key.clone(), |i| i.document_name.clone());
                if damaged {
                    name = photocraft_ui_egui::i18n::fmt(photocraft_ui_egui::i18n::t("{name} (damaged)"), &[("name", &name)]);
                }
                entries.push(RecoveryOfferEntry { key, name, saved_at_ms: info.map_or(0.0, |i| i.saved_at as f64 * 1000.0), thumbnail });
            }
            // Forget clean marks of sessions that left nothing behind.
            let kept: js_sys::Array = clean.iter().filter(|s| sessions_with_checkpoints.contains(*s)).map(|s| JsValue::from_str(s)).collect();
            rcv_set_clean_sessions(kept);
            entries.sort_by(|a, b| b.saved_at_ms.total_cmp(&a.saved_at_ms));
            me.0.borrow_mut().offer = Some((entries, crashed));
        });
    }

    /// Loads the checkpoints `keys` for the app to open.
    fn read(&self, keys: Vec<String>) {
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            for key in keys {
                let read = match rcv_read(&key).await {
                    Ok(v) => v,
                    Err(e) => {
                        me.0.borrow_mut().errors.push(storage_error(&e));
                        continue;
                    }
                };
                let get = |name: &str| js_sys::Reflect::get(&read, &name.into()).unwrap_or(JsValue::UNDEFINED);
                if get("status").as_string().as_deref() == Some("previous") {
                    me.0.borrow_mut().errors.push(photocraft_ui_egui::i18n::t("The last autosave didn't finish: the one before it was recovered.").to_string());
                }
                let descriptor = js_sys::Uint8Array::new(&get("descriptor")).to_vec();
                let (names, datas) = (js_sys::Array::from(&get("names")), js_sys::Array::from(&get("datas")));
                let mut objects = HashMap::with_capacity(names.length() as usize);
                for (name, data) in names.iter().zip(datas.iter()) {
                    if let Some(name) = name.as_string() {
                        objects.insert(name, js_sys::Uint8Array::new(&data).to_vec());
                    }
                }
                let Ok(info) = photocraft_format::object_checkpoint_info(&descriptor) else {
                    me.0.borrow_mut().errors.push(format!("{key}: unreadable recovery checkpoint"));
                    continue;
                };
                let (name, path) = (info.document_name.clone(), info.original_path.clone());
                let load_key = key.clone();
                me.0.borrow_mut().recovered.push(Recoverable {
                    key,
                    name,
                    path: path.clone(),
                    // Decoded by the recovery job, on a worker in the threads build.
                    load: Box::new(move || {
                        let (document, history, context) = photocraft_format::recover_object_checkpoint(&descriptor, Arc::new(objects)).map_err(|e| e.to_string())?;
                        Ok(RecoveredDocument { key: load_key, path, document, history: Some(history), context })
                    }),
                });
            }
        });
    }

    /// Per frame: hand the app what storage delivered, and act on its recovery answers.
    pub fn tick(&self, app: &mut PhotocraftApp) {
        if app.take_recovery_request() {
            self.discover(false);
        }
        if let Some(answer) = app.take_recovery_answer() {
            match answer {
                RecoveryAnswer::Recover(keys) => self.read(keys),
                RecoveryAnswer::Discard(keys) => {
                    for key in keys {
                        let me = self.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            if let Err(e) = rcv_remove(&key).await {
                                me.0.borrow_mut().errors.push(storage_error(&e));
                            }
                        });
                    }
                }
                RecoveryAnswer::Later => {}
            }
        }
        let (offer, recovered, errors) = {
            let mut inner = self.0.borrow_mut();
            (inner.offer.take(), std::mem::take(&mut inner.recovered), std::mem::take(&mut inner.errors))
        };
        if let Some((entries, crashed)) = offer {
            app.offer_recovery(entries, crashed);
            if std::mem::take(&mut self.0.borrow_mut().auto_accept) {
                app.accept_recovery_offer();
            }
        }
        if !recovered.is_empty() {
            app.recover_documents(recovered);
        }
        for e in errors {
            photocraft_ui_egui::notices::post(app, photocraft_ui_egui::i18n::t("Crash recovery"), vec![e], true, None);
        }
    }
}

impl Inner {
    /// Document `id`'s slot, created with `inherited` (a recovered document's key) or a new key.
    fn claim(&mut self, id: u64, inherited: Option<&str>) -> Result<&mut Slot, String> {
        if !self.slots.contains_key(&id) {
            let key = match inherited {
                Some(key) => {
                    if key.is_empty() || key.len() > 200 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        return Err("Invalid recovery key".into());
                    }
                    if self.slots.values().any(|s| s.key == key) {
                        return Err("Recovery snapshot already belongs to another document".into());
                    }
                    key.to_string()
                }
                None => {
                    self.next_key += 1;
                    format!("web-{}-{}", self.session, self.next_key)
                }
            };
            self.slots.insert(id, Slot { key, saver: Arc::default(), busy: false, pending: None, retiring: false });
        }
        self.slots.get_mut(&id).ok_or_else(|| "Recovery ownership unavailable".into())
    }
}

/// A small PNG of `doc` for the recovery dialog.
fn thumbnail(doc: &Document) -> Option<Vec<u8>> {
    let buf = photocraft_compose::thumbnail_buffer(doc, THUMB_SIDE);
    let (w, h) = (buf.rect.width(), buf.rect.height());
    if w == 0 || h == 0 || buf.px.len() != (w as usize) * (h as usize) {
        return None;
    }
    let q = |v: f32| (if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) } * 255.0).round() as u8;
    let rgba: Vec<u8> = buf.px.iter().flat_map(|p| p.map(q)).collect();
    let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, rgba).ok()?;
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).ok()
}

fn decode_png(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = photocraft_codecs::decode(png).ok()?;
    (img.width() <= 1024 && img.height() <= 1024).then(|| (img.width(), img.height(), img.to_rgba8()))
}

/// Panic hook: the app can't go on. Hand any checkpoint already encoded to the browser (its write
/// completes on its own) and mark the session crashed, so the next load offers its documents.
pub fn on_panic() {
    CURRENT.with(|c| {
        let Some(inner) = c.try_borrow().ok().and_then(|c| c.as_ref().and_then(std::rc::Weak::upgrade)) else { return };
        let me = Recovery(inner);
        let Ok(session) = me.0.try_borrow().map(|i| i.session.clone()) else { return };
        rcv_mark_crashed(&session);
        let ready: Vec<(u64, u64, Checkpointed)> = {
            let Ok(mut inner) = me.0.try_borrow_mut() else { return };
            let mut ready = Vec::new();
            inner.encoding.retain(|(id, revision, slot)| match slot.try_lock().ok().and_then(|mut s| s.take()) {
                Some(Ok(done)) => {
                    ready.push((*id, *revision, done));
                    false
                }
                _ => true,
            });
            ready
        };
        for (id, revision, (checkpoint, thumb)) in ready {
            me.write(id, revision, checkpoint, thumb);
        }
    });
}
