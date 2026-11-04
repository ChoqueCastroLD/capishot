//! The user's own fonts: Type › Load Fonts…, Load Font Folder… and Manage Fonts…, the missing
//! fonts notice of an opened document, and the shell side of `photocraft_text::user_fonts` (its
//! index, lazy restore and store writes).
//!
//! Font files come in through a [`FontSink`]: the browser's pickers (`Services::pick_fonts`,
//! which also walk a picked folder), the desktop's file dialog, files dropped on the window, and
//! the web shell's store reads. [`tick`] (every frame) checks and registers them with the shared
//! text engine, so they are in the font menus and psd-compiler's font list at once, re-renders
//! the type layers that were drawn without them, and sums a pick up in one notice. Keeping them
//! is the platform's job: the web shell writes [`photocraft_text::user_fonts::Op`]s to IndexedDB
//! (this browser only; nothing is uploaded) and reads a stored file back when a layout, a font
//! menu or a document asks for one of its families, or in idle time.

use std::sync::{Arc, Mutex, PoisonError};

use egui::RichText;
use photocraft_doc::{DocId, LayerContent, LayerId};
pub use photocraft_text::user_fonts;
use photocraft_text::user_fonts::{Added, Persistence, WriteError};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The Manage Fonts dialog (`__prefsui`).
const MANAGE: &str = "userFonts";
/// Font data checked and registered per frame at most, so a big folder keeps the window drawing.
const BYTES_PER_FRAME: u64 = 48 << 20;
/// The `Notice::tag` of a document's missing fonts notice.
const MISSING_TAG: &str = "missingFonts:";
/// The `Notice::tag` of the fonts-not-saved notice.
const UNSAVED_TAG: &str = "unsavedFonts";
/// Notice lines naming fonts, at most.
const MAX_NAMES: usize = 12;

/// What a platform hands over.
#[derive(Debug)]
pub enum Delivery {
    /// A picked or dropped file (its name, maybe with the folders it was in) and its bytes.
    File(String, Vec<u8>),
    /// A file of a picked folder that was not read, and why.
    Skipped(String, String),
    /// The pick is over (`cancelled`: nothing was chosen).
    Done { cancelled: bool },
    /// A stored file read back (lazy restore), by id.
    Stored(String, Result<Vec<u8>, String>),
    /// A store write of the font `id` finished.
    Written(String, Result<(), String>),
}

/// Hands fonts to the app from any thread or task; wakes it.
#[derive(Clone)]
pub struct FontSink {
    queue: Arc<Mutex<Vec<Delivery>>>,
    ctx: Option<egui::Context>,
}

impl FontSink {
    pub fn send(&self, d: Delivery) {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).push(d);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }
    pub fn file(&self, name: String, bytes: Vec<u8>) {
        self.send(Delivery::File(name, bytes));
    }
    pub fn skipped(&self, name: String, why: String) {
        self.send(Delivery::Skipped(name, why));
    }
    pub fn done(&self, cancelled: bool) {
        self.send(Delivery::Done { cancelled });
    }
    /// Files still waiting for [`tick`] (a picker reads the next file once the app caught up).
    pub fn backlog(&self) -> usize {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

/// Shows the browser's font file picker (`folder`: a folder picker) and reads the chosen files
/// into the sink, ending with [`FontSink::done`].
pub type PickFontsFn = Box<dyn FnMut(bool, FontSink)>;

/// One pick, summed up when it is done.
#[derive(Debug, Default)]
struct Batch {
    families: Vec<String>,
    files: usize,
    duplicates: usize,
    skipped: Vec<String>,
}

/// The app's part (the index itself is process-wide, in `photocraft_text::user_fonts`).
#[derive(Default)]
pub struct UserFonts {
    queue: Arc<Mutex<Vec<Delivery>>>,
    batch: Batch,
    /// Type layers whose document was busy when their font arrived.
    pending: Vec<(DocId, LayerId)>,
    /// Fonts whose store write failed this session (their families), and the last error not
    /// reported yet.
    unsaved: Vec<String>,
    write_error: Option<WriteError>,
}

impl PhotocraftApp {
    /// A sink for fonts (the web shell's pickers and store reads).
    pub fn font_sink(&self, ctx: Option<&egui::Context>) -> FontSink {
        FontSink { queue: self.user_fonts.queue.clone(), ctx: ctx.cloned() }
    }
}

/// Is `name` a font file (by its extension)?
pub fn is_font(name: &str) -> bool {
    user_fonts::is_font_file(name)
}

// ------------------------------------------------------------------ commands

/// Type › Load Fonts…, Load Font Folder… and Manage Fonts…; `None` for other ids.
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    match id {
        "type.loadFonts" | "type.loadFontFolder" => {
            let folder = id == "type.loadFontFolder" || params.get("folder").and_then(Value::as_bool) == Some(true);
            Some(load(app, folder))
        }
        "type.manageFonts" => Some(manage(app, params)),
        _ => None,
    }
}

