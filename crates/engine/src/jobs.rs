//! Background jobs: long commands run on a worker thread, report progress and can be cancelled
//! (#210).
//!
//! A command opts in by doing its heavy work through [`run`] (or [`edit_job`] for a plain document
//! edit). The work closure gets a [`JobCtx`] (progress plus a cancellation flag) and runs against
//! a snapshot: documents are copy-on-write, so cloning one is O(layers). Its result is applied on
//! the UI thread as one undo step, through the same path as [`Session::execute`], so the journal,
//! history, damage and post-command hooks all behave as for a synchronous command.
//!
//! - [`Session::execute`] runs everything inline, as before: tests, the CLI and scripts see no
//!   difference.
//! - [`Session::start`] runs a job-capable command on a worker thread and returns its
//!   [`JobId`]; [`Session::poll_jobs`] (called every frame by the UI) applies finished jobs.
//!   On wasm `start` runs inline (same API, written once).
//! - [`Session::cancel_job`] sets the flag; the algorithms check it per tile, row band or
//!   iteration, and the result is discarded. The document is never touched by a worker.
//! - A panic in the worker becomes an error; the document is unchanged.
//! - While a job runs on a document, commands that would edit that document are disabled with a
//!   message naming the job (queries and document-independent commands still run).

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, PoisonError, Weak};

// A job's status and notices are written by its worker and read by the page's thread every frame:
// a std lock contended on the page's thread of the web threads build waits with `Atomics.wait`,
// which browsers forbid there. wasm_sync's lock spins on that thread (std's everywhere else).
use wasm_sync::Mutex;

use photocraft_doc::{DocId, Document, LayerId};
use photocraft_raster::Interrupt;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

type Authorization = fn(&str, &Value) -> Result<()>;

thread_local! {
    // Smart-filter rendering has no Session argument. Carry the requesting session's gate
    // through synchronous rendering and explicitly reinstall it on background workers.
    static RENDER_AUTHORIZATION: std::cell::Cell<Option<Authorization>> = const { std::cell::Cell::new(None) };
}

pub(crate) struct AuthorizationScope(Option<Authorization>);

impl AuthorizationScope {
    fn enter(gate: Option<Authorization>) -> Self {
        Self(RENDER_AUTHORIZATION.with(|current| current.replace(gate.or(current.get()))))
    }
}

impl Drop for AuthorizationScope {
    fn drop(&mut self) {
        RENDER_AUTHORIZATION.with(|current| current.set(self.0));
    }
}

/// [`AuthorizationScope`] entered with `gate` (or the one already installed, so trusted work on
/// this thread keeps ambient paths usable). [`Session::edit`] and the job paths use this to carry
/// the requesting session's authority into every smart-object re-render.
pub(crate) fn render_authorization_scope(gate: Option<Authorization>) -> AuthorizationScope {
    AuthorizationScope::enter(gate)
}

/// Recheck a stored filter's command and parameters before evaluating it or using its cache.
pub(crate) fn authorize_render_command(id: &str, params: &Value) -> Result<()> {
    match RENDER_AUTHORIZATION.with(std::cell::Cell::get) {
        Some(gate) => gate(id, params),
        None => Ok(()),
    }
}

/// Whether the work on this thread is done for an untrusted session (a gate is installed).
pub(crate) fn authorization_installed() -> bool {
    RENDER_AUTHORIZATION.with(std::cell::Cell::get).is_some()
}

/// Identifies one background job for the session's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub struct JobId(pub u64);

/// What a job's work closure sees: report progress, check for cancellation. Cheap to clone; all
/// clones share the same state.
#[derive(Clone, Default)]
pub struct JobCtx {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    cancel: AtomicBool,
    /// `f32` bits of the progress fraction. Non-negative floats order like their bits, so
    /// `fetch_max` keeps progress monotonic even with several reporting threads.
    progress: AtomicU32,
    message: Mutex<String>,
    /// [`JobCtx::notice`]s, added to the result as `notices`.
    notices: Mutex<Vec<Value>>,
}

impl std::fmt::Debug for JobCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCtx").field("progress", &self.fraction()).field("cancelled", &self.cancelled()).finish()
    }
}

