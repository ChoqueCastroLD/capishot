//! [`Host`]: one engine session in the host's process, driven by command id like the control
//! protocol, with typed helpers for the frequent calls.

use std::collections::HashMap;
use std::sync::Arc;

use photocraft_engine::jobs;
use photocraft_engine::{BatchHistory, DocState, HistoryMode, Session};
use serde_json::{Value, json};

use crate::error::{Error, Result, bad};
use crate::events::{Event, Origin, Tracker, job_event};
use crate::live::LiveState;
use crate::{DocumentId, JobHandle, LayerId};

/// How a command's change enters the document's undo history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum History {
    /// One undo step per command (the default).
    #[default]
    Step,
    /// No undo step: the change is carried into every history state, so the person's own Undo
    /// and Redo keep it (a collaborator's or the host's change; the control protocol's
    /// `history: "none"`). Must finish inline: [`Host::start`] refuses it.
    None,
}

impl From<History> for HistoryMode {
    fn from(h: History) -> Self {
        match h {
            History::Step => HistoryMode::Step,
            History::None => HistoryMode::None,
        }
    }
}

/// Where and how [`Host::execute_with`] runs a command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecOptions {
    /// The open document to run on without making it active (`None`: the active one). Hidden
    /// documents are reached this way.
    pub document: Option<DocumentId>,
    /// Undo history behaviour.
    pub history: History,
}

impl ExecOptions {
    /// On `document`, one undo step.
    pub fn on(document: DocumentId) -> Self {
        ExecOptions { document: Some(document), history: History::Step }
    }

    /// On `document`, no undo step ([`History::None`]).
    pub fn silent(document: DocumentId) -> Self {
        ExecOptions { document: Some(document), history: History::None }
    }
}

/// How [`Host::start`] ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Started {
    /// The command finished inline (it isn't a job, or this build runs jobs inline).
    Done(Value),
    /// The command runs in the background: poll [`Host::job`], cancel with [`Host::cancel`];
    /// [`Event::JobFinished`] reports the end.
    Job(JobHandle),
}

/// A running job's state ([`Host::job`]).
#[derive(Clone, Debug, PartialEq)]
pub struct JobStatus {
    /// The job.
    pub job: JobHandle,
    /// Its command id.
    pub command: String,
    /// Shown to people ("Gaussian Blur").
    pub label: String,
    /// The document it works on (and locks against edits).
    pub document: Option<DocumentId>,
    /// `0.0..=1.0`.
    pub progress: f32,
    /// What it is doing now.
    pub message: String,
}

/// An open document, as [`Host::documents`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentInfo {
    /// Its id.
    pub id: DocumentId,
    /// Its name (the file name it was opened from).
    pub name: String,
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// A hidden helper document: never shown, given a tab or made active.
    pub hidden: bool,
    /// The active document.
    pub active: bool,
    /// Unsaved changes.
    pub dirty: bool,
    /// Moves on every change.
    pub revision: u64,
}

/// [`Host::open`]'s options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// The document's name (a file name; its extension also tells the decoder the format when
    /// the bytes don't). Default `"document.psd"`.
    pub name: Option<String>,
    /// Open as a hidden helper document: reached by id only, the active document left as it is.
    pub hidden: bool,
}

/// [`Host::inspect`]'s options (`document.inspect`'s params).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InspectOptions {
    /// Type layers' full character and paragraph styles (`text.style`) and every effect's
    /// settings (`effects.items[].fx`).
    pub text_style: bool,
}

/// [`Host::get_pixels`]'s options (`layer.getPixels`'s params).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GetPixels {
    /// `[x, y, width, height]` in document pixels (default: the layer's pixel bounds; a mask's:
    /// the canvas).
    pub rect: Option<[i32; 4]>,
    /// Read the layer mask (gray, one channel) instead of the pixels.
    pub mask: bool,
    /// Raw 8-bit rows instead of a PNG.
    pub raw: bool,
}

/// Pixels read by [`Host::get_pixels`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    /// The area read, `[x, y, width, height]` in document pixels.
    pub rect: [i32; 4],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// 4 (straight RGBA) or 1 (a mask).
    pub channels: u32,
    /// A PNG, or raw 8-bit rows with [`GetPixels::raw`].
    pub data: Vec<u8>,
    /// The layer's pixel revision when read.
    pub pixel_rev: u64,
}