/// Type › Manage Fonts…: the dialog, or for agents `{list: true}` (the loaded fonts),
/// `{remove: id}` or `{removeAll: true}`.
fn manage(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    if let Some(id) = p.get("remove") {
        let id = id.as_str().ok_or("`remove` must be a font id (from `list`)")?;
        let name = remove(app, id).ok_or_else(|| format!("no loaded font has the id {id:?}"))?;
        return Ok(json!({"removed": name}));
    }
    if p.get("removeAll").and_then(Value::as_bool) == Some(true) {
        return Ok(json!({"removed": clear(app)}));
    }
    if p.get("list").and_then(Value::as_bool) == Some(true) {
        let s = user_fonts::store();
        let fonts: Vec<Value> = s
            .entries()
            .iter()
            .map(|e| {
                let faces: Vec<Value> = e.faces.iter().map(|f| json!({"family": f.family, "style": f.style, "postscriptName": f.postscript})).collect();
                json!({"id": e.id, "name": e.name, "bytes": e.bytes, "faces": faces, "loaded": s.is_loaded(&e.id), "saved": !s.is_unsaved(&e.id)})
            })
            .collect();
        let kept = match s.persistence() {
            Persistence::Session => "session".to_string(),
            Persistence::Pending => "opening".to_string(),
            Persistence::Stored => "browser".to_string(),
            Persistence::Unavailable(e) => format!("unavailable: {e}"),
        };
        return Ok(json!({"fonts": fonts, "bytes": s.total_bytes(), "kept": kept, "persisted": s.persisted, "estimate": s.estimate}));
    }
    if p.as_object().is_some_and(|o| !o.is_empty()) {
        return Err("type.manageFonts takes {list: true}, {remove: id} or {removeAll: true}".into());
    }
    Ok(json!({"dialog": open_manage(app)}))
}

/// Shows the platform's picker; the files arrive through the sink.
fn load(app: &mut PhotocraftApp, folder: bool) -> Result<Value, String> {
    let sink = app.font_sink(None);
    if let Some(pick) = app.services.pick_fonts.as_mut() {
        pick(folder, sink);
        return Ok(json!({"picker": if folder { "folder" } else { "files" }}));
    }
    if folder {
        app.ask_folder(move |app, dir| {
            #[cfg(not(target_arch = "wasm32"))]
            let files = {
                let mut files = Vec::new();
                walk(std::path::Path::new(&dir), 0, &mut files);
                files
            };
            #[cfg(target_arch = "wasm32")]
            let files = {
                let _ = &dir;
                Vec::new()
            };
            read_files(app, files);
            Ok(Value::Null)
        })
    } else {
        let exts: Vec<&str> = user_fonts::FONT_EXTENSIONS.to_vec();
        app.pick_files_bytes(&exts, |app, files| {
            let sink = app.font_sink(None);
            for (name, bytes) in files {
                sink.file(name, bytes);
            }
            sink.done(false);
            tick(app, None);
            Ok(Value::Null)
        })
    }
}

/// The font files under `dir`, recursively (bounded), by extension.
#[cfg(not(target_arch = "wasm32"))]
fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > 16 || out.len() > 20_000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, depth + 1, out);
        } else if p.file_name().and_then(|n| n.to_str()).is_some_and(is_font) {
            out.push(p);
        }
    }
}

/// Reads font files from disk into the sink (the desktop's Load Font Folder…).
fn read_files(app: &mut PhotocraftApp, files: Vec<std::path::PathBuf>) {
    let sink = app.font_sink(None);
    for f in files {
        let name = f.to_string_lossy().into_owned();
        match photocraft_format::read_file(&f) {
            Ok(bytes) => sink.file(name, bytes),
            Err(e) => sink.skipped(name, e.to_string()),
        }
    }
    sink.done(false);
    tick(app, None);
}

// ------------------------------------------------------------------ every frame

/// Checks and registers what arrived, reports finished picks and failed writes, and re-renders
/// the type layers drawn without the new fonts. Call it every frame.
pub fn tick(app: &mut PhotocraftApp, ctx: Option<&egui::Context>) {
    let arrived = take_some(&app.user_fonts.queue);
    let more = !app.user_fonts.queue.lock().unwrap_or_else(PoisonError::into_inner).is_empty();
    if more && let Some(ctx) = ctx {
        ctx.request_repaint();
    }
    let mut ready: Vec<user_fonts::Prepared> = Vec::new();
    let mut picked_done = None;
    for d in arrived {
        match d {
            Delivery::File(name, bytes) => {
                app.user_fonts.batch.files += 1;
                match user_fonts::prepare(&name, bytes) {
                    Ok(p) => ready.push(p),
                    Err(why) => app.user_fonts.batch.skipped.push(format!("{}: {why}", user_fonts::file_name(&name))),
                }
            }
            Delivery::Skipped(name, why) => app.user_fonts.batch.skipped.push(format!("{}: {why}", user_fonts::file_name(&name))),
            Delivery::Done { cancelled } => picked_done = Some(cancelled),
            Delivery::Stored(id, result) => match result.and_then(|b| user_fonts::prepare("stored", b)) {
                Ok(p) if p.entry.id == id => ready.push(p),
                Ok(_) | Err(_) => {
                    if let Some(e) = user_fonts::store().load_failed(&id) {
                        log::warn!("a stored font ({}) could not be read and was removed", e.name);
                    }
                }
            },
            Delivery::Written(id, Ok(())) => log::debug!("font {id} stored"),
            Delivery::Written(id, Err(e)) => {
                let kind = user_fonts::store().write_failed(&id, &e);
                let families = user_fonts::store().entry(&id).map(|e| e.families().join(", "));
                if let Some(f) = families
                    && !app.user_fonts.unsaved.contains(&f)
                {
                    app.user_fonts.unsaved.push(f);
                }
                if app.user_fonts.write_error.is_none() || kind == WriteError::Quota {
                    app.user_fonts.write_error = Some(kind);
                }
            }
        }
    }
    if !ready.is_empty() {
        install(app, ready);
    }
    if let Some(cancelled) = picked_done {
        finish_batch(app, cancelled);
    }
    if app.user_fonts.write_error.is_some() {
        report_unsaved(app);
    }
    if !app.user_fonts.pending.is_empty() {
        let layers = std::mem::take(&mut app.user_fonts.pending);
        app.user_fonts.pending = app.session.refresh_type_layers(&layers);
    }
}