impl JobCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Report progress in `0.0..=1.0` with a short status (empty keeps the previous one).
    /// Progress never goes backwards: a lower value than already reported is ignored.
    pub fn progress(&self, fraction: f32, message: &str) {
        if fraction.is_finite() {
            self.shared.progress.fetch_max(fraction.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
        if !message.is_empty() {
            let mut m = self.shared.message.lock().unwrap_or_else(PoisonError::into_inner);
            if *m != message {
                message.clone_into(&mut m);
            }
        }
    }

    /// Has the job been cancelled? Long loops check this and stop early.
    pub fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    /// Ask the job to stop (idempotent).
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    /// `Err(Cancelled)` once cancelled, for `?` between stages.
    pub fn check(&self) -> Result<()> {
        if self.cancelled() { Err(EngineError::Cancelled) } else { Ok(()) }
    }

    /// Progress reported so far (`0.0..=1.0`).
    pub fn fraction(&self) -> f32 {
        f32::from_bits(self.shared.progress.load(Ordering::Relaxed))
    }

    /// The latest status message.
    pub fn message(&self) -> String {
        self.shared.message.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Something the user should know about a job that succeeded anyway ("the GPU couldn't run
    /// this model; used the CPU instead"): `class` is a stable id (`ai.gpuFallback`), `text` the
    /// message to show, `cause` the underlying error (for agents and the error's telemetry
    /// class, never sent as text). The result gets `"notices": [{"class", "text", "cause"}]`.
    pub fn notice(&self, class: &str, text: &str, cause: &str) {
        let n = serde_json::json!({"class": class, "text": text, "cause": cause});
        self.shared.notices.lock().unwrap_or_else(PoisonError::into_inner).push(n);
    }

    /// `result` with this job's notices (an object result only; anything else as is).
    fn with_notices(&self, mut result: Value) -> Value {
        let notices = std::mem::take(&mut *self.shared.notices.lock().unwrap_or_else(PoisonError::into_inner));
        if !notices.is_empty()
            && let Some(o) = result.as_object_mut()
        {
            o.insert("notices".into(), Value::Array(notices));
        }
        result
    }

    /// Run `f` with an algorithm-level [`Interrupt`] whose progress maps onto `lo..hi` of this
    /// job (with `message`) and whose cancellation is this job's.
    pub fn stage<R>(&self, lo: f32, hi: f32, message: &str, f: impl FnOnce(&Interrupt) -> R) -> R {
        self.progress(lo, message);
        let cancel = || self.cancelled();
        let progress = |p: f32| self.progress(lo + (hi - lo) * p, "");
        f(&Interrupt::new(&cancel, &progress))
    }
}

/// Final state of a job.
#[derive(Clone, Debug, PartialEq)]
pub enum JobOutcome {
    /// Applied; the command's result.
    Done(Value),
    /// Failed (including a panic in the worker); the document is unchanged.
    Failed(String),
    /// Cancelled; the result (if any) was discarded and the document is unchanged.
    Cancelled,
}

/// A job that has ended, as reported by [`Session::poll_jobs`].
#[derive(Clone, Debug, PartialEq)]
pub struct JobEvent {
    pub id: JobId,
    pub command: String,
    pub label: String,
    pub document: Option<DocId>,
    pub outcome: JobOutcome,
    /// How long it ran, from start to end (ms).
    pub elapsed_ms: f64,
}

/// A job's state for the UI, the control channel and MCP (`jobs.list`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: JobId,
    pub command: String,
    /// Shown in the progress UI ("Gaussian Blur").
    pub label: String,
    /// The document the job edits (and locks), if any.
    pub document: Option<DocId>,
    /// `running`, `done`, `failed` or `cancelled`.
    pub state: &'static str,
    pub progress: f32,
    pub message: String,
    pub elapsed_ms: f64,
    /// The command's result (done) or the error (failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

type Payload = Box<dyn Any + Send>;
type Apply = Box<dyn FnOnce(&mut Session, Payload) -> Result<Value> + Send>;

/// Wall-clock milliseconds since some fixed point.
fn now_ms() -> f64 {
    static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(web_time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

struct Running {
    id: JobId,
    command: String,
    label: String,
    params: Value,
    journal: bool,
    /// The recording (`ActionState::take`) running when the job started, if any (set by
    /// `dispatch`, once recording resumes). Only that recording gets the job's step: one started
    /// later must not pick up an edit made before it.
    recorded_by: Option<u64>,
    pre_selected: Option<Vec<LayerId>>,
    doc: Option<DocId>,
    /// The document as the job saw it: applying fails if it was edited meanwhile.
    snapshot: Option<Weak<Document>>,
    ctx: JobCtx,
    started_ms: f64,
    coalesce: Option<String>,
    color_restrict: Option<usize>,
    filter_mask_layer: Option<LayerId>,
    /// The document's layer selection when the job started: applying targets it, not whatever
    /// got selected meanwhile (a Type click does that without editing the document, #7366).
    layers: Option<LayerSelection>,
    rx: std::sync::mpsc::Receiver<Result<Payload>>,
    apply: Option<Apply>,
    #[cfg(not(target_arch = "wasm32"))]
    handle: Option<std::thread::JoinHandle<()>>,
}

/// A document's active layer, selected layers and selection anchor.
#[derive(Clone, PartialEq)]
struct LayerSelection {
    active: Option<LayerId>,
    selected: Vec<LayerId>,
    anchor: Option<LayerId>,
}

impl LayerSelection {
    fn of(st: &crate::DocState) -> Self {
        Self { active: st.active_layer, selected: st.selected_layers.clone(), anchor: st.layer_anchor }
    }

    fn set(self, st: &mut crate::DocState) {
        st.active_layer = self.active;
        st.selected_layers = self.selected;
        st.layer_anchor = self.anchor;
        crate::channel_cmds::fix_view(st);
    }
}

impl Running {
    fn info(&self) -> JobInfo {
        JobInfo {
            id: self.id,
            command: self.command.clone(),
            label: self.label.clone(),
            document: self.doc,
            state: "running",
            progress: self.ctx.fraction(),
            message: self.ctx.message(),
            elapsed_ms: now_ms() - self.started_ms,
            result: None,
            error: None,
        }
    }
}

/// The session's job table (see the module docs).
#[derive(Default)]
pub struct Jobs {
    next: u64,
    /// Set while [`Session::start`] runs a command: [`run`] spawns instead of running inline.
    spawn: bool,
    /// The job a command registered while `spawn` was set (picked up by `start`).
    pending: Option<Running>,
    running: Vec<Running>,
    /// Ended jobs not yet collected by [`Session::poll_jobs`].
    events: Vec<JobEvent>,
    /// The last few ended jobs, for `jobs.list`.
    recent: Vec<JobInfo>,
    /// Workers of cancelled jobs that may still be unwinding (native).
    #[cfg(not(target_arch = "wasm32"))]
    draining: Vec<std::thread::JoinHandle<()>>,
    /// The context inline job work runs under (see [`Session::set_inline_job_ctx`]).
    inline: Option<JobCtx>,
    /// Jobs done a step at a time on this thread ([`Session::start_stepped_job`]).
    steppers: Vec<(JobId, Stepper)>,
}

/// One step of a [`Session::start_stepped_job`]: true once the job has delivered its result (or
/// was cancelled).
type Stepper = Box<dyn FnMut(&dyn Fn() -> bool) -> bool + Send>;

const RECENT: usize = 16;

impl Drop for Jobs {
    fn drop(&mut self) {
        // Workers own snapshots only; stop them so they don't burn CPU for a closed session.
        for r in self.running.iter().chain(self.pending.iter()) {
            r.ctx.cancel();
        }
    }
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs").field("running", &self.running.len()).finish()
    }
}

/// How [`Session::start`] ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Started {
    /// The command ran to completion inline (it isn't a job, or this is wasm).
    Done(Value),
    /// The command is running in the background.
    Job(JobId),
}

/// Commands that never edit the active document, allowed while a job runs on it.
fn independent_of_document(id: &str) -> bool {
    const PREFIXES: &[&str] = &["jobs.", "tools.", "view.", "window.", "help.", "brush.presets.", "gradient.presets.", "type.fonts."];
    const IDS: &[&str] = &["file.new", "file.close", "file.closeAll", "edit.preferences", "edit.colorSettings", "edit.keyboardShortcuts", "edit.menus"];
    PREFIXES.iter().any(|p| id.starts_with(p)) || IDS.contains(&id)
}

#[cfg(not(target_arch = "wasm32"))]
fn import_path(
    name: &str,
    path: &str,
    opts: &photocraft_io::ImportOptions,
    ctl: &photocraft_raster::Interrupt,
) -> std::result::Result<(photocraft_io::ImportResult, bool), photocraft_io::IoError> {
    // Bounded reads, and a clear error for a file larger than memory (#375); a PSD/PSB is
    // parsed from the open file instead of being read whole.
    photocraft_io::import_file_for_open_with(name, std::path::Path::new(path), opts, ctl)
}

#[cfg(target_arch = "wasm32")]
fn import_path(
    _: &str,
    path: &str,
    _: &photocraft_io::ImportOptions,
    _: &photocraft_raster::Interrupt,
) -> std::result::Result<(photocraft_io::ImportResult, bool), photocraft_io::IoError> {
    Err(photocraft_io::IoError::Read(format!("cannot open paths on the web: {path}")))
}

/// Whether [`Session::start`] can run work in the background: native threads, or the browser
/// threads build's rayon Web Worker pool. The single-threaded browser build runs it inline.
const SPAWNS: bool = cfg!(any(not(target_arch = "wasm32"), target_feature = "atomics"));

/// Run a job-capable command's heavy part. `work` runs on a worker thread when the command was
/// started with [`Session::start`] (inline otherwise, and always in the single-threaded browser
/// build; the browser threads build runs it on the rayon pool); `apply` then runs on
/// the calling thread with its output and returns the command's result. With `lock_document`,
/// the active document is locked against other edits while the job runs, and applying fails if
/// it was edited anyway.
///
/// `work` must only read what it captured (clone the document `Arc` first): it never sees the
/// session.
pub fn run<T: Send + 'static>(
    s: &mut Session,
    label: &str,
    lock_document: bool,
    work: impl FnOnce(&JobCtx) -> Result<T> + Send + 'static,
    apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    let authorization = s.authorize.or_else(|| RENDER_AUTHORIZATION.with(std::cell::Cell::get));
    let _authorization = AuthorizationScope::enter(authorization);
    // The job's document: stored paths it follows are resolved against it on any thread.
    let document = s.active().and_then(|d| d.path.as_deref()).map(std::path::PathBuf::from);
    let _document = crate::doc_path::scope(document.as_deref());
    if !s.jobs.spawn || !SPAWNS {
        let t = work(&s.jobs.inline.clone().unwrap_or_default())?;
        return apply(s, t);
    }
    if s.jobs.pending.is_some() {
        return Err(EngineError::Other(format!("`{label}` started a second background job in one command")));
    }
    let (doc, snapshot, layers) = match (lock_document, s.active()) {
        (true, Some(st)) => (Some(st.doc.id), Some(Arc::downgrade(&st.doc)), Some(LayerSelection::of(st))),
        (true, None) => return Err(EngineError::NoDocument),
        (false, _) => (None, None, None),
    };
    let ctx = JobCtx::new();
    let (tx, rx) = std::sync::mpsc::channel::<Result<Payload>>();
    let apply_document = document.clone();
    let erased: Apply = Box::new(move |s, any| {
        let _authorization = AuthorizationScope::enter(authorization);
        let _document = crate::doc_path::scope(apply_document.as_deref());
        let t = any.downcast::<T>().map_err(|_| EngineError::Other("internal error: job result type mismatch".into()))?;
        apply(s, *t)
    });
    s.jobs.next += 1;
    let id = JobId(s.jobs.next);
    #[cfg(not(target_arch = "wasm32"))]
    let handle = {
        let wctx = ctx.clone();
        let label_w = label.to_string();
        // The work closure is moved into the thread. If spawning fails (thread limits), there is
        // no way to get it back, so the error is reported instead.
        let spawned = std::thread::Builder::new().name(format!("photocraft-job-{}", id.0)).spawn(move || {
            let _authorization = AuthorizationScope::enter(authorization);
            let _document = crate::doc_path::scope(document.as_deref());
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&wctx).map(|t| Box::new(t) as Payload)))
                .unwrap_or_else(|_| Err(EngineError::Other(format!("`{label_w}` failed with an internal error (logged); the document is unchanged"))));
            // The receiver is gone when the job was cancelled: nothing to report.
            let _ = tx.send(r);
        });
        Some(spawned.map_err(|e| EngineError::Other(format!("could not start a background job: {e}")))?)
    };
    // The browser threads build: a worker of the rayon pool (no handle to join; a cancelled
    // job's result is dropped with its channel).
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    {
        let wctx = ctx.clone();
        rayon::spawn(move || {
            let _ = tx.send(work(&wctx).map(|t| Box::new(t) as Payload));
        });
    }
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    {
        // Unreachable (the single-threaded build runs inline above); keep the types used.
        drop((work, tx));
    }
    s.jobs.pending = Some(Running {
        id,
        command: String::new(),
        label: label.to_string(),
        params: Value::Null,
        journal: true,
        recorded_by: None,
        pre_selected: None,
        doc,
        snapshot,
        ctx,
        started_ms: now_ms(),
        coalesce: s.coalesce_request.clone(),
        color_restrict: s.color_restrict,
        filter_mask_layer: s.filter_mask_layer,
        layers,
        rx,
        apply: Some(erased),
        #[cfg(not(target_arch = "wasm32"))]
        handle,
    });
    Ok(json!({"job": id.0}))
}

/// Delivers the result of an [`run_async`] job's work. Call [`Completion::finish`] once, from
/// any thread or browser event; dropping it unfinished fails the job ("stopped unexpectedly").
pub struct Completion<T> {
    tx: std::sync::mpsc::Sender<Result<Payload>>,
    _t: std::marker::PhantomData<fn(T)>,
}

impl<T: Send + 'static> Completion<T> {
    pub fn finish(self, r: Result<T>) {
        // The receiver is gone when the job was cancelled: nothing to report.
        let _ = self.tx.send(r.map(|t| Box::new(t) as Payload));
    }
}

