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
use photocraft_doc::{DocId, LayerContent, LayerId, TextLayer};
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
    let mut out = Vec::new();
    if names.is_empty() {
        return out;
    }
    for st in app.session.documents() {
        for (_, _, l) in st.doc.walk() {
            if let LayerContent::Text(t) = &l.content
                && uses(t, names)
                && crate::type_tool::shows_own_layout(&st.doc, t)
            {
                out.push((st.doc.id, l.id));
            }
        }
    }
    out
}

/// A name compared loosely: lower case, without spaces, hyphens and underscores (a PSD written
/// without the font names it "DejaVuSerif-Regular", its family guessed as "Deja Vu Serif").
fn loose(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, ' ' | '-' | '_')).flat_map(char::to_lowercase).collect()
}

fn uses(t: &TextLayer, names: &[String]) -> bool {
    let names: Vec<String> = names.iter().map(|n| loose(n)).collect();
    t.char_runs().iter().any(|r| {
        let family = loose(&r.style.font_family);
        names.contains(&family)
            || r.style.postscript_name.as_deref().is_some_and(|ps| {
                let ps = loose(ps);
                names.iter().any(|n| ps == *n || ps.strip_prefix(n.as_str()).is_some_and(|rest| !rest.is_empty() && family.starts_with(n.as_str())))
            })
    })
}

/// Fonts arrived: in every open document, the missing families an installed one now matches by
/// name or PostScript name are mapped to it (Resolve Missing Fonts' automatic choices), one undo
/// step per document. The pixels are left to [`install`]'s redraw.
fn auto_resolve(app: &mut PhotocraftApp) {
    let docs: Vec<DocId> =
        app.session.documents().iter().filter(|d| !photocraft_engine::type_extra_cmds::auto_matches(&d.doc).is_empty()).map(|d| d.doc.id).collect();
    for doc in docs {
        let r = app.with_document(doc, |app| app.run("type.resolveMissingFonts", json!({"auto": true, "keepPixels": true})));
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

/// After a document opened: its missing fonts. The user's stored fonts that provide them are read
/// (they re-render the layers when they arrive); for the others a notice offers Load Fonts… and
/// Resolve Missing Fonts…
pub fn after_open(app: &mut PhotocraftApp) {
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.id;
    let names = photocraft_engine::type_extra_cmds::missing_font_names(&st.doc);
    let mut still = Vec::new();
    for (family, ps) in names {
        let mut stored = user_fonts::request(&family);
        for p in &ps {
            stored |= user_fonts::request(p);
        }
        if !stored {
            still.push(family);
        }
    }
    if still.is_empty() {
        return;
    }
    let tag = format!("{MISSING_TAG}{}", doc.0);
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(tag.as_str()));
    let id = crate::notices::post(app, tl!("Missing fonts"), names_lines(&still), false, None);
    set_actions(app, id, &[("Load Fonts…", "type.loadFonts"), ("Resolve Missing Fonts…", "type.resolveMissingFonts")]);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.tag = Some(tag);
    }
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
        let missing =
            app.session.documents().iter().find(|d| d.doc.id.0 == doc).map(|d| photocraft_engine::type_extra_cmds::missing_fonts(&d.doc)).unwrap_or_default();
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