/// The deliveries to handle this frame: everything but files past [`BYTES_PER_FRAME`].
fn take_some(queue: &Arc<Mutex<Vec<Delivery>>>) -> Vec<Delivery> {
    let mut q = queue.lock().unwrap_or_else(PoisonError::into_inner);
    let mut bytes = 0u64;
    let mut n = 0;
    for d in q.iter() {
        let size = match d {
            Delivery::File(_, b) | Delivery::Stored(_, Ok(b)) => b.len() as u64,
            _ => 0,
        };
        if n > 0 && bytes.saturating_add(size) > BYTES_PER_FRAME {
            break;
        }
        bytes = bytes.saturating_add(size);
        n += 1;
    }
    q.drain(..n).collect()
}

/// Registers checked fonts, adds them to the index (and the store), and re-renders the layers
/// that use them.
fn install(app: &mut PhotocraftApp, ready: Vec<user_fonts::Prepared>) {
    // Which names arrive, to find the layers drawn without them, before they are registered:
    // "shows our own layout" compares the layer's pixels with what the engine draws now.
    let mut names: Vec<String> = Vec::new();
    for p in &ready {
        for f in &p.entry.faces {
            names.push(f.family.clone());
            if let Some(ps) = &f.postscript {
                names.push(ps.clone());
            }
        }
    }
    let mut layers = layers_using(app, &names);
    let mut new: Vec<user_fonts::Prepared> = Vec::new();
    {
        let mut store = user_fonts::store();
        for p in ready {
            match store.add(&p) {
                Added::New { families, .. } => {
                    app.user_fonts.batch.families.extend(families);
                    new.push(p);
                }
                Added::Duplicate { .. } => app.user_fonts.batch.duplicates += 1,
            }
        }
    }
    if new.is_empty() {
        return;
    }
    {
        let mut eng = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
        for p in &new {
            if eng.fonts.register_user_font(&p.entry.id, p.data.clone()).is_empty() {
                log::warn!("{} registered no family", p.entry.name);
            }
        }
    }
    {
        let mut store = user_fonts::store();
        for p in &new {
            store.loaded(&p.entry.id);
        }
    }
    crate::type_tool::invalidate_families();
    auto_resolve(app);
    layers.retain(|l| !app.user_fonts.pending.contains(l));
    app.user_fonts.pending.extend(layers);
    crate::fonts_ui::refresh_missing_dialogs(app);
    refresh_missing_notices(app);
}

/// Type layers (in every open document) that use one of `names` (families or PostScript names)
/// and show the engine's own layout, so they are redrawn when those fonts change.
fn layers_using(app: &PhotocraftApp, names: &[String]) -> Vec<(DocId, LayerId)> {
    photocraft_engine::type_extra_cmds::layers_using_fonts(&app.session, names, true)
}

/// Fonts arrived: in every open document, the missing families an installed one now matches by
/// name or PostScript name are mapped to it (Resolve Missing Fonts' automatic choices), one undo
/// step per document. The pixels are left to [`install`]'s redraw. The user edited nothing, so
/// a document that was unmodified (just opened, fonts still loading) stays unmodified.
fn auto_resolve(app: &mut PhotocraftApp) {
    let docs: Vec<DocId> =
        app.session.documents().iter().filter(|d| !photocraft_engine::type_extra_cmds::auto_matches(&d.doc).is_empty()).map(|d| d.doc.id).collect();
    for doc in docs {
        let r = app.with_document(doc, |app| {
            let clean = app.session.active().is_some_and(|st| !st.is_dirty());
            let r = app.run("type.resolveMissingFonts", json!({"auto": true, "keepPixels": true}));
            if clean && let Some(st) = app.session.active_mut() {
                st.saved_revision = st.revision;
            }
            r
        });
        if let Err(e) = r {
            log::warn!("matching missing fonts: {e}");
        }
    }
}

/// A pick is over: one notice with what was loaded and what was skipped.
fn finish_batch(app: &mut PhotocraftApp, cancelled: bool) {
    let b = std::mem::take(&mut app.user_fonts.batch);
    if cancelled && b.files == 0 && b.skipped.is_empty() {
        return;
    }
    let mut families: Vec<String> = Vec::new();
    for f in b.families {
        if !families.contains(&f) {
            families.push(f);
        }
    }
    let mut lines: Vec<String> = families.iter().take(MAX_NAMES).cloned().collect();
    if families.len() > MAX_NAMES {
        lines.push(crate::i18n::fmt(tl!("…and {n} more"), &[("n", &(families.len() - MAX_NAMES).to_string())]));
    }
    if b.duplicates > 0 {
        lines.push(crate::i18n::fmt(tl!("Already loaded: {n}"), &[("n", &b.duplicates.to_string())]));
    }
    for s in b.skipped.iter().take(MAX_NAMES) {
        lines.push(crate::i18n::fmt(tl!("Skipped {file}"), &[("file", s)]));
    }
    if b.skipped.len() > MAX_NAMES {
        lines.push(crate::i18n::fmt(tl!("…and {n} more"), &[("n", &(b.skipped.len() - MAX_NAMES).to_string())]));
    }
    let (title, error) = if families.is_empty() && b.duplicates == 0 {
        (tl!("No fonts were found").to_string(), true)
    } else {
        (crate::i18n::fmt(tl!("Fonts loaded: {n}"), &[("n", &families.len().to_string())]), false)
    };
    app.ui.status.clone_from(&title);
    app.ui.status_error = error;
    crate::notices::post(app, title, lines, error, None);
}

/// Fonts that could not be stored (quota): they work until the tab closes; one notice says so.
fn report_unsaved(app: &mut PhotocraftApp) {
    let Some(kind) = app.user_fonts.write_error.take() else { return };
    let names = app.user_fonts.unsaved.clone();
    // One notice for them all, however many writes failed.
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(UNSAVED_TAG));
    let mut lines = vec![crate::i18n::fmt(tl!("Not saved, usable until you close this tab: {fonts}"), &[("fonts", &short_list(&names))])];
    let title = match kind {
        WriteError::Quota => {
            lines.push(tl!("Remove fonts in Type › Manage Fonts… to free space.").to_string());
            tl!("Browser storage is full").to_string()
        }
        WriteError::Other(e) => {
            lines.push(e);
            tl!("Fonts could not be saved").to_string()
        }
    };
    let id = crate::notices::post(app, title, lines, true, None);
    set_actions(app, id, &[("Manage Fonts…", "type.manageFonts")]);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.tag = Some(UNSAVED_TAG.into());
    }
}