/// [`run`] for work that completes asynchronously: `work` starts it and hands the
/// [`Completion`] to whatever finishes it (the AI Web Worker's reply in the browser, which can't
/// block on WebGPU). `apply` then runs on the UI thread as for [`run`].
///
/// Natively, started with [`Session::start`], `work` runs on a worker thread; run inline it is
/// waited for. In the browser it is always a background job, even from [`Session::execute`]
/// (which then returns `{"job": id, "pending": true}`): `work` must return at once there.
pub fn run_async<T: Send + 'static>(
    s: &mut Session,
    label: &str,
    lock_document: bool,
    work: impl FnOnce(&JobCtx, Completion<T>) + Send + 'static,
    apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<Payload>>();
    let done = Completion { tx, _t: std::marker::PhantomData };
    #[cfg(not(target_arch = "wasm32"))]
    if !s.jobs.spawn {
        let ctx = s.jobs.inline.clone().unwrap_or_default();
        work(&ctx, done);
        let r = rx.recv().unwrap_or_else(|_| Err(EngineError::Other(format!("`{label}` stopped unexpectedly"))))?;
        let t = r.downcast::<T>().map_err(|_| EngineError::Other("internal error: job result type mismatch".into()))?;
        return apply(s, *t).map(|v| ctx.with_notices(v));
    }
    if s.jobs.pending.is_some() {
        return Err(EngineError::Other(format!("`{label}` started a second background job in one command")));
    }
    let (doc, snapshot, layers) = match (lock_document, s.active()) {
        (true, Some(st)) => (Some(st.doc.id), Some(Arc::downgrade(&st.doc)), Some(LayerSelection::of(st))),
        (true, None) => return Err(EngineError::NoDocument),
        (false, _) => (None, None, None),
    };
    let ctx = JobCtx::new();
    let erased: Apply = Box::new(move |s, any| {
        let t = any.downcast::<T>().map_err(|_| EngineError::Other("internal error: job result type mismatch".into()))?;
        apply(s, *t)
    });
    s.jobs.next += 1;
    let id = JobId(s.jobs.next);
    #[cfg(not(target_arch = "wasm32"))]
    let handle = {
        let wctx = ctx.clone();
        let fail = done.tx.clone();
        let label_w = label.to_string();
        let spawned = std::thread::Builder::new().name(format!("photocraft-job-{}", id.0)).spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&wctx, done))).is_err() {
                let _ = fail.send(Err(EngineError::Other(format!("`{label_w}` failed with an internal error (logged); the document is unchanged"))));
            }
        });
        Some(spawned.map_err(|e| EngineError::Other(format!("could not start a background job: {e}")))?)
    };
    #[cfg(target_arch = "wasm32")]
    work(&ctx, done);
    s.jobs.pending = Some(Running {
        id,
        command: String::new(),
        label: label.to_string(),
        params: Value::Null,
        journal: true,
        recorded_by: None,
        pre_selected: None,
        doc,
        snapshot,
        ctx,
        started_ms: now_ms(),
        coalesce: s.coalesce_request.clone(),
        color_restrict: s.color_restrict,
        filter_mask_layer: s.filter_mask_layer,
        layers,
        rx,
        apply: Some(erased),
        #[cfg(not(target_arch = "wasm32"))]
        handle,
    });
    Ok(json!({"job": id.0}))
}

/// Whether [`run_async_prepared`] reads its input on the pool and hands it back to the page's
/// thread: the browser threads build, where `run_async`'s work runs on the page's thread.
const HAND_OFF: bool = cfg!(all(target_arch = "wasm32", target_feature = "atomics"));

/// Whether [`run_async_prepared`] reads its input a part at a time between frames: the
/// single-threaded browser build, which has no pool to read it on.
const STEPPED_PREP: bool = cfg!(all(target_arch = "wasm32", not(target_feature = "atomics")));

#[cfg(test)]
thread_local! {
    /// Tests take the browser threads build's hand-off path natively.
    pub(crate) static TEST_HAND_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests take the single-threaded browser build's stepped path natively.
    pub(crate) static TEST_STEPPED_PREP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The rest of a [`run_async_prepared`] job's input reading: a part of it, which returns the next
/// one, or the input. The single-threaded browser build runs one part per step between frames
/// (each part well under a frame budget on a phone); elsewhere the parts run one after the other.
pub enum Prep<P> {
    /// The next part.
    Then(PrepPart<P>),
    /// Read.
    Done(P),
}

/// A part of a [`Prep`].
pub type PrepPart<P> = Box<dyn FnOnce(&JobCtx) -> Result<Prep<P>> + Send>;

impl<P> Prep<P> {
    /// A part that runs `f` next.
    pub fn then(f: impl FnOnce(&JobCtx) -> Result<Prep<P>> + Send + 'static) -> Self {
        Self::Then(Box::new(f))
    }

    /// Every remaining part, at once.
    pub fn finish(self, ctx: &JobCtx) -> Result<P> {
        let mut p = self;
        loop {
            match p {
                Self::Done(v) => return Ok(v),
                Self::Then(f) => {
                    ctx.check()?;
                    p = f(ctx)?;
                }
            }
        }
    }
}

/// [`run_async`] whose input is read first by `prep` (the window's pixels, the hole: seconds
/// for a big stroke on a phone), as parts ([`Prep`]): on the rayon pool in the browser threads
/// build, which then hands `prep`'s output to `work` on the page's thread (`work` talks to the AI
/// Web Worker, which only the page's thread may), between frames ([`Session::step_jobs`]); in the
/// single-threaded browser build one part per step, between frames, so no part holds the page
/// for long. Elsewhere `prep` runs where `work` does (a worker thread natively, inline when run
/// inline). An error from `prep` fails the job.
pub fn run_async_prepared<P: Send + 'static, T: Send + 'static>(
    s: &mut Session,
    label: &str,
    lock_document: bool,
    prep: impl FnOnce(&JobCtx) -> Result<Prep<P>> + Send + 'static,
    work: impl FnOnce(&JobCtx, P, Completion<T>) + Send + 'static,
    apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    #[cfg(test)]
    let (hand_off, stepped) = (HAND_OFF || TEST_HAND_OFF.with(std::cell::Cell::get), STEPPED_PREP || TEST_STEPPED_PREP.with(std::cell::Cell::get));
    #[cfg(not(test))]
    let (hand_off, stepped) = (HAND_OFF, STEPPED_PREP);
    // In the browser `run_async` is always a background job; natively only when started.
    let background = cfg!(target_arch = "wasm32") || s.jobs.spawn;
    if stepped && background {
        return run_async_stepped(s, label, lock_document, prep, work, apply);
    }
    if !(hand_off && s.jobs.spawn) {
        return run_async(
            s,
            label,
            lock_document,
            move |ctx, done| match prep(ctx).and_then(|p| p.finish(ctx)) {
                Ok(p) => work(ctx, p, done),
                Err(e) => done.finish(Err(e)),
            },
            apply,
        );
    }
    type Slot<X> = Arc<wasm_sync::Mutex<Option<X>>>;
    let held: Slot<(JobCtx, Completion<T>)> = Arc::default();
    let ready: Slot<Result<P>> = Arc::default();
    let (put_held, put_ready) = (Arc::clone(&held), Arc::clone(&ready));
    let label_w = label.to_string();
    let v = run_async(
        s,
        label,
        lock_document,
        move |ctx, done| {
            *put_held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((ctx.clone(), done));
            let ctx = ctx.clone();
            on_pool(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prep(&ctx).and_then(|p| p.finish(&ctx))))
                    .unwrap_or_else(|_| Err(EngineError::Other(format!("`{label_w}` failed with an internal error (logged); the document is unchanged"))));
                *put_ready.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
            });
        },
        apply,
    )?;
    let Some(id) = s.jobs.pending.as_ref().map(|r| r.id) else { return Ok(v) };
    let mut work = Some(work);
    let stepper: Stepper = Box::new(move |_more| {
        let mut h = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Not handed over yet (the work starts on a thread natively): try again next time.
        let Some((ctx, _)) = h.as_ref() else { return false };
        if ctx.cancelled() {
            return true;
        }
        let Some(r) = ready.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else { return false };
        let (Some((ctx, done)), Some(work)) = (h.take(), work.take()) else { return true };
        match r {
            Ok(p) => work(&ctx, p, done),
            Err(e) => done.finish(Err(e)),
        }
        true
    });
    s.jobs.steppers.push((id, stepper));
    Ok(v)
}

/// [`run_async_prepared`] in the single-threaded browser build: `prep`'s parts run on this
/// thread, one per step between frames while the step has time (at least one), then `work`.
fn run_async_stepped<P: Send + 'static, T: Send + 'static>(
    s: &mut Session,
    label: &str,
    lock_document: bool,
    prep: impl FnOnce(&JobCtx) -> Result<Prep<P>> + Send + 'static,
    work: impl FnOnce(&JobCtx, P, Completion<T>) + Send + 'static,
    apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
) -> Result<Value> {
    type Slot<X> = Arc<wasm_sync::Mutex<Option<X>>>;
    let held: Slot<(JobCtx, Completion<T>)> = Arc::default();
    let put_held = Arc::clone(&held);
    let v = run_async(
        s,
        label,
        lock_document,
        move |ctx, done| *put_held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((ctx.clone(), done)),
        apply,
    )?;
    let Some(id) = s.jobs.pending.as_ref().map(|r| r.id) else { return Ok(v) };
    let mut next = Some(Prep::then(prep));
    let mut work = Some(work);
    let failed = format!("`{label}` failed with an internal error (logged); the document is unchanged");
    let stepper: Stepper = Box::new(move |more| {
        let mut h = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Not handed over yet (the work starts on a thread natively): try again next time.
        let Some((ctx, _)) = h.as_ref() else { return false };
        if ctx.cancelled() {
            return true;
        }
        let ctx = ctx.clone();
        loop {
            match next.take() {
                Some(Prep::Then(f)) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&ctx))) {
                    Ok(Ok(p)) => next = Some(p),
                    Ok(Err(e)) => {
                        if let Some((_, done)) = h.take() {
                            done.finish(Err(e));
                        }
                        return true;
                    }
                    Err(_) => {
                        if let Some((_, done)) = h.take() {
                            done.finish(Err(EngineError::Other(failed.clone())));
                        }
                        return true;
                    }
                },
                Some(Prep::Done(p)) => {
                    if let (Some((ctx, done)), Some(work)) = (h.take(), work.take()) {
                        work(&ctx, p, done);
                    }
                    return true;
                }
                None => return true,
            }
            if !more() {
                return false;
            }
        }
    });
    s.jobs.steppers.push((id, stepper));
    Ok(v)
}

/// Runs `f` on the rayon pool in the browser threads build; in the single-threaded one later, by
/// itself between frames ([`Session::step_jobs`]), not in the task that asked (an AI worker's
/// reply); else here (tests: the hand-off still waits for the next step). `f` must not touch the
/// browser's APIs.
pub(crate) fn on_pool(f: impl FnOnce() + Send + 'static) {
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    rayon::spawn(f);
    #[cfg(not(all(target_arch = "wasm32", target_feature = "atomics")))]
    {
        #[cfg(test)]
        let defer = STEPPED_PREP || TEST_STEPPED_PREP.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let defer = STEPPED_PREP;
        if defer {
            DEFERRED.with(|d| d.borrow_mut().push_back(Box::new(f)));
        } else {
            f();
        }
    }
}