/// [`Host::put_pixels`]'s options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PutPixels {
    /// Where the image's top-left goes (document pixels; default the origin).
    pub x: i32,
    /// Where the image's top-left goes (document pixels; default the origin).
    pub y: i32,
    /// Undo history behaviour.
    pub history: History,
}

/// [`Host::batch`]'s history: one step for the batch or one per command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchSteps {
    /// One undo step for the whole batch (default).
    #[default]
    One,
    /// One undo step per command.
    Each,
    /// No undo step at all ([`History::None`]).
    None,
}

/// What the widget (`CapishotView`, #31) does when the host shows or prewarms a document. The
/// engine side ([`Host::show`]) runs first, then the hook. Without a hook, `show` changes the
/// active document only and `prewarm` reports `false`.
pub trait ViewHooks {
    /// `document` became the shown one (draw it this frame).
    fn show(&mut self, document: DocumentId);
    /// Get `document` ready to show at once (decode, upload, lay out type); `options` as the
    /// control protocol's `document.prewarm`. Returns whether anything was done.
    fn prewarm(&mut self, document: DocumentId, options: &Value) -> Result<bool>;
}

/// How [`Host::start_save`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveStarted {
    /// Saved inline: the bytes.
    Done(Vec<u8>),
    /// Saving in the background: [`Host::take_saved`] once it finished.
    Job(JobHandle),
}

/// A subscription made with [`Host::subscribe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Subscription(u64);

struct OpenGroup {
    scope: photocraft_engine::ExecScope,
    batch: photocraft_engine::Batch,
}

type Callback = Box<dyn FnMut(&Event)>;
type Slot = Arc<wasm_sync::Mutex<Option<Vec<u8>>>>;

/// Capishot's engine in the host's process: open documents, commands, history, jobs, events and
/// live sync. One per editor; not `Send` (keep it on the thread that draws, like the UI).
///
/// Every call returns [`Result`]; nothing panics on bad input. Commands are the control
/// protocol's (`docs/control-protocol.md` › Engine commands), with the same ids and params.
pub struct Host {
    session: Session,
    tracker: Tracker,
    queue: Vec<Event>,
    subscribers: Vec<(u64, Callback)>,
    next_subscription: u64,
    senders: Vec<std::sync::mpsc::Sender<Event>>,
    group: Option<OpenGroup>,
    tool: String,
    hooks: Option<Box<dyn ViewHooks>>,
    saved: HashMap<u64, Slot>,
    pub(crate) live: HashMap<DocumentId, LiveState>,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").field("documents", &self.session.documents().len()).field("tool", &self.tool).finish()
    }
}

const DEFAULT_TOOL: &str = "move";

impl Host {
    /// A host with a fresh session and no documents.
    pub fn new() -> Self {
        Self::from_session(Session::new())
    }

    /// A host around an existing session (the widget's, #31).
    pub fn from_session(session: Session) -> Self {
        let mut host = Host {
            session,
            tracker: Tracker::default(),
            queue: Vec::new(),
            subscribers: Vec::new(),
            next_subscription: 1,
            senders: Vec::new(),
            group: None,
            tool: DEFAULT_TOOL.into(),
            hooks: None,
            saved: HashMap::new(),
            live: HashMap::new(),
        };
        // The baseline: events report changes from here on.
        host.tracker.collect(&host.session, &host.tool, Origin::Local);
        host
    }

    /// **Unstable.** The engine session. For the widget (#31) and escape hatches; its API may
    /// change in any release.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// **Unstable.** The engine session, mutably. Changes made here are reported by the next
    /// [`Self::poll`] like any other.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Install the widget's [`ViewHooks`] (replacing any before).
    pub fn set_view_hooks(&mut self, hooks: Box<dyn ViewHooks>) {
        self.hooks = Some(hooks);
    }

    // ---------- commands ----------