fn short_list(names: &[String]) -> String {
    let mut v: Vec<&str> = names.iter().map(String::as_str).filter(|s| !s.is_empty()).take(MAX_NAMES).collect();
    v.dedup();
    let mut s = v.join(", ");
    if names.len() > MAX_NAMES {
        s.push_str(", …");
    }
    s
}

fn set_actions(app: &mut PhotocraftApp, id: u64, actions: &[(&str, &str)]) {
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.actions =
            actions.iter().map(|(label, command)| crate::notices::NoticeAction { label: (*label).to_string(), command: (*command).to_string() }).collect();
    }
}

// ------------------------------------------------------------------ lazy restore (web shell)

/// The stored files to read now: those a layout, menu or document asked for, else one in idle
/// time (`idle`: the user isn't dragging, start-up is over).
pub fn take_loads(idle: bool) -> Vec<String> {
    user_fonts::store().next_loads(idle)
}

// ------------------------------------------------------------------ missing fonts notice

/// How long a document's served fonts may take before those still on their way count as not
/// downloaded (seconds).
const FONT_WAIT_S: f64 = 30.0;

/// The documents whose missing fonts are being decided (their served fonts are on their way, or
/// the manifest is), the `fonts.preload` requests waiting, and what an embedding host asked for.
pub struct FontChecks {
    checks: Vec<Check>,
    preloads: Vec<Preload>,
    /// Show the missing fonts notice (an embedding host may turn it off: `ui.set {fontsNotice}`).
    pub notice: bool,
    /// Families reported missing since the shell last looked (`fontsMissing` events).
    pub events: Vec<Vec<String>>,
    /// `fontsPreloadProgress` events since the shell last looked: one per family of a
    /// `fonts.preload` as it settles (`{id?, family, status, done, total}`).
    pub progress: Vec<Value>,
    /// `fontsReady` events since the shell last looked: an opened document's fonts settled
    /// (`{document, ready, missing, failed}`), its type drawn with them.
    pub ready: Vec<Value>,
    /// `document.fontsReady` requests waiting for their document's fonts.
    waits: Vec<DocWait>,
}

impl Default for FontChecks {
    fn default() -> Self {
        Self { checks: Vec::new(), preloads: Vec::new(), notice: true, events: Vec::new(), progress: Vec::new(), ready: Vec::new(), waits: Vec::new() }
    }
}

/// A `document.fontsReady` waiting until no font its document uses is on its way.
pub struct DocWait {
    pub doc: DocId,
    pub timeout_s: f64,
    pub reply: std::sync::mpsc::Sender<Value>,
    since: Option<f64>,
}

impl DocWait {
    pub fn new(doc: DocId, timeout_s: f64, reply: std::sync::mpsc::Sender<Value>) -> Self {
        Self { doc, timeout_s, reply, since: None }
    }
}

struct Check {
    doc: DocId,
    /// When the check first ran with a clock (egui time).
    since: Option<f64>,
}

/// A `fonts.preload` waiting for its families.
pub struct Preload {
    pub families: Vec<String>,
    pub timeout_s: f64,
    pub reply: std::sync::mpsc::Sender<Value>,
    /// The caller's tag, echoed in its `fontsPreloadProgress` events.
    pub id: Option<String>,
    since: Option<f64>,
    /// Families whose progress event went out.
    reported: Vec<bool>,
}

impl Preload {
    pub fn new(families: Vec<String>, timeout_s: f64, reply: std::sync::mpsc::Sender<Value>) -> Self {
        let reported = vec![false; families.len()];
        Self { families, timeout_s, reply, id: None, since: None, reported }
    }

    /// Tags the `fontsPreloadProgress` events of this preload.
    pub fn with_id(mut self, id: Option<String>) -> Self {
        self.id = id;
        self
    }
}

/// What a family a document asks for is, right now.
enum Verdict {
    /// Installed, bundled, arrived, or one of the user's stored fonts (read lazily).
    Here,
    /// On its way (or the manifest is): decide later.
    Waiting,
    /// The host serves it but every download failed.
    Failed,
    /// Nobody has it.
    Missing,
}

fn verdict(family: &str, ps: &[String]) -> Verdict {
    use photocraft_text::served::{Lookup, Status};
    match photocraft_text::served::lookup(family, ps) {
        Lookup::Served(served) => {
            photocraft_text::served::request_with(family, ps.first().map(String::as_str));
            if served != family {
                photocraft_text::served::request(&served);
            }
            match photocraft_text::served::family_status(&served) {
                Status::Ready => Verdict::Here,
                Status::Failed(_) => Verdict::Failed,
                Status::Pending | Status::NotRequested => Verdict::Waiting,
            }
        }
        Lookup::ManifestPending => Verdict::Waiting,
        Lookup::NotServed => {
            let mut stored = user_fonts::request(family);
            for p in ps {
                stored |= user_fonts::request(p);
            }
            if stored { Verdict::Here } else { Verdict::Missing }
        }
    }
}

/// After a document opened: its missing fonts. Families the host serves (`fonts/manifest.txt`,
/// the web build) are fetched first and only reported if their download fails; until the
/// manifest is read nothing is reported. The user's stored fonts that provide the others are read
/// too; for the rest a notice offers Load Fonts… and Resolve Missing Fonts… (unless the host
/// turned it off; it gets a `fontsMissing` event either way). Decided now when nothing is on its
/// way, else by [`tick_checks`] when it has arrived.
pub fn after_open(app: &mut PhotocraftApp) {
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.id;
    check_fonts_of(app, doc);
}