thread_local! {
    /// Work [`on_pool`] put off until the next step between frames (single-threaded browser build).
    static DEFERRED: std::cell::RefCell<std::collections::VecDeque<Box<dyn FnOnce()>>> = std::cell::RefCell::default();
}

/// Runs the work [`on_pool`] put off, one at a time while `more` (at least one). True if any is
/// left.
fn run_deferred(more: &dyn Fn() -> bool) -> bool {
    loop {
        let Some(f) = DEFERRED.with(|d| d.borrow_mut().pop_front()) else { return false };
        f();
        if !more() {
            return DEFERRED.with(|d| !d.borrow().is_empty());
        }
    }
}

/// A document edit whose body is the job: like [`Session::edit`] (one undo step named `label`),
/// but `f` runs on a worker against a copy of the active document when started in the
/// background. `finish` turns `f`'s output into the command's result.
pub fn edit_job<R: Send + 'static>(
    s: &mut Session,
    label: &str,
    f: impl FnOnce(&mut Document, &mut Option<LayerId>, &JobCtx) -> Result<R> + Send + 'static,
    finish: impl FnOnce(R) -> Value + Send + 'static,
) -> Result<Value> {
    if !s.jobs.spawn || !SPAWNS {
        // Inline: exactly `Session::edit`, as before jobs existed.
        let ctx = s.jobs.inline.clone().unwrap_or_default();
        let r = s.edit(label, |doc, active| f(doc, active, &ctx))?;
        return Ok(finish(r));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (base, active) = (st.doc.clone(), st.active_layer);
    let label_owned = label.to_string();
    run(
        s,
        label,
        true,
        move |ctx| {
            let mut doc = (*base).clone();
            let mut active = active;
            let r = f(&mut doc, &mut active, ctx)?;
            Ok((doc, active, r))
        },
        move |s, (doc, new_active, r)| {
            s.edit(&label_owned, move |d, active| {
                *d = doc;
                *active = new_active;
                Ok(())
            })?;
            Ok(finish(r))
        },
    )
}

/// Scan every layer surface's content bounds into the shared tile bounds cache, so the first
/// draw of a document opened in the background doesn't do it on the UI thread: GPU planning
/// asks each layer for its bounds, a multi-second scan on a multi-gigabyte PSB (#4024).
/// `check` runs between layers (cancellation).
pub fn warm_layer_bounds(doc: &Document, mut check: impl FnMut() -> Result<()>) -> Result<()> {
    let mut pending: Vec<&photocraft_doc::Layer> = doc.layers.iter().collect();
    while let Some(layer) = pending.pop() {
        check()?;
        if let Some(children) = layer.children() {
            pending.extend(children);
        }
        if let Some(surface) = layer.surface() {
            photocraft_compose::bounds::content_bounds(surface);
        }
    }
    Ok(())
}

/// A PSD/PSB's merged image, reduced ([`open_preview`]).
pub use photocraft_psd::preview::Preview as OpenPreview;

/// Where an open puts its PSD/PSB's merged image, reduced, for the shell to show meanwhile.
pub type PreviewSlot = Arc<wasm_sync::Mutex<Option<OpenPreview>>>;

/// The longest side of an open's preview at most: about a page at fit on a large screen (it shows
/// for a moment; the fewer rows it reads the sooner it does).
pub const PREVIEW_SIDE: u32 = 1280;

fn put_preview(slot: &PreviewSlot, p: Option<OpenPreview>) {
    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = p;
}

/// The merged-image preview of the file `source` holds, if it is a PSD/PSB with one that can
/// stand for it ([`photocraft_psd::preview::merged_preview`]), in sRGB: an RGB file with another
/// embedded profile is converted through it.
pub fn open_preview(source: &OpenSource) -> Option<OpenPreview> {
    let mut p = match source {
        OpenSource::Bytes(b) => photocraft_io::is_psd(b).then(|| photocraft_psd::preview::merged_preview(&**b, PREVIEW_SIDE)).flatten(),
        #[cfg(not(target_arch = "wasm32"))]
        OpenSource::Path(path) => {
            let f = photocraft_psd::FileSource::open(std::path::Path::new(path)).ok()?;
            photocraft_psd::preview::merged_preview(&f, PREVIEW_SIDE)
        }
        #[cfg(target_arch = "wasm32")]
        OpenSource::Path(_) => None,
    }?;
    if !p.gray
        && let Some(icc) = &p.icc
    {
        // A profile that doesn't parse is the document's problem; its preview is shown as sRGB,
        // as the document will be.
        if let Ok(src) = photocraft_cms::Profile::parse(icc)
            && src.color_space == photocraft_cms::ColorSpace::Rgb
        {
            let dst = photocraft_cms::Builtin::Srgb.profile();
            if !src.same_colors(dst) {
                let t = photocraft_cms::Transform::new(&src, dst, photocraft_cms::Intent::RelativeColorimetric, true).ok()?;
                let mut px: Vec<f32> = p.rgba.iter().map(|&v| f32::from(v) / 255.0).collect();
                t.apply(&mut px, 4);
                for (o, v) in p.rgba.iter_mut().zip(px) {
                    *o = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
    Some(p)
}

fn open_error(e: photocraft_io::IoError) -> EngineError {
    match e {
        photocraft_io::IoError::Cancelled => EngineError::Cancelled,
        e => EngineError::Other(e.to_string()),
    }
}

/// What an open does with a decoded file before the document is shown: the layers' bounds and,
/// for type, the fonts (see [`Session::start_open`]).
fn prepare_opened(ctx: &JobCtx, imported: (photocraft_io::ImportResult, bool)) -> Result<(photocraft_io::ImportResult, bool)> {
    ctx.progress(0.95, "Preparing layers");
    warm_layer_bounds(&imported.0.document, || ctx.check())?;
    // Type layers need the shared text engine, whose first use waits for the
    // installed-font scan started at launch (minutes on some machines). Wait here,
    // so the shell's missing-font check after the open finds it ready instead of
    // freezing the window (#7332).
    if crate::type_extra_cmds::has_type_layers(&imported.0.document) {
        ctx.progress(0.97, "Loading fonts");
        let _ = photocraft_text::shared();
        ctx.check()?;
    }
    Ok(imported)
}

/// An open job's last part, on the session's thread: the document joins the session (or is held
/// for the Color Settings or damage prompts); see [`Session::start_open`]. A `hidden` open adds a
/// hidden helper document, the Color Settings policy applied without asking
/// ([`Session::start_open_hidden`]).
fn open_apply(
    name: &str,
    confirm_damage: bool,
    hidden: bool,
) -> impl FnOnce(&mut Session, (photocraft_io::ImportResult, bool)) -> Result<Value> + Send + 'static {
    let name_a = name.to_string();
    move |s, (mut r, damaged)| {
        // File Handling's ACEScg option, before the colour prompt looks at the profile.
        crate::file_cmds::tag_exr_colorspace(s, &name_a, &mut r.document);
        crate::file_cmds::raw_defaults(s, &r.warnings, &mut r.document);
        if hidden {
            let (index, _) = s.open_hidden_document(r.document, None);
            let id = s.documents().get(index).ok_or(EngineError::NoDocument)?.doc.id;
            return Ok(json!({"document": index, "docId": id.0, "name": name_a, "warnings": r.warnings, "hidden": true}));
        }
        // Ask When Opening: held back until the prompt is answered (`take_held_open`). A
        // developed raw has no embedded profile to ask about.
        let raw = r.warnings.iter().any(|w| w.contains(photocraft_io::raw::DEVELOPED_NOTE));
        let damaged = confirm_damage && damaged;
        let question = s.color.open_question(&r.document).filter(|_| !raw);
        if damaged || question.is_some() {
            let held = s.hold_open(r.document, r.source_read_only);
            return Ok(json!({"held": held, "name": name_a, "warnings": r.warnings, "color": question, "damaged": damaged}));
        }
        let (index, color) = s.open_document(r.document, None);
        // Persisted IDs can collide: report the identity the session actually admitted.
        let id = s.documents().get(index).ok_or(EngineError::NoDocument)?.doc.id;
        if let Some(st) = s.active_mut() {
            st.source_read_only = r.source_read_only;
        }
        Ok(json!({"document": index, "docId": id.0, "name": name_a, "warnings": r.warnings, "color": color}))
    }
}

/// Where a background open reads its file from.
#[derive(Clone, Debug)]
pub enum OpenSource {
    /// File contents already in memory (web pickers, drops, automation).
    Bytes(Arc<Vec<u8>>),
    /// A path the worker reads itself (native), so even the read stays off the UI thread.
    Path(String),
}

/// The command id open jobs report (there is no engine command: opening is the shell's).
pub const OPEN_JOB: &str = "file.open";

/// The import settings files are opened and placed with.
pub fn import_options(prefs: &crate::prefs::Preferences) -> photocraft_io::ImportOptions {
    photocraft_io::ImportOptions {
        max_group_depth: prefs.file_handling.rasterize_svg_groups_deeper_than as usize,
        demosaic: prefs.raw_defaults.demosaic.method(),
    }
}

impl Session {
    /// Run the heavy part of inline (not background) job commands under `ctx`, so another thread
    /// can cancel them: a live preview whose parameters changed stops its stale computation
    /// (`Err(Cancelled)`, the document unchanged) instead of finishing it. `None` restores the
    /// default, a context nothing can cancel.
    pub fn set_inline_job_ctx(&mut self, ctx: Option<JobCtx>) {
        self.jobs.inline = ctx;
    }

    /// Run a command, in the background when it supports it (see [`jobs`](crate::jobs)). Errors
    /// like [`Session::execute`] for unknown, disabled or failing commands.
    pub fn start(&mut self, id: &str, params: Value) -> Result<Started> {
        self.dispatch(id, params, true)
    }

    /// Open a file in the background: read (for a path) and decode on a worker, with progress
    /// (per layer for PSD/PSB) and cancellation, then add the document like
    /// [`Session::open_document`] (Color Settings policies apply). `name` names the document.
    /// The job's result is `{document, docId, name, warnings, color}`; on wasm it runs inline.
    /// `document` is the tab index at apply time; resolve `docId` before handling a delayed
    /// completion, since earlier handlers may have moved or closed tabs. When
    /// Color Settings ask about the file's profile, the document is held back instead and the
    /// result is `{held, name, warnings, color}` (`color` the question): take it with
    /// [`Session::take_held_open`] once answered.
    pub fn start_open(&mut self, name: &str, source: OpenSource) -> Result<Started> {
        self.start_open_inner(name, source, false, None, false)
    }

    /// [`Session::start_open`] of a hidden helper document ([`Session::open_hidden_document`]):
    /// nothing is shown or made active, the Color Settings policy applies without asking and a
    /// damaged PSD opens as read. In the single-threaded browser build a PSD/PSB opens a step at a
    /// time between frames (no merged preview: nothing shows it). The result adds `hidden: true`.
    pub fn start_open_hidden(&mut self, name: &str, source: OpenSource) -> Result<Started> {
        if !SPAWNS && matches!(&source, OpenSource::Bytes(b) if photocraft_io::is_psd(b)) {
            return self.start_stepped_open_inner(name, source, None, true);
        }
        self.start_open_inner(name, source, false, None, true)
    }

    /// Interactive opening: hold a damaged PSD until the user answers a damage
    /// confirmation, without changing import or subsequent save behaviour.
    /// Headless callers keep using [`Self::start_open`].
    pub fn start_open_for_user(&mut self, name: &str, source: OpenSource) -> Result<Started> {
        self.start_open_inner(name, source, true, None, false)
    }

    /// [`Session::start_open_for_user`] that first puts a PSD/PSB's merged image, reduced, in
    /// `preview` (read straight from the file, before the layers are built), so the shell can
    /// show the page while it opens. In the single-threaded browser build a PSD/PSB opens a step
    /// at a time between frames ([`Self::start_stepped_open`]).
    pub fn start_open_with_preview(&mut self, name: &str, source: OpenSource, preview: PreviewSlot) -> Result<Started> {
        if !SPAWNS && matches!(&source, OpenSource::Bytes(b) if photocraft_io::is_psd(b)) {
            return self.start_stepped_open(name, source, preview);
        }
        self.start_open_inner(name, source, true, Some(preview), false)
    }

    /// Decodes a file in a job and optionally holds damaged PSD content for
    /// interactive confirmation while leaving headless opens non-interactive.
    fn start_open_inner(&mut self, name: &str, source: OpenSource, confirm_damage: bool, preview: Option<PreviewSlot>, hidden: bool) -> Result<Started> {
        let label = format!("Opening {name}");
        let name_w = name.to_string();
        let opts = import_options(self.prefs());
        self.start_job(
            OPEN_JOB,
            json!({"name": name}),
            &label,
            false,
            move |ctx| {
                ctx.progress(0.0, "Reading");
                // The merged image is read beside the layers (on the pool), not before them.
                if let Some(slot) = preview {
                    if SPAWNS {
                        let source = source.clone();
                        rayon::spawn(move || put_preview(&slot, open_preview(&source)));
                    } else {
                        put_preview(&slot, open_preview(&source));
                    }
                }
                let imported = match source {
                    OpenSource::Bytes(bytes) => {
                        ctx.check()?;
                        ctx.progress(0.02, "Decoding");
                        ctx.stage(0.02, 0.95, "Decoding", |ctl| photocraft_io::import_shared_for_open_with(&name_w, &bytes, &opts, ctl))
                    }
                    // A PSD/PSB is decoded straight from the file, which is never read whole.
                    OpenSource::Path(p) => ctx.stage(0.0, 0.95, "Decoding", |ctl| import_path(&name_w, &p, &opts, ctl)),
                }
                .map_err(open_error)?;
                prepare_opened(ctx, imported)
            },
            open_apply(name, confirm_damage, hidden),
        )
    }

    /// [`Session::start_open_with_preview`] for a PSD/PSB in `source`'s bytes, done a step at a
    /// time on this thread ([`Self::start_stepped_job`]): the merged image first (into `preview`,
    /// shown while the rest goes on), then the parse, the layers' pixels a band at a time, and the
    /// document. The single-threaded browser build opens so, keeping the page responsive.
    pub fn start_stepped_open(&mut self, name: &str, source: OpenSource, preview: PreviewSlot) -> Result<Started> {
        self.start_stepped_open_inner(name, source, Some(preview), false)
    }

    /// [`Self::start_stepped_open`]; no merged preview read without a `preview` slot, a `hidden`
    /// helper document ([`Self::start_open_hidden`]).
    fn start_stepped_open_inner(&mut self, name: &str, source: OpenSource, preview: Option<PreviewSlot>, hidden: bool) -> Result<Started> {
        enum Stage {
            Preview(Arc<Vec<u8>>),
            Parse(Arc<Vec<u8>>),
            Decode(Box<photocraft_io::PsdOpen>),
            Prepare(Box<photocraft_io::PsdBuilt>),
            Done,
        }
        let OpenSource::Bytes(bytes) = source else {
            return self.start_open_inner(name, source, !hidden, preview, hidden);
        };
        let label = format!("Opening {name}");
        let name_w = name.to_string();
        let mut stage = Stage::Preview(bytes);
        self.start_stepped_job(
            OPEN_JOB,
            json!({"name": name}),
            &label,
            move |ctx, more| {
                loop {
                    stage = match std::mem::replace(&mut stage, Stage::Done) {
                        Stage::Preview(bytes) => {
                            ctx.progress(0.0, "Reading");
                            match &preview {
                                None => Stage::Parse(bytes),
                                Some(slot) => {
                                    put_preview(slot, open_preview(&OpenSource::Bytes(Arc::clone(&bytes))));
                                    // Shown before anything else is done.
                                    stage = Stage::Parse(bytes);
                                    return None;
                                }
                            }
                        }
                        Stage::Parse(bytes) => {
                            ctx.progress(0.02, "Decoding");
                            match photocraft_io::PsdOpen::shared(&name_w, &bytes) {
                                Some(Ok(open)) => Stage::Decode(Box::new(open)),
                                Some(Err(e)) => return Some(Err(open_error(e))),
                                None => return Some(Err(EngineError::Other(format!("{name_w} is not a PSD/PSB file")))),
                            }
                        }
                        Stage::Decode(mut open) => {
                            if !open.decoded() {
                                open.step(more);
                                ctx.progress(0.05 + 0.8 * open.progress(), "Decoding");
                                stage = Stage::Decode(open);
                                return None;
                            }
                            // The layers, their type and the rest each get a step (a frame).
                            match ctx.stage(0.85, 0.9, "Decoding", |ctl| open.build(ctl)) {
                                Ok(built) => {
                                    stage = Stage::Prepare(Box::new(built));
                                    return None;
                                }
                                Err(e) => return Some(Err(open_error(e))),
                            }
                        }
                        Stage::Prepare(mut built) => {
                            ctx.progress(0.9, "Preparing layers");
                            if !built.prepare(more) {
                                stage = Stage::Prepare(built);
                                return None;
                            }
                            return Some(prepare_opened(ctx, built.finish()));
                        }
                        Stage::Done => return Some(Err(EngineError::Other("the open already ended".into()))),
                    };
                    if !more() {
                        return None;
                    }
                }
            },
            open_apply(name, !hidden, hidden),
        )
    }

    /// [`Session::start_open`] for deep EXRs: the document keeps its depth data as a deep
    /// layer (`io::import_deep_exr`) instead of opening flattened.
    pub fn start_open_deep(&mut self, name: &str, source: OpenSource) -> Result<Started> {
        let label = format!("Opening {name}");
        let name_w = name.to_string();
        let name_a = name.to_string();
        self.start_job(
            OPEN_JOB,
            json!({"name": name, "deep": true}),
            &label,
            false,
            move |ctx| {
                ctx.progress(0.0, "Reading");
                let bytes = match source {
                    OpenSource::Bytes(b) => b,
                    OpenSource::Path(p) => Arc::new(read_path(&p)?),
                };
                ctx.check()?;
                ctx.progress(0.02, "Decoding");
                photocraft_io::import_deep_exr(&name_w, &bytes, &photocraft_codecs::Limits::default()).map_err(|e| match e {
                    photocraft_io::IoError::Cancelled => EngineError::Cancelled,
                    e => EngineError::Other(e.to_string()),
                })
            },
            move |s, r: photocraft_io::ImportResult| {
                let (index, color) = s.open_document(r.document, None);
                Ok(json!({"document": index, "name": name_a, "warnings": r.warnings, "color": color}))
            },
        )
    }

    /// Start a job that isn't an engine command (opening a file, a shell operation, a test's fake
    /// job): `work` runs on a worker, `apply` on this thread when [`Session::poll_jobs`] or
    /// [`Session::wait_job`] picks the result up. `command` and `params` identify it in
    /// [`JobInfo`]; it is not journaled. With `lock_document` the active document is locked as
    /// for a command. Runs inline in the single-threaded browser build, on the rayon Web Worker
    /// pool in the threads build.
    pub fn start_job<T: Send + 'static>(
        &mut self,
        command: &str,
        params: Value,
        label: &str,
        lock_document: bool,
        work: impl FnOnce(&JobCtx) -> Result<T> + Send + 'static,
        apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
    ) -> Result<Started> {
        self.jobs.spawn = true;
        let r = run(self, label, lock_document, work, apply);
        self.jobs.spawn = false;
        let pending = self.jobs.pending.take();
        match (r, pending) {
            (Err(e), Some(job)) => {
                job.ctx.cancel();
                Err(e)
            }
            (Err(e), None) => Err(self.explain_missing_layer(e)),
            (Ok(_), Some(mut job)) => {
                job.command = command.to_string();
                job.params = params;
                job.journal = false;
                let id = job.id;
                self.jobs.running.push(job);
                Ok(Started::Job(id))
            }
            (Ok(v), None) => Ok(Started::Done(v)),
        }
    }

    /// [`Self::start_job`] for work that completes asynchronously ([`run_async`]): `work` starts it
    /// and hands the [`Completion`] to whatever finishes it (a browser event). Not journaled, no
    /// document locked; a job card with progress and Esc to cancel like any other.
    pub fn start_async_job<T: Send + 'static>(
        &mut self,
        command: &str,
        params: Value,
        label: &str,
        work: impl FnOnce(&JobCtx, Completion<T>) + Send + 'static,
        apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
    ) -> Result<Started> {
        self.jobs.spawn = true;
        let r = run_async(self, label, false, work, apply);
        self.jobs.spawn = false;
        let pending = self.jobs.pending.take();
        match (r, pending) {
            (Err(e), Some(job)) => {
                job.ctx.cancel();
                Err(e)
            }
            (Err(e), None) => Err(e),
            (Ok(_), Some(mut job)) => {
                job.command = command.to_string();
                job.params = params;
                job.journal = false;
                let id = job.id;
                self.jobs.running.push(job);
                Ok(Started::Job(id))
            }
            (Ok(v), None) => Ok(Started::Done(v)),
        }
    }

    /// [`Self::start_job`] for work done a step at a time on this thread, between frames: the
    /// single-threaded browser build, where a job would otherwise hold the page until it is done.
    /// [`Self::step_jobs`] (the shell calls it every frame) calls `step` until it returns the
    /// result; `step` does a little and returns `None` while its `more` says there is time left.
    /// A job card with progress and Esc to cancel like any other; not journaled, no document
    /// locked. [`Self::wait_job`] runs it to the end.
    pub fn start_stepped_job<T: Send + 'static>(
        &mut self,
        command: &str,
        params: Value,
        label: &str,
        mut step: impl FnMut(&JobCtx, &dyn Fn() -> bool) -> Option<Result<T>> + Send + 'static,
        apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
    ) -> Result<Started> {
        type Slot<T> = Arc<wasm_sync::Mutex<Option<(JobCtx, Completion<T>)>>>;
        let slot: Slot<T> = Arc::default();
        let put = Arc::clone(&slot);
        let started = self.start_async_job(
            command,
            params,
            label,
            move |ctx, done| *put.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((ctx.clone(), done)),
            apply,
        )?;
        let Started::Job(id) = started else { return Ok(started) };
        let stepper: Stepper = Box::new(move |more| {
            let mut held = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Not handed over yet (the work starts on a thread natively): try again next time.
            let Some((ctx, _)) = held.as_ref() else { return false };
            if ctx.cancelled() {
                return true;
            }
            let ctx = ctx.clone();
            match step(&ctx, more) {
                None => false,
                Some(r) => {
                    if let Some((_, done)) = held.take() {
                        done.finish(r);
                    }
                    true
                }
            }
        });
        self.jobs.steppers.push((id, stepper));
        Ok(Started::Job(id))
    }

    /// Advance the jobs done a step at a time ([`Self::start_stepped_job`]) while `more` says
    /// there is time (each does at least a step). Their results are applied by
    /// [`Self::poll_jobs`].
    pub fn step_jobs(&mut self, more: &dyn Fn() -> bool) {
        // Work put off by a job (an AI fill's encode, see `on_pool`) first, then a step of each.
        if run_deferred(more) {
            return;
        }
        let running: Vec<JobId> = self.jobs.running.iter().map(|r| r.id).collect();
        self.jobs.steppers.retain_mut(|(id, step)| running.contains(id) && !step(more));
    }

    /// Whether any job is done a step at a time by [`Self::step_jobs`].
    pub fn has_stepped_jobs(&self) -> bool {
        !self.jobs.steppers.is_empty() || DEFERRED.with(|d| !d.borrow().is_empty())
    }

    /// [`Self::start_job`] that runs `work` off the page's thread in the browser threads build
    /// too (on the rayon Web Worker pool), so a long job (a save) never freezes the page. `work`
    /// must not touch the browser's APIs, which only the page's thread may call. Inline in the
    /// single-threaded browser build, a worker thread natively, as `start_job`.
    pub fn start_job_in_worker<T: Send + 'static>(
        &mut self,
        command: &str,
        params: Value,
        label: &str,
        lock_document: bool,
        work: impl FnOnce(&JobCtx) -> Result<T> + Send + 'static,
        apply: impl FnOnce(&mut Session, T) -> Result<Value> + Send + 'static,
    ) -> Result<Started> {
        self.start_job(command, params, label, lock_document, work, apply)
    }

    /// Apply finished background jobs (on this thread) and return every job that ended since
    /// the last call. Cheap when nothing runs; the UI calls it every frame.
    pub fn poll_jobs(&mut self) -> Vec<JobEvent> {
        let mut i = 0;
        while i < self.jobs.running.len() {
            match self.jobs.running.get(i).map(|r| r.rx.try_recv()) {
                Some(Ok(r)) => {
                    let job = self.jobs.running.remove(i);
                    self.finish_job(job, r);
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => i += 1,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    let job = self.jobs.running.remove(i);
                    let label = job.label.clone();
                    self.finish_job(job, Err(EngineError::Other(format!("`{label}` stopped unexpectedly; the document is unchanged"))));
                }
                None => break,
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.jobs.draining.retain(|h| !h.is_finished());
        std::mem::take(&mut self.jobs.events)
    }

    /// Block until job `id` ends, apply it, and return its result (the control channel's and
    /// MCP's default `wait`, and the CLI). `Err(Cancelled)` if it was cancelled.
    pub fn wait_job(&mut self, id: JobId) -> Result<Value> {
        // A job done a step at a time is done here, to the end.
        if let Some(i) = self.jobs.steppers.iter().position(|(j, _)| *j == id) {
            let (_, mut step) = self.jobs.steppers.remove(i);
            let mut tries = 0u32;
            while !step(&|| true) {
                // Its work hands it over from a thread natively: wait for that.
                tries += 1;
                if tries > 10_000 {
                    break;
                }
                std::thread::yield_now();
            }
        }
        if let Some(i) = self.jobs.running.iter().position(|r| r.id == id) {
            let job = self.jobs.running.remove(i);
            let stopped = || Err(EngineError::Other(format!("`{}` stopped unexpectedly", job.label)));
            // The browser's page thread may not block (`recv` waits with `Atomics.wait`, which
            // throws there): spin on the channel instead, for at most a minute (work that needs
            // this thread to finish, an AI worker's reply, can't arrive meanwhile; there is no
            // clock here, so the bound is a count of tries, about a minute's worth).
            #[cfg(target_arch = "wasm32")]
            let r = {
                let mut tries = 0u64;
                loop {
                    match job.rx.try_recv() {
                        Ok(r) => break r,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break stopped(),
                        Err(std::sync::mpsc::TryRecvError::Empty) if tries > 1_000_000_000 => {
                            break Err(EngineError::Other(format!("`{}` is still running", job.label)));
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            tries += 1;
                            std::hint::spin_loop();
                        }
                    }
                }
            };
            #[cfg(not(target_arch = "wasm32"))]
            let r = job.rx.recv().unwrap_or_else(|_| stopped());
            self.finish_job(job, r);
        }
        match self.jobs.recent.iter().rev().find(|j| j.id == id) {
            Some(j) if j.state == "done" => Ok(j.result.clone().unwrap_or(Value::Null)),
            Some(j) if j.state == "cancelled" => Err(EngineError::Cancelled),
            Some(j) => Err(EngineError::Other(j.error.clone().unwrap_or_else(|| "the job failed".into()))),
            None => Err(EngineError::Other(format!("no job {}", id.0))),
        }
    }

    /// Cancel job `id`: it stops at its next check, its result is discarded and the document
    /// stays unchanged. The document is unlocked at once. False if no such job is running.
    pub fn cancel_job(&mut self, id: JobId) -> bool {
        let Some(i) = self.jobs.running.iter().position(|r| r.id == id) else { return false };
        // A print worker can already have handed the job to the OS. Discarding its result
        // would falsely report cancellation; removal belongs to the system print queue.
        if matches!(self.jobs.running[i].command.as_str(), "file.print" | "file.printOneCopy") {
            return false;
        }
        let job = self.jobs.running.remove(i);
        job.ctx.cancel();
        self.end_job(&job, JobOutcome::Cancelled);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(h) = job.handle {
            self.jobs.draining.push(h);
        }
        true
    }

    /// Cancel every running job (e.g. before quitting).
    pub fn cancel_all_jobs(&mut self) {
        let ids: Vec<JobId> = self.jobs.running.iter().map(|r| r.id).collect();
        for id in ids {
            self.cancel_job(id);
        }
    }

    /// Is any job running? (Cheap: the UI asks every frame.)
    pub fn has_jobs(&self) -> bool {
        !self.jobs.running.is_empty()
    }

    /// Running job `id`.
    pub fn job(&self, id: JobId) -> Option<JobInfo> {
        self.jobs.running.iter().find(|r| r.id == id).map(Running::info)
    }

    /// Running jobs.
    pub fn jobs(&self) -> Vec<JobInfo> {
        self.jobs.running.iter().map(Running::info).collect()
    }

    /// Running jobs followed by the last few that ended (newest last), for `jobs.list`.
    pub fn jobs_with_recent(&self) -> Vec<JobInfo> {
        self.jobs().into_iter().chain(self.jobs.recent.iter().cloned()).collect()
    }

    /// The running job that locks document `doc`, if any.
    pub fn job_on(&self, doc: DocId) -> Option<JobInfo> {
        self.jobs.running.iter().find(|r| r.doc == Some(doc)).map(Running::info)
    }

    /// The running job that locks the active document, if any.
    pub fn active_job(&self) -> Option<JobInfo> {
        self.active().and_then(|st| self.job_on(st.doc.id))
    }

    /// Wait for background workers of cancelled jobs to exit (tests and benchmarks measure
    /// cancel latency with it). Native only; returns at once on wasm.
    pub fn join_cancelled_jobs(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        for h in self.jobs.draining.drain(..) {
            let _ = h.join();
        }
    }

    /// The name of the job that has the active document busy.
    pub fn active_job_label(&self) -> Option<String> {
        self.active_job().map(|j| j.label.clone())
    }

    /// Why command `id` can't run now because of a background job (None = no conflict).
    pub(crate) fn job_conflict(&self, id: &str, journal: bool) -> Option<String> {
        if !journal || independent_of_document(id) {
            return None;
        }
        let job = self.active_job()?;
        Some(format!("“{}” is still running on this document; wait for it to finish or cancel it (Esc)", job.label))
    }

    /// Cancel the jobs editing document `doc` (it is closing).
    pub(crate) fn cancel_jobs_on(&mut self, doc: DocId) {
        let ids: Vec<JobId> = self.jobs.running.iter().filter(|r| r.doc == Some(doc)).map(|r| r.id).collect();
        for id in ids {
            self.cancel_job(id);
        }
    }

    /// Run the command `id`: inline, or (with `background`) as a job when it is one.
    pub(crate) fn dispatch(&mut self, id: &str, params: Value, background: bool) -> Result<Started> {
        // The offer belongs to this one dispatch. Early errors drop it locally, including
        // authorization, validation, floating-selection and admission failures.
        let live_commit = self.live_commit.take();
        let spec = crate::commands::find(id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        // Params are named fields: anything but an object (or null, meaning none) is a caller
        // mistake, not "use every default".
        if !(params.is_object() || params.is_null()) {
            return Err(EngineError::BadParams { cmd: id.to_string(), msg: "params must be a JSON object".into() });
        }
        // Untrusted sessions gate every command, including the ones a command runs on its own
        // behalf (`file.automate.conditionalModeChange` runs `image.mode.*`), before any side
        // effect.
        if let Some(gate) = self.authorize {
            gate(id, &params)?;
        }
        let _authorization = AuthorizationScope::enter(self.authorize);
        let _document = crate::doc_path::scope(self.active().and_then(|d| d.path.as_deref()).map(std::path::Path::new));
        // Last Filter runs as the filter it repeats: journaled, recorded in an action, faded and
        // run as a job like that filter, so a recorded step replays the same filter anywhere
        // (#3089), as Transform Again records its transform (#1440). With no filter yet it falls
        // through to its own precondition.
        if id == "filter.lastFilter"
            && let Some((filter, p)) = crate::filters::last_filter(self).filter(|(f, _)| f != id)
        {
            return self.dispatch(&filter, p, background);
        }
        if id == "file.new" {
            crate::document_preset_cmds::validate_new_params(self, &params)?;
        }
        crate::param_check::check(spec, &params)?;
        crate::filters::validate_params(id, &params)?;
        // A floating selection drops before any other command (Undo puts it back instead).
        if let Some(v) = crate::float_cmds::before_command(self, id)? {
            return Ok(Started::Done(v));
        }
        // Pixel commands follow the Channels panel target unless the caller names one.
        let run_params = self.admit(id, spec, &params)?;
        // Color Lookup's `lut` may name an installed LUT; its text is read here, where the library is.
        let run_params = crate::lut_id_params::resolve(self, id, run_params)?;
        // This UI-only disclosure preference applies to both `execute` and `start`:
        // desktop UI normally calls `start`, even for synchronous Layer Style commands.
        // The layer the command will change, named as the command accepts it (id or name), else the
        // active one. `layer.layerStyle.clear` needs no case: it leaves no effects, so nothing expands.
        // Transfer Effects changes its destination (`to`), not the layer it takes the effects from.
        let new_fx_target = if id.starts_with("layer.layerStyle.") {
            let target = if id == "layer.layerStyle.transferEffects" {
                crate::commands::layer_param(self, &json!({"layer": params.get("to").cloned().unwrap_or(Value::Null)}))
            } else {
                crate::commands::layer_param(self, &params)
            }
            .ok();
            self.active().and_then(|st| {
                let target = target?;
                (st.doc.layer(target)?.effects.items.is_empty()).then_some((st.doc.id, target))
            })
        } else {
            None
        };
        let expand_new_fx = crate::commands::layer_panel_option(self, "expandNewEffects", true);
        self.coalesce_request = params.get("coalesce").and_then(Value::as_str).map(str::to_string);
        self.color_restrict = crate::channel_cmds::color_restriction(self, id, &run_params);
        self.filter_mask_layer = crate::channel_cmds::filter_mask_layer(self, id, &run_params);
        self.jobs.spawn = background;
        // Commands this one runs on its own behalf journal after this mark (#3067).
        let nested_from = self.journal.len();
        let pre_selected = if self.actions.recording.is_some() { self.active().map(|st| st.selected_layers()) } else { None };
        // A replayable command is recorded as one step, so recording pauses while it runs: an
        // action it plays (a script step) must not flush the steps it ran so far into the action
        // being recorded, nor move the recording mark past the entries it is about to replace.
        let recording = if spec.journal && crate::actions_cmds::replayable(id) { self.actions.recording.take() } else { None };
        let before = self.active().map(|st| st.doc.clone());
        // Last-resort guard (AGENTS.md, Never crash): a command that panics anyway fails with an
        // error instead of taking the app down. `edit` only commits a document after its closure
        // returns, so the documents are unchanged.
        self.live_commit = live_commit.filter(|live| live.cmd == id);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.run)(self, &run_params)))
            .unwrap_or_else(|_| Err(EngineError::Other(format!("`{id}` failed with an internal error (logged); the document is unchanged"))));
        self.live_commit = None;
        if self.actions.recording.is_none() {
            self.actions.recording = recording;
        }
        self.jobs.spawn = false;
        self.coalesce_request = None;
        self.color_restrict = None;
        self.filter_mask_layer = None;
        #[cfg(any(test, debug_assertions))]
        {
            self.hint_before = None;
        }
        let pending = self.jobs.pending.take();
        match (r, pending) {
            (Err(e), Some(job)) => {
                job.ctx.cancel();
                Err(e)
            }
            (Err(e), None) => Err(self.explain_missing_layer(e)),
            (Ok(_), Some(mut job)) => {
                job.command = id.to_string();
                job.params = params;
                job.journal = spec.journal;
                job.recorded_by = self.actions.recording.map(|_| self.actions.take);
                job.pre_selected = pre_selected.clone();
                let jid = job.id;
                self.jobs.running.push(job);
                Ok(Started::Job(jid))
            }
            (Ok(v), None) => {
                if let Some(before) = before {
                    self.add_relayout_damage(&before);
                }
                if let Some((doc_id, layer_id)) = new_fx_target
                    && let Some(st) = self.active_mut()
                    && st.doc.id == doc_id
                    && st.doc.layer(layer_id).is_some_and(|l| !l.effects.items.is_empty())
                {
                    if expand_new_fx {
                        st.fx_collapsed.retain(|id| *id != layer_id);
                    } else if !st.fx_collapsed.contains(&layer_id) {
                        st.fx_collapsed.push(layer_id);
                    }
                }
                self.after_command(id, params, spec.journal, Some(nested_from), pre_selected.as_deref());
                Ok(Started::Done(v))
            }
        }
    }

    /// Grows the damage a command reported for the pixels it changed by what effects laid out
    /// over a layer's frame repaint when it moved ([`photocraft_compose::relayout_damage`]): a
    /// stroke on a layer with a Gradient Overlay reports the stroke, but the gradient is laid out
    /// again over the whole grown layer (#4053). `before` is the active document before.
    fn add_relayout_damage(&mut self, before: &Arc<Document>) {
        let Some(st) = self.active_mut().filter(|st| st.doc.id == before.id && !Arc::ptr_eq(&st.doc, before)) else { return };
        let Some(d) = st.last_damage.filter(|d| !d.is_empty()) else { return };
        let extra = photocraft_compose::relayout_damage(before, &st.doc);
        if !extra.is_empty() {
            st.last_damage = Some(d.union(&extra));
        }
    }

    /// A command that failed on a layer id the document doesn't have says which ids it does have
    /// (#5058): ids are not positions (an opened PNG's Background is not layer 1), and an agent
    /// should not need `document.inspect` to recover.
    fn explain_missing_layer(&self, e: EngineError) -> EngineError {
        match (e, self.active()) {
            (EngineError::NoLayer(id), Some(st)) if st.doc.layer(id).is_none() => {
                EngineError::UnknownLayer { id, known: crate::commands::layer_listing(&st.doc) }
            }
            (e, _) => e,
        }
    }

    /// Bookkeeping after a command (or a job's apply) succeeded.
    ///
    /// `nested_from` is the journal length when the command started. A replayable command stands
    /// for the commands it ran itself (Straighten runs Image Rotation and Crop): their entries are
    /// dropped, so a recorded action replays the command once, not its steps as well (#3067).
    /// Edit › Transform › Again (and Again on a Copy) is not replayable and keeps the concrete
    /// steps it ran instead.
    fn after_command(&mut self, id: &str, params: Value, journal: bool, nested_from: Option<usize>, pre_selected: Option<&[LayerId]>) {
        // A layer-mask view ends when another layer becomes active (#196).
        if let Some(st) = self.active_mut() {
            crate::mask_view_cmds::fix(st);
        }
        crate::edit_menu_cmds::after_command(self, id, &params);
        crate::automate_cmds::after_command(self, id);
        self.sync_preset_store();
        if !journal {
            return;
        }
        if let Some(from) = nested_from.filter(|_| crate::actions_cmds::replayable(id)) {
            self.journal.truncate(from);
        }
        if !crate::brush_cmds::coalesce_journal(self, id, &params) {
            let params = match id {
                _ if self.actions.recording.is_none() => params,
                "layer.duplicate" => crate::artboard_cmds::recorded_duplicate(self, crate::layer_multi_cmds::recorded_step(self, id, params, pre_selected)),
                "layerComp.apply" => crate::comps_cmds::recorded_apply(self, params),
                _ => crate::layer_multi_cmds::recorded_step(self, id, params, pre_selected),
            };
            self.journal.push((id.to_string(), params));
        }
    }

    /// Apply (or report the failure of) a job whose worker has delivered `r`.
    fn finish_job(&mut self, mut job: Running, r: Result<Payload>) {
        let apply = job.apply.take();
        let outcome = match r {
            Err(EngineError::Cancelled) => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed(e.to_string()),
            Ok(_) if job.ctx.cancelled() => JobOutcome::Cancelled,
            Ok(payload) => match self.apply_job(&job, apply, payload) {
                Ok(v) => JobOutcome::Done(job.ctx.with_notices(v)),
                Err(EngineError::Cancelled) => JobOutcome::Cancelled,
                Err(e) => JobOutcome::Failed(e.to_string()),
            },
        };
        self.end_job(&job, outcome);
    }

    fn apply_job(&mut self, job: &Running, apply: Option<Apply>, payload: Payload) -> Result<Value> {
        let Running { label, doc, snapshot, coalesce, color_restrict, filter_mask_layer, layers, command, params, journal, recorded_by, pre_selected, .. } =
            job;
        // A recording started after the job (#7289) keeps what it has so far, and the job's step
        // goes into the journal past its start, so that recording doesn't get it.
        let foreign = self.actions.recording.is_some() && *recorded_by != Some(self.actions.take);
        if foreign {
            crate::actions_cmds::flush_recording(self)?;
        }
        let prev_active = self.active;
        let target = match doc {
            Some(d) => {
                let i =
                    self.documents().iter().position(|st| st.doc.id == *d).ok_or_else(|| EngineError::Other(format!("`{label}`: the document was closed")))?;
                let unchanged = snapshot.as_ref().is_some_and(|w| self.documents().get(i).is_some_and(|st| Weak::ptr_eq(w, &Arc::downgrade(&st.doc))));
                if !unchanged {
                    return Err(EngineError::Other(format!("`{label}`: the document changed while it ran; the result was discarded")));
                }
                self.active = Some(i);
                self.note_active_change();
                Some(i)
            }
            None => None,
        };
        // Selecting a layer doesn't change the document, so the snapshot check above lets it
        // through. Apply against the layers the job started on: the colour restriction and any
        // `active`-based edit land on them (#7366). The user's selection is put back below.
        let user_layers = match (target, layers) {
            (Some(i), Some(started)) => self.docs.get_mut(i).and_then(|st| {
                let now = LayerSelection::of(st);
                (now != *started).then(|| {
                    started.clone().set(st);
                    now
                })
            }),
            _ => None,
        };
        self.coalesce_request = coalesce.clone();
        self.color_restrict = *color_restrict;
        self.filter_mask_layer = *filter_mask_layer;
        // Apply-time hooks can dispatch commands too. Keep the same single-step recording
        // contract as synchronous dispatch without truncating work done while the worker ran.
        let nested_from = self.journal.len();
        let recording = if *journal && crate::actions_cmds::replayable(command) { self.actions.recording.take() } else { None };
        let r = match apply {
            Some(f) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self, payload)))
                .unwrap_or_else(|_| Err(EngineError::Other(format!("`{label}` failed with an internal error (logged); the document is unchanged")))),
            None => Err(EngineError::Other("internal error: job applied twice".into())),
        };
        if self.actions.recording.is_none() {
            self.actions.recording = recording;
        }
        self.coalesce_request = None;
        self.color_restrict = None;
        self.filter_mask_layer = None;
        // Finish successful command bookkeeping on the document the job edited.
        // Restoring the viewed document first would attribute Fade and slice updates
        // to that document instead, when the user switched tabs mid-job.
        if r.is_ok() {
            self.after_command(command, params.clone(), *journal, Some(nested_from), pre_selected.as_deref());
            if foreign && let Some((start, _)) = self.actions.recording.as_mut() {
                *start = self.journal.len();
            }
        }
        // Give the user back the layers they selected while the job ran, unless the job chose a
        // selection itself (a new layer, say) or one of their layers is gone.
        if let (Some(started), Some(user), Some(st)) = (layers, user_layers, target.and_then(|i| self.docs.get_mut(i))) {
            let kept = LayerSelection::of(st) == *started;
            let exist = user.active.iter().chain(&user.selected).all(|id| st.doc.layer(*id).is_some());
            if (r.is_err() || kept) && exist {
                user.set(st);
            }
        }
        // Keep the user's active document unless the job opened a new one.
        // This also restores the selection when the apply step returned an error.
        if target.is_some() && self.active == target {
            self.active = prev_active.filter(|i| *i < self.docs.len()).or(self.active);
            self.note_active_change();
        }
        r
    }

    fn end_job(&mut self, job: &Running, outcome: JobOutcome) {
        // Every terminal path invalidates a preview, including worker errors, cancellation
        // and discarded results whose document was closed or changed.
        self.live_commit = None;
        let (state, result, error) = match &outcome {
            JobOutcome::Done(v) => ("done", Some(v.clone()), None),
            JobOutcome::Failed(e) => ("failed", None, Some(e.clone())),
            JobOutcome::Cancelled => ("cancelled", None, None),
        };
        let mut info = job.info();
        info.state = state;
        info.result = result;
        info.error = error;
        if state == "done" {
            info.progress = 1.0;
        }
        self.jobs.recent.push(info);
        if self.jobs.recent.len() > RECENT {
            let extra = self.jobs.recent.len() - RECENT;
            self.jobs.recent.drain(..extra);
        }
        self.jobs.events.push(JobEvent { id: job.id, command: job.command.clone(), label: job.label.clone(), document: job.doc, outcome, elapsed_ms: now_ms() - job.started_ms });
    }
}