    /// Run command `command` with `params` on the active document, one undo step; its result.
    /// Ids and params are the control protocol's. A job-capable command runs to its end here
    /// (natively on this thread; use [`Self::start`] to run it in the background).
    pub fn execute(&mut self, command: &str, params: Value) -> Result<Value> {
        self.execute_with(command, params, ExecOptions::default())
    }

    /// [`Self::execute`] on another document and/or without an undo step.
    pub fn execute_with(&mut self, command: &str, params: Value, opts: ExecOptions) -> Result<Value> {
        if self.group.is_some() {
            return self.execute_in_group(command, params, opts);
        }
        Ok(self.session.execute_scoped(command, params, opts.document.map(Into::into), opts.history.into())?)
    }

    /// A control-protocol `engine.execute` request: `{command, params?, document?, history?}`
    /// (`history`: `"step"` or `"none"`).
    pub fn execute_request(&mut self, request: &Value) -> Result<Value> {
        let command = request.get("command").and_then(Value::as_str).ok_or_else(|| bad("engine.execute", "missing `command`"))?;
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let document = match request.get("document") {
            None | Some(Value::Null) => None,
            Some(v) => Some(DocumentId(v.as_u64().ok_or_else(|| bad("engine.execute", "`document` must be a document id"))?)),
        };
        let history = match HistoryMode::parse(request.get("history")).map_err(|m| bad("engine.execute", m))? {
            HistoryMode::Step => History::Step,
            HistoryMode::None => History::None,
        };
        self.execute_with(command, params, ExecOptions { document, history })
    }

    /// Every command: id, label, menu path, shortcut, params description and whether it is
    /// enabled now (`command.list`'s reply).
    pub fn commands(&mut self) -> Result<Value> {
        self.execute("command.list", json!({}))
    }

    /// Run `command` in the background when it supports it (filters, Content-Aware Fill, Remove
    /// with AI…): a [`JobHandle`] at once, or the result if it finished inline. Not with
    /// [`History::None`] (a change outside the history must finish inline) or inside a group.
    pub fn start(&mut self, command: &str, params: Value, document: Option<DocumentId>) -> Result<Started> {
        if self.group.is_some() {
            return Err(bad(command, "a group is open: run commands inline in it (execute)"));
        }
        let scope = self.session.begin_scope(document.map(Into::into), HistoryMode::Step)?;
        let r = self.session.start(command, params);
        self.session.end_scope(scope);
        Ok(match r? {
            jobs::Started::Done(v) => Started::Done(v),
            jobs::Started::Job(id) => Started::Job(id.into()),
        })
    }

    /// The running job's state; `None` once it ended (its end comes as [`Event::JobFinished`]).
    pub fn job(&self, job: JobHandle) -> Option<JobStatus> {
        self.session.job(job.into()).map(|j| JobStatus {
            job: j.id.into(),
            command: j.command,
            label: j.label,
            document: j.document.map(Into::into),
            progress: j.progress,
            message: j.message,
        })
    }

    /// Every running job.
    pub fn jobs(&self) -> Vec<JobStatus> {
        self.session.jobs().into_iter().filter_map(|j| self.job(j.id.into())).collect()
    }

    /// Cancel a running job: it stops at its next check, the document stays unchanged. False if
    /// no such job runs.
    pub fn cancel(&mut self, job: JobHandle) -> bool {
        self.session.cancel_job(job.into())
    }