/// [`after_open`] for open document `doc`, active or not (a hidden helper document).
pub fn check_fonts_of(app: &mut PhotocraftApp, doc: photocraft_doc::DocId) {
    if !app.font_checks.checks.iter().any(|c| c.doc == doc) {
        app.font_checks.checks.push(Check { doc, since: None });
    }
    tick_checks(app, None);
}

/// Decides the documents whose fonts were on their way and answers `fonts.preload` requests:
/// every frame (`now`: egui time), and right after an open (`None`).
pub fn tick_checks(app: &mut PhotocraftApp, now: Option<f64>) {
    if !app.font_checks.checks.is_empty() {
        let checks = std::mem::take(&mut app.font_checks.checks);
        let mut keep = Vec::new();
        for mut c in checks {
            let Some(d) = app.session.documents().iter().find(|d| d.doc.id == c.doc) else { continue };
            let names = photocraft_engine::type_extra_cmds::missing_font_names(&d.doc);
            let (mut missing, mut failed, mut waiting) = (Vec::new(), Vec::new(), false);
            for (family, ps) in names {
                match verdict(&family, &ps) {
                    Verdict::Here => {}
                    Verdict::Waiting => waiting = true,
                    Verdict::Failed => failed.push(family),
                    Verdict::Missing => missing.push(family),
                }
            }
            if let Some(t) = now
                && c.since.is_none()
            {
                c.since = Some(t);
            }
            let timed_out = matches!((now, c.since), (Some(t), Some(s)) if t - s > FONT_WAIT_S);
            if waiting && !timed_out {
                keep.push(c);
                continue;
            }
            if timed_out {
                // What is still on its way after the wait counts as not downloaded.
                for (family, ps) in photocraft_engine::type_extra_cmds::missing_font_names(&d.doc) {
                    if matches!(verdict(&family, &ps), Verdict::Waiting) && !failed.contains(&family) {
                        failed.push(family);
                    }
                }
            }
            let ready = fonts_ready(&d.doc, missing.clone(), failed.clone());
            report_missing(app, c.doc, &missing, &failed);
            // Taken by the web shell each frame; elsewhere nobody does, so only the latest stay.
            if app.font_checks.ready.len() >= 64 {
                app.font_checks.ready.remove(0);
            }
            app.font_checks.ready.push(ready);
        }
        keep.append(&mut app.font_checks.checks);
        app.font_checks.checks = keep;
    }
    if !app.font_checks.waits.is_empty() {
        for mut w in std::mem::take(&mut app.font_checks.waits) {
            if let Some(t) = now
                && w.since.is_none()
            {
                w.since = Some(t);
            }
            let timed_out = matches!((now, w.since), (Some(t), Some(s)) if t - s > w.timeout_s);
            settle_wait(app, w, timed_out);
        }
    }
    if !app.font_checks.preloads.is_empty() {
        let preloads = std::mem::take(&mut app.font_checks.preloads);
        for mut p in preloads {
            if let Some(t) = now
                && p.since.is_none()
            {
                p.since = Some(t);
            }
            let timed_out = matches!((now, p.since), (Some(t), Some(s)) if t - s > p.timeout_s);
            settle_preload(app, p, timed_out);
        }
    }
}