/// Engine commands for the job table (the control channel and MCP use them too).
pub fn specs() -> Vec<crate::commands::CommandSpec> {
    use crate::commands::CommandSpec;
    vec![
        CommandSpec {
            id: "jobs.list",
            label: "List Background Jobs",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: |_| Ok(()),
            run: |s, _| Ok(json!({"jobs": s.jobs_with_recent()})),
            journal: false,
        },
        CommandSpec {
            id: "jobs.cancel",
            label: "Cancel Background Job",
            menu: &[],
            shortcut: None,
            params: r##"{"job":u64? (default: every running job)}"##,
            enabled: |_| Ok(()),
            run: |s, p| match p.get("job") {
                None | Some(Value::Null) => {
                    let n = s.jobs.running.len();
                    s.cancel_all_jobs();
                    Ok(json!({"cancelled": n.saturating_sub(s.jobs.running.len())}))
                }
                Some(v) => {
                    let id = v.as_u64().ok_or_else(|| EngineError::BadParams { cmd: "jobs.cancel".into(), msg: "`job` must be a job id".into() })?;
                    if s.cancel_job(JobId(id)) {
                        Ok(json!({"cancelled": 1}))
                    } else if s.job(JobId(id)).is_some() {
                        Err(EngineError::Other("printing cannot be cancelled here; use the system print queue".into()))
                    } else {
                        Err(EngineError::Other(format!("no running job {id}")))
                    }
                }
            },
            journal: false,
        },
    ]
}