    /// Block until `job` ends and return its result (native only: the browser's page thread
    /// must not block; there, [`Self::poll`] every frame and wait for [`Event::JobFinished`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait(&mut self, job: JobHandle) -> Result<Value> {
        // The job's end is reported by the next `poll`, as for any other.
        Ok(self.session.wait_job(job.into())?)
    }

    // ---------- events ----------

    /// Apply finished background jobs and report everything that changed since the last poll:
    /// the events are returned, passed to every [`Self::subscribe`] callback and sent to every
    /// [`Self::events`] channel. Call it once per frame (the widget does); cheap when nothing
    /// changed. Never blocks.
    pub fn poll(&mut self) -> Vec<Event> {
        self.session.step_jobs(&|| false);
        for e in self.session.poll_jobs() {
            self.queue.push(job_event(e));
        }
        self.collect(Origin::Local);
        let events = std::mem::take(&mut self.queue);
        for e in &events {
            for (_, f) in &mut self.subscribers {
                f(e);
            }
        }
        self.senders.retain(|tx| events.iter().all(|e| tx.send(e.clone()).is_ok()));
        events
    }

    /// Call `f` with every event from now on (at [`Self::poll`]).
    pub fn subscribe(&mut self, f: impl FnMut(&Event) + 'static) -> Subscription {
        let id = self.next_subscription;
        self.next_subscription += 1;
        self.subscribers.push((id, Box::new(f)));
        Subscription(id)
    }

    /// Stop a [`Self::subscribe`] callback.
    pub fn unsubscribe(&mut self, s: Subscription) {
        self.subscribers.retain(|(id, _)| *id != s.0);
    }

    /// A channel that gets every event from now on (at [`Self::poll`]); dropped receivers are
    /// forgotten.
    pub fn events(&mut self) -> std::sync::mpsc::Receiver<Event> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.senders.push(tx);
        rx
    }

    /// Compare the session with the last look and queue the events; live documents note their
    /// local changes.
    pub(crate) fn collect(&mut self, origin: Origin) {
        let events = self.tracker.collect(&self.session, &self.tool, origin);
        if origin == Origin::Local {
            for e in &events {
                crate::live::note_local(&mut self.live, e);
            }
        }
        self.queue.extend(events);
    }

    pub(crate) fn uid_of(&self, document: DocumentId, layer: LayerId) -> Option<crate::LayerUid> {
        self.tracker.uid_of(document, layer)
    }

    // ---------- tools ----------

    /// The tool the editor uses (`"move"` at first).
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// Set the tool (an id from the embed API's `listTools`: `brush`, `eraser`, `cloneStamp`,
    /// `removeAi`, `eyedropper`, `marquee`…). The widget (#31) applies it; [`Event::ToolChanged`]
    /// reports it at the next poll.
    pub fn set_tool(&mut self, tool: &str) -> Result<()> {
        let t = tool.trim();
        if t.is_empty() || t.len() > 64 || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
            return Err(bad("setTool", format!("not a tool id: {tool:?}")));
        }
        self.tool = t.to_string();
        Ok(())
    }

    // ---------- documents ----------

    /// The open documents: shown ones in tab order, then hidden ones.
    pub fn documents(&self) -> Vec<DocumentInfo> {
        let active = self.session.active().map(|st| st.doc.id);
        self.session.documents().iter().map(|st| info(st, Some(st.doc.id) == active)).collect()
    }

    /// The active document.
    pub fn active_document(&self) -> Option<DocumentId> {
        self.session.active().map(|st| st.doc.id.into())
    }

    fn state(&self, document: DocumentId) -> Result<&DocState> {
        self.session.documents().iter().find(|st| st.doc.id.0 == document.0).ok_or(Error::NoDocument(document))
    }

    /// Open a PSD, PSB, `.pcraft`, PNG, JPEG, WebP… from `bytes` (a flat image becomes a
    /// one-layer document), decoded now. A shown document becomes the active one; a hidden one
    /// leaves the active document as it is. Returns its id and the import warnings.
    pub fn open(&mut self, bytes: Vec<u8>, opts: OpenOptions) -> Result<(DocumentId, Vec<String>)> {
        let name = open_name(&opts);
        let import = jobs::import_options(self.session.prefs());
        let shared = Arc::new(bytes);
        let (r, _) = photocraft_io::import_shared_for_open_with(&name, &shared, &import, &photocraft_raster::Interrupt::NONE)?;
        let (doc, warnings) = (r.document, r.warnings);
        Ok((self.admit(doc, opts.hidden), warnings))
    }

    /// [`Self::open`] for PSD/PSB bytes (any format the decoder knows is accepted).
    pub fn open_psd(&mut self, bytes: Vec<u8>, opts: OpenOptions) -> Result<(DocumentId, Vec<String>)> {
        self.open(bytes, opts)
    }

    /// [`Self::open`] in the background: decoded on a worker (natively, and in the browser
    /// threads build), with progress and cancel. The job's result is `{document, warnings}`
    /// (`document` the new id). Runs inline (returns [`Started::Done`]) in the single-threaded
    /// browser build.
    pub fn start_open(&mut self, bytes: Vec<u8>, opts: OpenOptions) -> Result<Started> {
        let name = open_name(&opts);
        let import = jobs::import_options(self.session.prefs());
        let shared = Arc::new(bytes);
        let hidden = opts.hidden;
        let label = format!("Opening {name}");
        let started = self.session.start_job_in_worker(
            jobs::OPEN_JOB,
            json!({"name": name, "hidden": hidden}),
            &label,
            false,
            move |ctx| {
                ctx.stage(0.0, 1.0, "Decoding", |ctl| photocraft_io::import_shared_for_open_with(&name, &shared, &import, ctl))
                    .map(|(r, _)| (r.document, r.warnings))
                    .map_err(|e| match e {
                        photocraft_io::IoError::Cancelled => photocraft_engine::EngineError::Cancelled,
                        e => photocraft_engine::EngineError::Other(e.to_string()),
                    })
            },
            move |s, (doc, warnings)| {
                let id = admit(s, doc, hidden);
                Ok(json!({"document": id.0, "warnings": warnings}))
            },
        )?;
        Ok(match started {
            jobs::Started::Done(v) => Started::Done(v),
            jobs::Started::Job(id) => Started::Job(id.into()),
        })
    }

    fn admit(&mut self, doc: photocraft_engine::doc::Document, hidden: bool) -> DocumentId {
        admit(&mut self.session, doc, hidden)
    }

    /// Make `document` the shown, active one in one step (a hidden one becomes shown; with
    /// `hide_previous` the one shown before becomes hidden), then tell the widget
    /// ([`ViewHooks::show`]).
    pub fn show(&mut self, document: DocumentId, hide_previous: bool) -> Result<()> {
        self.no_group("show")?;
        self.session.show_document(document.into(), hide_previous).ok_or(Error::NoDocument(document))?;
        if let Some(h) = self.hooks.as_mut() {
            h.show(document);
        }
        Ok(())
    }

    /// Ask the widget to get `document` ready to show at once ([`ViewHooks::prewarm`]); `false`
    /// without a widget.
    pub fn prewarm(&mut self, document: DocumentId, options: &Value) -> Result<bool> {
        self.state(document)?;
        match self.hooks.as_mut() {
            Some(h) => h.prewarm(document, options),
            None => Ok(false),
        }
    }

    /// Close `document`. Unsaved changes refuse unless `force`.
    pub fn close(&mut self, document: DocumentId, force: bool) -> Result<()> {
        self.no_group("close")?;
        let st = self.state(document)?;
        if !force && st.revision != st.saved_revision {
            return Err(Error::Engine(format!("{} has unsaved changes (pass force to close it anyway)", st.doc.name)));
        }
        let i = self.session.index_of_document(document.into()).ok_or(Error::NoDocument(document))?;
        self.session.close(i);
        self.live.remove(&document);
        Ok(())
    }

    /// `document` as PSD bytes (PSB when it is too large for PSD). Incremental: layers unchanged
    /// since the last save of this document aren't encoded again (the bytes are a full save's).
    /// Marks the document saved. Linked smart objects are not read from disk.
    pub fn save_psd(&mut self, document: DocumentId) -> Result<Vec<u8>> {
        self.save(document, "psd")
    }

    /// `document` as `format` bytes: `psd`, `psb`, `pcraft`, `png`, `jpg`, `webp`, `tiff`…
    /// A layered save (PSD, PSB, `.pcraft`) marks the document saved.
    pub fn save(&mut self, document: DocumentId, format: &str) -> Result<Vec<u8>> {
        let (doc, opts, revision) = self.export_args(document, format)?;
        let bytes = photocraft_io::export(&doc, &export_name(&doc.name, format)?, &opts)?.bytes;
        self.mark_saved(document, format, revision);
        Ok(bytes)
    }

    /// [`Self::save`] in the background: encoded on a worker (natively, and in the browser
    /// threads build), with progress and cancel. Take the bytes with [`Self::take_saved`] once
    /// [`Event::JobFinished`] says it is done (its result is `{bytes: length, document}`). In
    /// the single-threaded browser build it finishes inline: [`SaveStarted::Done`].
    pub fn start_save(&mut self, document: DocumentId, format: &str) -> Result<SaveStarted> {
        let (doc, opts, revision) = self.export_args(document, format)?;
        let name = export_name(&doc.name, format)?;
        let slot = Slot::default();
        let put = slot.clone();
        let layered = photocraft_engine::file_cmds::saves_in_place(&name);
        let started = self.session.start_job_in_worker(
            "file.save",
            json!({"document": document.0, "format": format}),
            &format!("Saving {name}"),
            false,
            move |ctx| {
                ctx.stage(0.0, 1.0, "Encoding", |ctl| photocraft_io::export_with(&doc, &name, &opts, ctl)).map(|r| r.bytes).map_err(|e| match e {
                    photocraft_io::IoError::Cancelled => photocraft_engine::EngineError::Cancelled,
                    e => photocraft_engine::EngineError::Other(e.to_string()),
                })
            },
            move |s, bytes: Vec<u8>| {
                if layered {
                    mark_saved(s, document, revision);
                }
                let len = bytes.len();
                *put.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bytes);
                Ok(json!({"bytes": len, "document": document.0}))
            },
        )?;
        Ok(match started {
            jobs::Started::Done(_) => SaveStarted::Done(slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().unwrap_or_default()),
            jobs::Started::Job(id) => {
                self.saved.insert(id.0, slot);
                SaveStarted::Job(id.into())
            }
        })
    }

    /// The bytes of a finished [`Self::start_save`]; `None` while it runs, if it failed or was
    /// cancelled, or once taken.
    pub fn take_saved(&mut self, job: JobHandle) -> Option<Vec<u8>> {
        let bytes = self.saved.get(&job.0)?.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if bytes.is_some() || self.session.job(job.into()).is_none() {
            self.saved.remove(&job.0);
        }
        bytes
    }

    fn export_args(&self, document: DocumentId, format: &str) -> Result<(Arc<photocraft_engine::doc::Document>, photocraft_io::ExportOptions, u64)> {
        let st = self.state(document)?;
        export_name(&st.doc.name, format)?;
        let opts = photocraft_io::ExportOptions { psd_cache: Some(st.psd_cache.clone()), read_linked_files: false, ..Default::default() };
        Ok((st.doc.clone(), opts, st.revision))
    }

    fn mark_saved(&mut self, document: DocumentId, format: &str, revision: u64) {
        if photocraft_engine::file_cmds::saves_in_place(&format!("x.{format}")) {
            mark_saved(&mut self.session, document, revision);
        }
    }

    /// `document.inspect`: the document and its layer tree (ids, `uid`, `pixelRev`, names,
    /// visibility, bounds, type…), as the control protocol gives it.
    pub fn inspect(&self, document: DocumentId, opts: InspectOptions) -> Result<Value> {
        let st = self.state(document)?;
        Ok(photocraft_engine::inspect::document_with(st, photocraft_engine::inspect::Options { text_style: opts.text_style }))
    }

    // ---------- pixels ----------

    /// Read a layer's pixels (straight RGBA, the document's RGB values) or its layer mask
    /// (`layer.getPixels`). No history step, nothing redrawn; at most 100 MP per call.
    pub fn get_pixels(&mut self, document: DocumentId, layer: LayerId, opts: GetPixels) -> Result<Pixels> {
        const ID: &str = "layer.getPixels";
        let mut p = json!({"layer": layer.0, "mask": opts.mask, "format": if opts.raw { "rgba" } else { "png" }});
        if let Some(r) = opts.rect {
            p["rect"] = json!(r);
        }
        let v = self.execute_with(ID, p, ExecOptions::on(document))?;
        let num = |k: &str| v.get(k).and_then(Value::as_u64).ok_or_else(|| Error::Engine(format!("{ID}: no `{k}` in the reply")));
        let rect = v.get("rect").and_then(|r| serde_json::from_value::<[i32; 4]>(r.clone()).ok()).ok_or_else(|| Error::Engine(format!("{ID}: no `rect` in the reply")))?;
        let data = v.get("data").and_then(Value::as_str).and_then(photocraft_paint::tile::b64_decode).ok_or_else(|| Error::Engine(format!("{ID}: no `data` in the reply")))?;
        Ok(Pixels {
            rect,
            width: u32::try_from(num("width")?).unwrap_or(u32::MAX),
            height: u32::try_from(num("height")?).unwrap_or(u32::MAX),
            channels: u32::try_from(num("channels")?).unwrap_or(4),
            data,
            pixel_rev: num("pixelRev")?,
        })
    }

    /// Replace a pixel layer's whole content with an image (`layer.putPixels {src}`): PNG, JPEG,
    /// WebP… bytes, top-left at `(x, y)`; the old pixels are dropped. One undo step (none with
    /// [`History::None`]).
    pub fn put_pixels(&mut self, document: DocumentId, layer: LayerId, src: &[u8], opts: PutPixels) -> Result<Value> {
        let p = json!({"layer": layer.0, "src": photocraft_paint::tile::b64_encode(src), "x": opts.x, "y": opts.y});
        self.execute_with("layer.putPixels", p, ExecOptions { document: Some(document), history: opts.history })
    }

    // ---------- batches, history, groups ----------

    /// Several commands as one transaction on `document` (`document.batch`): in order, all or
    /// nothing (a failing one undoes the batch and fails it). File, window, undo/redo and nested
    /// batches are refused. Returns each command's result.
    pub fn batch(&mut self, document: DocumentId, commands: &[(&str, Value)], steps: BatchSteps) -> Result<Vec<Value>> {
        let list: Vec<Value> = commands.iter().map(|(c, p)| json!({"command": c, "params": p})).collect();
        let (history, mode) = match steps {
            BatchSteps::One => ("one", History::Step),
            BatchSteps::Each => ("step", History::Step),
            BatchSteps::None => ("one", History::None),
        };
        let v = self.execute_with("document.batch", json!({"commands": list, "history": history}), ExecOptions { document: Some(document), history: mode })?;
        Ok(v.get("results").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Undo `document`'s last step. False when there is nothing to undo.
    pub fn undo(&mut self, document: DocumentId) -> Result<bool> {
        self.no_group("undo")?;
        let scope = self.session.begin_scope(Some(document.into()), HistoryMode::Step)?;
        let r = self.session.try_undo();
        self.session.end_scope(scope);
        Ok(r?)
    }

    /// Redo `document`'s next step. False when there is nothing to redo.
    pub fn redo(&mut self, document: DocumentId) -> Result<bool> {
        self.no_group("redo")?;
        let scope = self.session.begin_scope(Some(document.into()), HistoryMode::Step)?;
        let r = self.session.try_redo();
        self.session.end_scope(scope);
        Ok(r?)
    }

    /// Start a group on `document` (`None`: the active one): every [`Self::execute`] until
    /// [`Self::end_group`] runs on it and the group becomes one undo step. Commands that open,
    /// close or switch documents, undo/redo or batch are refused in a group. Keep a group within
    /// one frame: the widget draws the group's document meanwhile.
    pub fn begin_group(&mut self, document: Option<DocumentId>) -> Result<()> {
        self.no_group("beginGroup")?;
        let scope = self.session.begin_scope(document.map(Into::into), HistoryMode::Step)?;
        match self.session.begin_batch(BatchHistory::One) {
            Ok(batch) => {
                self.group = Some(OpenGroup { scope, batch });
                Ok(())
            }
            Err(e) => {
                self.session.end_scope(scope);
                Err(e.into())
            }
        }
    }

    /// End the group: with `commit`, its commands become one undo step; without, everything
    /// they did is undone. Err when no group is open.
    pub fn end_group(&mut self, commit: bool) -> Result<()> {
        let g = self.group.take().ok_or_else(|| bad("endGroup", "no group is open"))?;
        self.session.end_batch(g.batch, commit);
        self.session.end_scope(g.scope);
        Ok(())
    }

    /// Run `f` as one group ([`Self::begin_group`]): committed when it returns `Ok`, undone when
    /// it returns `Err` (that error is returned).
    pub fn group<R>(&mut self, document: Option<DocumentId>, f: impl FnOnce(&mut Host) -> Result<R>) -> Result<R> {
        self.begin_group(document)?;
        let r = f(self);
        if self.group.is_some() {
            self.end_group(r.is_ok())?;
        }
        r
    }

    /// Whether a group is open.
    pub fn in_group(&self) -> bool {
        self.group.is_some()
    }

    fn no_group(&self, what: &str) -> Result<()> {
        if self.group.is_some() {
            return Err(bad(what, "not while a group is open (end_group first)"));
        }
        Ok(())
    }

    fn execute_in_group(&mut self, command: &str, params: Value, opts: ExecOptions) -> Result<Value> {
        let Some(g) = self.group.as_ref() else { return Err(bad(command, "no group is open")) };
        let target = DocumentId::from(g.scope.target());
        if opts.document.is_some_and(|d| d != target) {
            return Err(bad(command, format!("a group is open on document {}: commands run on it", target.0)));
        }
        if opts.history == History::None {
            return Err(bad(command, "history none can't run inside a group"));
        }
        if photocraft_engine::commands::find(command).is_none() {
            return Err(Error::UnknownCommand(command.to_string()));
        }
        photocraft_engine::parse_batch_steps(Some(&json!([{"command": command}]))).map_err(|m| bad(command, m))?;
        let v = self.session.execute(command, params)?;
        let g = self.group.as_ref().ok_or_else(|| bad(command, "no group is open"))?;
        self.session.batch_step_check(&g.batch, 0, command)?;
        Ok(v)
    }

    // ---------- named views ----------

    /// `document`'s named views (`document.views.list`).
    pub fn views(&mut self, document: DocumentId) -> Result<Value> {
        self.execute_with("document.views.list", json!({}), ExecOptions::on(document))
    }

    /// Define (or replace) view `name`: the layers it shows and hides (`document.views.define`;
    /// `layers` holds its `visible` / `hidden` lists). No history step.
    pub fn define_view(&mut self, document: DocumentId, name: &str, layers: Value) -> Result<Value> {
        let mut p = match layers {
            Value::Object(o) => Value::Object(o),
            Value::Null => json!({}),
            _ => return Err(bad("document.views.define", "the view's layers must be {visible?, hidden?}")),
        };
        p["name"] = json!(name);
        self.execute_with("document.views.define", p, ExecOptions::on(document))
    }

    /// Show view `name` (`document.views.apply`): visibility only, no history step.
    pub fn apply_view(&mut self, document: DocumentId, name: &str) -> Result<Value> {
        self.execute_with("document.views.apply", json!({"name": name}), ExecOptions::on(document))
    }

}

fn open_name(opts: &OpenOptions) -> String {
    opts.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("document.psd").to_string()
}

fn export_name(doc_name: &str, format: &str) -> Result<String> {
    let f = format.trim().trim_start_matches('.').to_ascii_lowercase();
    if f.is_empty() || !f.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(bad("save", format!("not a format: {format:?}")));
    }
    let stem = doc_name.rsplit_once('.').map_or(doc_name, |(s, _)| s);
    let stem = if stem.trim().is_empty() { "document" } else { stem };
    Ok(format!("{stem}.{f}"))
}

fn admit(s: &mut Session, mut doc: photocraft_engine::doc::Document, hidden: bool) -> DocumentId {
    photocraft_engine::file_cmds::raw_defaults(s, &[], &mut doc);
    let (index, _) = if hidden { s.open_hidden_document(doc, None) } else { s.open_document(doc, None) };
    s.documents().get(index).map(|st| st.doc.id.into()).unwrap_or(DocumentId(0))
}

fn mark_saved(s: &mut Session, document: DocumentId, revision: u64) {
    // The session hands out the active document mutably only: a quiet scope.
    let Ok(scope) = s.begin_scope(Some(document.into()), HistoryMode::Step) else { return };
    if let Some(st) = s.active_mut() {
        st.saved_revision = revision;
    }
    s.end_scope(scope);
}

fn info(st: &DocState, active: bool) -> DocumentInfo {
    DocumentInfo {
        id: st.doc.id.into(),
        name: st.doc.name.clone(),
        width: st.doc.size.width,
        height: st.doc.size.height,
        hidden: st.hidden,
        active,
        dirty: st.revision != st.saved_revision,
        revision: st.revision,
    }
}