/// Sends the progress events of `p`'s newly settled families, then its reply once every family
/// has settled (or keeps it waiting).
fn settle_preload(app: &mut PhotocraftApp, mut p: Preload, timed_out: bool) {
    let states: Vec<FamilyState> = p.families.iter().map(|f| family_state(f)).collect();
    let settled = |st: FamilyState| if timed_out && st == FamilyState::Loading { FamilyState::Failed } else { st };
    let total = p.families.len();
    for (i, st) in states.iter().enumerate() {
        let st = settled(*st);
        if st == FamilyState::Loading || p.reported.get(i).copied().unwrap_or(true) {
            continue;
        }
        if let Some(r) = p.reported.get_mut(i) {
            *r = true;
        }
        let done = p.reported.iter().filter(|r| **r).count();
        let mut e = json!({"family": p.families.get(i), "status": st.name(), "done": done, "total": total});
        if let Some(id) = &p.id {
            e["id"] = json!(id);
        }
        app.font_checks.progress.push(e);
    }
    if states.iter().any(|st| settled(*st) == FamilyState::Loading) {
        app.font_checks.preloads.push(p);
        return;
    }
    let (mut ready, mut missing, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for (f, st) in p.families.iter().zip(&states) {
        match settled(*st) {
            FamilyState::Loaded => ready.push(f.clone()),
            FamilyState::Missing => missing.push(f.clone()),
            _ => failed.push(f.clone()),
        }
    }
    let _ = p.reply.send(json!({"ok": true, "result": {"ready": ready, "missing": missing, "failed": failed}}));
}

/// `{document, ready, missing, failed}`: the families `doc`'s type uses, those nobody has and
/// those whose download failed (or didn't finish); `ready` is the rest.
fn fonts_ready(doc: &photocraft_doc::Document, missing: Vec<String>, failed: Vec<String>) -> Value {
    let mut ready: Vec<String> = Vec::new();
    for (_, _, l) in doc.walk() {
        if let LayerContent::Text(t) = &l.content {
            for r in t.char_runs() {
                let f = r.style.font_family;
                if !f.is_empty() && !ready.contains(&f) && !missing.contains(&f) && !failed.contains(&f) {
                    ready.push(f);
                }
            }
        }
    }
    json!({"document": doc.id.0, "ready": ready, "missing": missing, "failed": failed})
}

/// Replies to `w` once none of its document's fonts is on its way (what still is after
/// `timeout_s` counts as failed), else keeps it waiting. A closed document is an error.
fn settle_wait(app: &mut PhotocraftApp, w: DocWait, timed_out: bool) {
    let Some(d) = app.session.documents().iter().find(|d| d.doc.id == w.doc) else {
        let _ = w.reply.send(json!({"ok": false, "error": format!("document {} was closed", w.doc.0)}));
        return;
    };
    let (mut missing, mut failed, mut waiting) = (Vec::new(), Vec::new(), false);
    for (family, ps) in photocraft_engine::type_extra_cmds::missing_font_names(&d.doc) {
        match verdict(&family, &ps) {
            Verdict::Here => {}
            Verdict::Waiting if timed_out => failed.push(family),
            Verdict::Waiting => waiting = true,
            Verdict::Failed => failed.push(family),
            Verdict::Missing => missing.push(family),
        }
    }
    // Type kept waiting for a font is drawn by the shell in the frame it arrives
    // (`served_fonts::install`, before this check runs).
    if waiting && !timed_out {
        app.font_checks.waits.push(w);
        return;
    }
    let _ = w.reply.send(json!({"ok": true, "result": fonts_ready(&d.doc, missing, failed)}));
}

/// `document.fontsReady {document?, timeoutMs?}`: replies once the fonts the document's type
/// uses have arrived or failed and its type is drawn with them (at once when nothing is on its way).
pub fn wait_fonts(app: &mut PhotocraftApp, w: DocWait) {
    settle_wait(app, w, false);
}

/// Something waits for fonts (a document's check, a `fonts.preload` or a `document.fontsReady`).
pub fn checks_pending(app: &PhotocraftApp) -> bool {
    !app.font_checks.checks.is_empty() || !app.font_checks.preloads.is_empty() || !app.font_checks.waits.is_empty()
}

/// Are `doc`'s fonts settled: no check for it is waiting for served fonts?
pub fn fonts_settled(app: &PhotocraftApp, doc: DocId) -> bool {
    !app.font_checks.checks.iter().any(|c| c.doc == doc)
}

/// A `fonts.preload` waits here until each family has arrived or failed.
pub fn preload(app: &mut PhotocraftApp, p: Preload) {
    for f in &p.families {
        photocraft_text::served::request(f);
    }
    settle_preload(app, p, false);
}

/// Where a family stands for `fonts.preload` and `fonts.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FamilyState {
    /// Installed, bundled, stored, or served and arrived: type can use it now.
    Loaded,
    /// Served and on its way (or the manifest is still being read).
    Loading,
    /// Served, but its download failed.
    Failed,
    /// Served and not asked for yet (`fonts.status` only: asking for it fetches it).
    Available,
    /// Not installed and not served.
    Missing,
}

impl FamilyState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Loading => "loading",
            Self::Failed => "failed",
            Self::Available => "available",
            Self::Missing => "missing",
        }
    }
}

/// `family`'s state now. A served family nobody asked for counts as [`FamilyState::Loading`]
/// here (`fonts.preload` has asked for it); [`family_status`] tells it apart.
fn family_state(family: &str) -> FamilyState {
    match family_status(family) {
        FamilyState::Available => FamilyState::Loading,
        st => st,
    }
}

/// `family`'s state without asking for anything (`fonts.status`).
pub fn family_status(family: &str) -> FamilyState {
    use photocraft_text::served::{Lookup, Status};
    if photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.has_family(family) {
        return FamilyState::Loaded;
    }
    match photocraft_text::served::lookup(family, &[]) {
        Lookup::Served(served) => match photocraft_text::served::family_status(&served) {
            Status::Ready => FamilyState::Loaded,
            Status::Failed(_) => FamilyState::Failed,
            Status::Pending => FamilyState::Loading,
            Status::NotRequested => FamilyState::Available,
        },
        Lookup::ManifestPending => FamilyState::Loading,
        Lookup::NotServed => FamilyState::Missing,
    }
}

/// `fonts.status {families?}`: each family's state, grouped (`loaded`, `loading`, `failed`,
/// `available`, `missing`), plus where the manifest is. Without `families`, every family the
/// host serves. Asks for nothing.
pub fn fonts_status(families: Option<&[String]>) -> Value {
    use photocraft_text::served::ManifestState;
    let all;
    let families = match families {
        Some(f) => f,
        None => {
            all = photocraft_text::served::families();
            &all[..]
        }
    };
    let mut groups: [(FamilyState, Vec<&str>); 5] = [
        (FamilyState::Loaded, Vec::new()),
        (FamilyState::Loading, Vec::new()),
        (FamilyState::Failed, Vec::new()),
        (FamilyState::Available, Vec::new()),
        (FamilyState::Missing, Vec::new()),
    ];
    for f in families {
        let st = family_status(f);
        if let Some((_, v)) = groups.iter_mut().find(|(g, _)| *g == st) {
            v.push(f);
        }
    }
    let mut out = serde_json::Map::new();
    for (st, v) in groups {
        out.insert(st.name().into(), json!(v));
    }
    let manifest = match photocraft_text::served::manifest_state() {
        ManifestState::Loading => "loading",
        ManifestState::Loaded => "loaded",
        ManifestState::None => "none",
    };
    out.insert("manifest".into(), json!(manifest));
    Value::Object(out)
}

/// The notice (unless turned off) and the `fontsMissing` event for `doc`'s truly missing families
/// and those whose download failed.
fn report_missing(app: &mut PhotocraftApp, doc: DocId, missing: &[String], failed: &[String]) {
    if missing.is_empty() && failed.is_empty() {
        return;
    }
    let mut all: Vec<String> = missing.to_vec();
    all.extend(failed.iter().cloned());
    app.font_checks.events.push(all);
    if !app.font_checks.notice {
        return;
    }
    let tag = format!("{MISSING_TAG}{}", doc.0);
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(tag.as_str()));
    let id = crate::notices::post(app, tl!("Missing fonts"), names_lines(&notice_names(missing, failed)), false, None);
    set_actions(app, id, &[("Load Fonts…", "type.loadFonts"), ("Resolve Missing Fonts…", "type.resolveMissingFonts")]);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.tag = Some(tag);
    }
}