/// A deep EXR's bytes read from `path` (bounded: a clear error for a file larger than memory).
#[cfg(not(target_arch = "wasm32"))]
fn read_path(path: &str) -> Result<Vec<u8>> {
    photocraft_format::read_file(std::path::Path::new(path)).map_err(|e| EngineError::Other(format!("{path}: {e}")))
}

#[cfg(target_arch = "wasm32")]
fn read_path(path: &str) -> Result<Vec<u8>> {
    Err(EngineError::Other(format!("cannot open paths on the web: {path}")))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "../../io/tests/support/recovery_psd.rs"]
mod psd_open_fixture;

#[cfg(test)]
mod psd_open_confirmation_tests {
    use super::*;

    /// Collects either an inline open result or the completed background-job result.
    fn result(s: &mut Session, started: Started) -> Value {
        match started {
            Started::Done(v) => v,
            Started::Job(id) => s.wait_job(id).unwrap(),
        }
    }

    /// Verifies headless opens return readable PSD content and warnings without a held prompt.
    #[test]
    fn headless_open_keeps_best_effort_psd_import() {
        let mut s = Session::new();
        let started = s.start_open("damaged.psd", OpenSource::Bytes(Arc::new(super::psd_open_fixture::make_psd(true)))).unwrap();
        let v = result(&mut s, started);
        assert!(v.get("document").is_some(), "{v}");
        assert!(v.get("held").is_none(), "no new prompt for a headless caller");
        assert!(!s.active().unwrap().source_read_only);
        assert!(v["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("could not be decoded")));
    }

    /// Verifies interactive opens hold damaged PSD files but admit valid files immediately.
    #[test]
    fn interactive_open_holds_only_a_damaged_psd() {
        for corrupt in [false, true] {
            let mut s = Session::new();
            let started = s.start_open_for_user("file.psd", OpenSource::Bytes(Arc::new(super::psd_open_fixture::make_psd(corrupt)))).unwrap();
            let v = result(&mut s, started);
            if corrupt {
                assert_eq!(v["damaged"], true);
                assert!(s.documents().is_empty());
                let held = v["held"].as_u64().unwrap();
                let (doc, read_only) = s.take_held_open(held).unwrap();
                assert_eq!(doc.layers.len(), 2);
                assert!(!read_only);
            } else {
                assert!(v.get("document").is_some(), "{v}");
                assert!(!s.active().unwrap().source_read_only);
            }
        }
    }
}