/// The notice's names: the missing families, then those that couldn't be downloaded, said so.
fn notice_names(missing: &[String], failed: &[String]) -> Vec<String> {
    let mut v: Vec<String> = missing.to_vec();
    v.extend(failed.iter().map(|f| crate::i18n::fmt(tl!("{family} (couldn't download it)"), &[("family", f)])));
    v
}

/// `doc`'s missing families still to report: without those the host serves (fetched or on their
/// way), but with those whose download failed (said so).
fn still_missing(doc: &photocraft_doc::Document) -> Vec<String> {
    let (mut missing, mut failed) = (Vec::new(), Vec::new());
    for (family, ps) in photocraft_engine::type_extra_cmds::missing_font_names(doc) {
        match verdict(&family, &ps) {
            Verdict::Missing => missing.push(family),
            Verdict::Failed => failed.push(family),
            Verdict::Here | Verdict::Waiting => {}
        }
    }
    notice_names(&missing, &failed)
}

/// The served fonts changed (the manifest arrived, or font files did): the families each missing
/// fonts notice lists that the host serves are fetched, and the notices list what is still missing
/// (or go). The shell calls it (`served_fonts::install`); the layers re-render there.
pub fn served_fonts_changed(app: &mut PhotocraftApp) {
    let tagged: Vec<u64> = app.ui.notices.iter().filter_map(|n| n.tag.as_deref()?.strip_prefix(MISSING_TAG)?.parse::<u64>().ok()).collect();
    for doc in tagged {
        let Some(d) = app.session.documents().iter().find(|d| d.doc.id.0 == doc) else { continue };
        for (family, ps) in photocraft_engine::type_extra_cmds::missing_font_names(&d.doc) {
            if let Some(served) = crate::served_fonts::served_family(&family, &ps) {
                photocraft_text::served::request(&served);
            }
        }
    }
    crate::type_tool::invalidate_families();
    crate::fonts_ui::refresh_missing_dialogs(app);
    refresh_missing_notices(app);
}

fn names_lines(names: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = names.iter().take(MAX_NAMES).cloned().collect();
    if names.len() > MAX_NAMES {
        lines.push(crate::i18n::fmt(tl!("…and {n} more"), &[("n", &(names.len() - MAX_NAMES).to_string())]));
    }
    lines
}

/// Fonts changed: each missing fonts notice lists what its document still misses, or goes.
fn refresh_missing_notices(app: &mut PhotocraftApp) {
    let tagged: Vec<(u64, u64)> =
        app.ui.notices.iter().filter_map(|n| Some((n.id, n.tag.as_deref()?.strip_prefix(MISSING_TAG)?.parse::<u64>().ok()?))).collect();
    for (id, doc) in tagged {
        let missing = app.session.documents().iter().find(|d| d.doc.id.0 == doc).map(|d| still_missing(&d.doc)).unwrap_or_default();
        if missing.is_empty() {
            app.ui.notices.retain(|n| n.id != id);
        } else if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
            n.lines = names_lines(&missing);
        }
    }
}

// ------------------------------------------------------------------ Manage Fonts dialog

/// Is this the Manage Fonts dialog (Close button only)?
pub fn is_manage(fields: &Map<String, Value>) -> bool {
    fields.get("__prefsui").and_then(Value::as_str) == Some(MANAGE)
}

/// Is this one of this module's dialogs?
pub fn owns(fields: &Map<String, Value>) -> bool {
    is_manage(fields)
}

/// Type › Manage Fonts…
pub fn open_manage(app: &mut PhotocraftApp) -> u64 {
    if let Some(d) = app.ui.dialogs.iter().find(|d| is_manage(&d.fields)) {
        return d.id;
    }
    user_fonts::store().want_estimate = true;
    crate::prefs_ui::open_dialog(app, MANAGE, "Manage Fonts", json!({"confirmClear": false, "message": ""}))
}

/// Removes the user font `id`: unregistered, out of the index and the store; the layers drawn
/// with it are redrawn with the fallback.
pub fn remove(app: &mut PhotocraftApp, id: &str) -> Option<String> {
    let entry = user_fonts::store().entry(id).cloned()?;
    let names: Vec<String> = entry.faces.iter().flat_map(|f| std::iter::once(f.family.clone()).chain(f.postscript.clone())).collect();
    let layers = layers_using(app, &names);
    user_fonts::store().remove(id);
    photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.unregister_user_font(id);
    crate::type_tool::invalidate_families();
    app.user_fonts.pending.extend(layers);
    crate::fonts_ui::refresh_missing_dialogs(app);
    Some(entry.name)
}

/// Removes every user font.
pub fn clear(app: &mut PhotocraftApp) -> usize {
    let ids: Vec<String> = user_fonts::store().entries().iter().map(|e| e.id.clone()).collect();
    let names: Vec<String> =
        user_fonts::store().entries().iter().flat_map(|e| e.faces.iter().flat_map(|f| std::iter::once(f.family.clone()).chain(f.postscript.clone()))).collect();
    let layers = layers_using(app, &names);
    user_fonts::store().clear();
    {
        let mut eng = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
        for id in &ids {
            eng.fonts.unregister_user_font(id);
        }
    }
    crate::type_tool::invalidate_families();
    app.user_fonts.pending.extend(layers);
    crate::fonts_ui::refresh_missing_dialogs(app);
    ids.len()
}

/// One row of the dialog.
struct Row {
    id: String,
    family: String,
    styles: String,
    bytes: u64,
    unsaved: bool,
}

/// The Manage Fonts dialog's body.
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let (rows, total, persistence, estimate, persisted) = {
        let s = user_fonts::store();
        let rows: Vec<Row> = s
            .entries()
            .iter()
            .map(|e| {
                let mut styles: Vec<&str> = e.faces.iter().map(|f| f.style.as_str()).collect();
                styles.dedup();
                Row { id: e.id.clone(), family: e.families().join(", "), styles: styles.join(", "), bytes: e.bytes, unsaved: s.is_unsaved(&e.id) }
            })
            .collect();
        (rows, s.total_bytes(), s.persistence().clone(), s.estimate, s.persisted)
    };
    ui.set_min_width(460.0);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    let summary = crate::i18n::fmt(tl!("Font files: {n} · {size}"), &[("n", &rows.len().to_string()), ("size", &crate::fonts_ui::size_text(total))]);
    ui.label(RichText::new(summary).font(crate::theme::medium(13.0)).color(t.text));
    ui.add_space(2.0);
    match &persistence {
        Persistence::Stored | Persistence::Pending => {
            ui.label(RichText::new(tl!("Stored in this browser only, never uploaded.")).color(t.text_dim).size(12.0));
            if let Some((used, quota)) = estimate {
                let text = crate::i18n::fmt(
                    tl!("Browser storage: {used} of {quota}"),
                    &[("used", &crate::fonts_ui::size_text(used)), ("quota", &crate::fonts_ui::size_text(quota))],
                );
                ui.label(RichText::new(text).color(t.text_dim).size(12.0));
            }
            match persisted {
                Some(true) => ui.label(RichText::new(tl!("Kept until you remove them.")).color(t.text_dim).size(12.0)),
                Some(false) => ui.label(RichText::new(tl!("The browser may clear them when space runs low.")).color(t.warning).size(12.0)),
                None => ui.label(""),
            };
        }
        Persistence::Session => {
            ui.label(RichText::new(tl!("Loaded for this session only.")).color(t.text_dim).size(12.0));
        }
        Persistence::Unavailable(why) => {
            ui.label(RichText::new(crate::i18n::fmt(tl!("Browser storage is unavailable: {error}"), &[("error", why)])).color(t.warning).size(12.0));
        }
    }
    ui.add_space(8.0);
    let mut action: Option<&str> = None;
    ui.horizontal(|ui| {
        if crate::widgets::secondary_button(ui, tl!("Load Fonts…"), 110.0).clicked() {
            action = Some("type.loadFonts");
        }
        if crate::widgets::secondary_button(ui, tl!("Load Font Folder…"), 130.0).clicked() {
            action = Some("type.loadFontFolder");
        }
        if !rows.is_empty() && crate::widgets::secondary_button(ui, tl!("Remove All"), 100.0).clicked() {
            f.insert("confirmClear".into(), json!(true));
        }
    });
    if f.get("confirmClear").and_then(Value::as_bool) == Some(true) {
        ui.add_space(6.0);
        let mut answer = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(tl!("Remove all fonts from this browser?")).color(t.warning));
            if crate::widgets::primary_button(ui, tl!("Remove All"), 100.0).clicked() {
                answer = Some(true);
            }
            if crate::widgets::secondary_button(ui, tl!("Keep"), 72.0).clicked() {
                answer = Some(false);
            }
        });
        if let Some(yes) = answer {
            f.insert("confirmClear".into(), json!(false));
            if yes {
                let n = clear(app);
                f.insert("message".into(), json!(crate::i18n::fmt(tl!("Removed: {n}"), &[("n", &n.to_string())])));
            }
        }
    }
    ui.add_space(8.0);
    let mut remove_id: Option<String> = None;
    if rows.is_empty() {
        ui.label(RichText::new(tl!("No fonts loaded yet.")).color(t.text_dim));
    } else {
        egui::ScrollArea::vertical().max_height(300.0).id_salt("user-fonts-manage").show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            egui::Grid::new("user-fonts-grid").num_columns(4).spacing([14.0, 6.0]).min_col_width(64.0).show(ui, |ui| {
                for h in ["Family", "Style", "Size"] {
                    ui.label(RichText::new(tl!(h)).color(t.text_faint).size(11.0));
                }
                ui.label("");
                ui.end_row();
                for r in &rows {
                    ui.vertical(|ui| {
                        ui.set_max_width(190.0);
                        ui.add(egui::Label::new(RichText::new(&r.family).font(crate::theme::medium(13.0)).color(t.text)).truncate());
                        if r.unsaved && matches!(persistence, Persistence::Stored) {
                            ui.label(RichText::new(tl!("Not saved")).color(t.warning).size(11.0));
                        }
                    });
                    ui.allocate_ui_with_layout(egui::vec2(120.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_max_width(120.0);
                        ui.add(egui::Label::new(RichText::new(&r.styles).color(t.text_dim)).truncate()).on_hover_text(&r.styles);
                    });
                    ui.label(RichText::new(crate::fonts_ui::size_text(r.bytes)).color(t.text_dim));
                    if crate::widgets::secondary_button(ui, tl!("Remove"), 72.0).clicked() {
                        remove_id = Some(r.id.clone());
                    }
                    ui.end_row();
                }
            });
        });
    }
    if let Some(id) = remove_id
        && let Some(name) = remove(app, &id)
    {
        f.insert("message".into(), json!(crate::i18n::fmt(tl!("“{family}” was removed."), &[("family", &name)])));
    }
    let msg = f.get("message").and_then(Value::as_str).unwrap_or("");
    if !msg.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(msg).color(t.text_dim).size(12.0));
    }
    if let Some(id) = action {
        // The picker is the platform's; files arrive on later frames and the list follows.
        if let Err(e) = crate::menus::invoke(app, ui.ctx(), id, json!({}))
            && e != crate::file_dialog::CANCELLED
        {
            f.insert("message".into(), json!(e));
        }
    }
}

#[cfg(test)]
#[path = "user_fonts_ui_tests.rs"]
mod tests;
