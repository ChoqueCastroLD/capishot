//! Fonts the host provides: it registers font files (from its own library, its server) at run
//! time, and the type layers that wait for them are redrawn. Nothing is stored or uploaded here;
//! keeping the files between runs is the host's job.
//!
//! Layout is deterministic given the same font files, so `type.layoutDigest` is the same on
//! native and wasm when both have the same fonts: call [`use_system_fonts`]`(false)` at start-up
//! (before anything lays out type) so the native build doesn't also see the computer's
//! installed fonts, which the browser never does.
//!
//! The flow per document: [`crate::Opened::missing_fonts`] (or [`crate::Host::fonts_ready`])
//! says which families are missing; the host fetches them (a [`FontProvider`] can be asked
//! through [`crate::Host::request_missing_fonts`]) and gives them to
//! [`crate::Host::add_fonts`], which registers them and redraws what waited (and maps loosely
//! spelled names, as Photoshop's automatic Resolve Missing Fonts does); `fonts_ready` then has an
//! empty `missing` list. A host that reads font files on a worker thread calls [`register`]
//! there, then [`crate::Host::pending_font_layers`] (before) and
//! [`crate::Host::refresh_fonts`] (after) on the editor's thread.
//!
//! **Per document:** [`crate::Host::add_document_fonts`] registers files for one document: they
//! are unregistered when it closes, unless another document or [`crate::Host::add_fonts`] (global)
//! holds them too. The registry is the process's: while registered, a font is usable by type in
//! any document. [`crate::Event::FontsReady`] says when a document has all its fonts: at its first
//! poll when none is missing, otherwise after the registration that completes them.
//!
//! **Aliases:** a [`FontFile`] can name other names documents call it by. An alias counts as one
//! of the file's families (`fonts_ready`, `FontsReady`, type layers asking for it) and is
//! unregistered with the file.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError};

use photocraft_doc::{DocId, Document, LayerContent, LayerId, TextLayer};
use photocraft_engine::{HistoryMode, Session};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{DocumentId, Error, LayerId as HostLayerId, Result};

/// One face of a registered font file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontFace {
    /// The family type layers name it by (typographic family, else family).
    pub family: String,
    /// Its style (Regular, Bold Italic…).
    pub style: String,
    /// Its PostScript name (PSD files name faces by it).
    pub postscript: Option<String>,
    /// 100..=900.
    pub weight: u16,
    /// Italic or oblique.
    pub italic: bool,
}

/// A font file to register, with the other names it answers to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFile {
    /// Its file name, for messages and the format when the bytes don't tell.
    pub name: String,
    /// TTF, OTF, TTC or WOFF.
    pub bytes: Vec<u8>,
    /// Other names documents call it by (an old PostScript name, a group's name for it). Each
    /// works as one of its families until the file is unregistered, except a name another font
    /// already provides, which stays that font's. Up to [`MAX_FONT_ALIASES`].
    pub aliases: Vec<String>,
}

impl FontFile {
    /// A file with no aliases.
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self { name: name.into(), bytes, aliases: Vec::new() }
    }

    /// The same file answering to `aliases`.
    pub fn with_aliases(mut self, aliases: Vec<String>) -> Self {
        self.aliases = aliases;
        self
    }
}

impl From<(String, Vec<u8>)> for FontFile {
    fn from((name, bytes): (String, Vec<u8>)) -> Self {
        Self::new(name, bytes)
    }
}

impl From<(&str, Vec<u8>)> for FontFile {
    fn from((name, bytes): (&str, Vec<u8>)) -> Self {
        Self::new(name, bytes)
    }
}

/// At most this many aliases per [`FontFile`] (each one registers the file's faces again), each
/// of at most [`MAX_FONT_ALIAS_CHARS`].
pub const MAX_FONT_ALIASES: usize = 256;
/// See [`MAX_FONT_ALIASES`].
pub const MAX_FONT_ALIAS_CHARS: usize = 256;

/// A registered font file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredFont {
    /// Its id: the lower-case hex SHA-256 of the font data (registering the same file twice is
    /// one font). [`unregister`] takes it.
    pub id: String,
    /// The file name it was registered as (no folders).
    pub name: String,
    /// Its faces (a collection has several).
    pub faces: Vec<FontFace>,
    /// The families it provides, in order, without repeats (its aliases are in `aliases`).
    pub families: Vec<String>,
    /// The aliases it answers to now ([`FontFile::aliases`]), from this registration and earlier ones.
    pub aliases: Vec<String>,
    /// It was registered before (same bytes): only new aliases changed anything.
    pub duplicate: bool,
    /// The aliases asked for that it doesn't answer to: another font holds the name, or a file
    /// with a face alike (same weight and slant) does. A text asking for this font by that name
    /// gets the other one.
    pub refused_aliases: Vec<String>,
}

/// Where a document's fonts stand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontsReady {
    /// The document.
    pub document: DocumentId,
    /// Families its type uses that type can use now.
    pub ready: Vec<String>,
    /// Families its type uses that no font provides; each with the PostScript names its runs ask
    /// for (to look up in the host's library).
    pub missing: Vec<MissingFamily>,
}

impl FontsReady {
    /// Every family is here: its type is drawn with its own fonts.
    pub fn is_ready(&self) -> bool {
        self.missing.is_empty()
    }
}

/// A family a document needs and no registered font provides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingFamily {
    /// The family name the type asks for.
    pub family: String,
    /// The PostScript names its runs ask for (may be empty).
    pub postscript: Vec<String>,
}

/// Where a family stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FamilyStatus {
    /// Type can use it now (bundled, registered, or installed when system fonts are on).
    Loaded,
    /// Nobody provides it.
    Missing,
}

/// The host's font library, asked for the families documents need. Implementations start a
/// fetch and return at once; when a file arrives the host calls [`register`] (from any thread)
/// and then [`crate::Host::refresh_fonts`] on the editor's thread.
pub trait FontProvider {
    /// Fetch the font for `family` (and these PostScript names, which pick the exact faces).
    fn request(&self, family: &str, postscript: &[String]);
}

fn bad(message: impl Into<String>) -> Error {
    crate::error::bad("fonts", message)
}

/// Whether type also uses the fonts installed on the computer (native builds; the browser never
/// sees them). Off makes layout identical across targets. Must be called before the text engine
/// first starts (before any document with type opens); returns whether it took effect.
pub fn use_system_fonts(on: bool) -> bool {
    photocraft_text::set_system_fonts(on)
}

/// Registers a font file (TTF, OTF, TTC or WOFF; `name` is its file name, for messages and the
/// format when the bytes don't tell). From now on type in every document can use its families.
/// Type layers already drawn without it are redrawn by [`crate::Host::refresh_fonts`]. Any thread.
pub fn register(name: &str, bytes: Vec<u8>) -> Result<RegisteredFont> {
    register_file(FontFile::new(name, bytes))
}

/// [`register`] with aliases. Any thread.
pub fn register_file(file: FontFile) -> Result<RegisteredFont> {
    let (prepared, aliases) = prepare_file(file)?;
    register_prepared(prepared, &aliases)
}

fn prepare_file(file: FontFile) -> Result<(photocraft_text::user_fonts::Prepared, Vec<String>)> {
    let FontFile { name, bytes, aliases } = file;
    if aliases.len() > MAX_FONT_ALIASES || aliases.iter().any(|a| a.chars().count() > MAX_FONT_ALIAS_CHARS) {
        let name = photocraft_text::user_fonts::file_name(&name);
        return Err(Error::File(format!("{name}: at most {MAX_FONT_ALIASES} aliases of {MAX_FONT_ALIAS_CHARS} characters")));
    }
    Ok((prepare(&name, bytes)?, aliases))
}

fn prepare(name: &str, bytes: Vec<u8>) -> Result<photocraft_text::user_fonts::Prepared> {
    photocraft_text::user_fonts::prepare(name, bytes).map_err(|why| Error::File(format!("{}: {why}", photocraft_text::user_fonts::file_name(name))))
}

fn register_prepared(p: photocraft_text::user_fonts::Prepared, aliases: &[String]) -> Result<RegisteredFont> {
    let faces: Vec<FontFace> = p
        .entry
        .faces
        .iter()
        .map(|f| FontFace { family: f.family.clone(), style: f.style.clone(), postscript: f.postscript.clone(), weight: f.weight, italic: f.italic })
        .collect();
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
    let before = eng.fonts.has_user_font(&p.entry.id);
    let families = eng.fonts.register_user_font(&p.entry.id, Arc::clone(&p.data));
    let alias_names = eng.fonts.alias_user_font(&p.entry.id, aliases);
    drop(eng);
    if families.is_empty() {
        return Err(Error::File(format!("{}: the file registered no font family", p.entry.name)));
    }
    let mut unique: Vec<String> = Vec::new();
    for f in families {
        if !unique.contains(&f) {
            unique.push(f);
        }
    }
    let refused_aliases = aliases
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty() && !alias_names.iter().chain(&unique).any(|n| n.eq_ignore_ascii_case(a)))
        .map(str::to_string)
        .collect();
    Ok(RegisteredFont { id: p.entry.id, name: p.entry.name, faces, families: unique, aliases: alias_names, refused_aliases, duplicate: before })
}

/// Removes a font [`register`] added (by its id). Returns the families that went with it. Type
/// using them falls back on its next layout; nothing is redrawn until the host edits it or
/// calls [`crate::Host::refresh_fonts`].
pub fn unregister(id: &str) -> Vec<String> {
    photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.unregister_user_font(id)
}

/// `family`'s status now (asks for nothing).
pub fn family_status(family: &str) -> FamilyStatus {
    if photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.has_family(family) {
        FamilyStatus::Loaded
    } else {
        FamilyStatus::Missing
    }
}

/// Every family type can use now, sorted.
pub fn families() -> Vec<String> {
    photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.families()
}

/// Which families open document `doc`'s type uses, and which are missing.
fn fonts_ready(session: &Session, doc: DocumentId) -> Result<FontsReady> {
    let d = document(session, doc)?;
    let missing: Vec<MissingFamily> = photocraft_engine::type_extra_cmds::missing_font_names(d)
        .into_iter()
        .map(|(family, postscript)| MissingFamily { family, postscript })
        .collect();
    let mut ready: Vec<String> = Vec::new();
    for t in type_layers(d) {
        for r in t.char_runs() {
            let f = r.style.font_family;
            if !f.is_empty() && !ready.contains(&f) && !missing.iter().any(|m| m.family == f) {
                ready.push(f);
            }
        }
    }
    Ok(FontsReady { document: doc, ready, missing })
}

/// Asks `provider` for every family open document `doc` is missing; returns them.
fn request_missing(session: &Session, doc: DocumentId, provider: &dyn FontProvider) -> Result<Vec<MissingFamily>> {
    let r = fonts_ready(session, doc)?;
    for m in &r.missing {
        provider.request(&m.family, &m.postscript);
    }
    Ok(r.missing)
}

/// What [`crate::Host::refresh_fonts`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Refreshed {
    /// Type layers redrawn with the fonts that arrived.
    pub redrawn: Vec<(DocumentId, HostLayerId)>,
    /// Layers of documents a background job holds: call [`crate::Host::refresh_fonts`] again after it ends.
    pub busy: Vec<(DocumentId, HostLayerId)>,
    /// Documents whose missing names were mapped to a registered family spelled differently
    /// (`CCWildWords-Roman` → "CC Wild Words"); one undo step each, a clean document stays clean.
    pub resolved: Vec<DocumentId>,
}

/// After [`register`]ing a batch: in every open document, missing names a registered family now
/// matches loosely are mapped to it (`auto_resolve`), and type layers drawn without their fonts
/// are redrawn (no history step; a clean document stays clean). Layers showing pixels from
/// another app (a PSD's own rendering, which matches no layout of ours) are left as they are,
/// as the editor does: their pixels change only when the person edits the text.
///
/// `waiting` is what [`crate::Host::pending_font_layers`] returned before the fonts were registered (the layers
/// drawn by our fallback then); `None` redraws every type layer that uses `families`.
fn refresh(session: &mut Session, families: &[String], waiting: Option<&[(DocumentId, HostLayerId)]>, auto_resolve: bool) -> Refreshed {
    let mut out = Refreshed::default();
    if auto_resolve {
        let docs: Vec<DocId> =
            session.documents().iter().filter(|d| !photocraft_engine::type_extra_cmds::auto_matches(&d.doc).is_empty()).map(|d| d.doc.id).collect();
        for id in docs {
            let clean = session.documents().iter().find(|d| d.doc.id == id).is_some_and(|d| !d.is_dirty());
            let r = session.execute_scoped("type.resolveMissingFonts", json!({"auto": true, "keepPixels": true}), Some(id), HistoryMode::Step);
            if r.is_ok() {
                if clean && let Ok(scope) = session.begin_scope(Some(id), HistoryMode::Step) {
                    if let Some(st) = session.active_mut() {
                        st.saved_revision = st.revision;
                    }
                    session.end_scope(scope);
                }
                out.resolved.push(id.into());
            }
        }
    }
    let layers: Vec<(DocId, LayerId)> = match waiting {
        Some(w) => w.iter().map(|(d, l)| ((*d).into(), (*l).into())).collect(),
        None => photocraft_engine::type_extra_cmds::layers_using_fonts(session, families, false),
    };
    let busy = session.refresh_type_layers(&layers);
    out.busy = busy.iter().map(|(d, l)| ((*d).into(), (*l).into())).collect();
    out.redrawn = layers.iter().filter(|x| !busy.contains(*x)).map(|(d, l)| ((*d).into(), (*l).into())).collect();
    out
}

/// The type layers (in every open document) that use one of `families` (or PostScript names)
/// and show our own layout now: call it *before* [`register`]ing them and pass the result to
/// [`crate::Host::refresh_fonts`], so only layers drawn by our fallback are redrawn (a PSD's own pixels are kept).
fn pending_layers(session: &Session, families: &[String]) -> Vec<(DocumentId, HostLayerId)> {
    photocraft_engine::type_extra_cmds::layers_using_fonts(session, families, true).into_iter().map(|(d, l)| (d.into(), l.into())).collect()
}

/// `type.layoutDigest` of open document `doc` (one layer, or every type layer): equal digests
/// mean every glyph is placed alike, bit for bit (the same on native and wasm with the same fonts).
fn layout_digest(session: &mut Session, doc: DocumentId, layer: Option<HostLayerId>, font_hashes: bool) -> Result<Value> {
    let mut p = json!({ "fontHashes": font_hashes });
    if let Some(l) = layer {
        p["layer"] = json!(l.0);
    }
    Ok(session.execute_scoped("type.layoutDigest", p, Some(doc.into()), HistoryMode::None)?)
}

/// Who holds which registered font (by id), and which documents wait for fonts.
#[derive(Debug, Default)]
pub(crate) struct FontBook {
    global: HashSet<String>,
    per_document: HashMap<DocumentId, HashSet<String>>,
    /// Documents seen, and whether their [`crate::Event::FontsReady`] went out.
    reported: HashMap<DocumentId, bool>,
    /// Fonts changed since the waiting documents were last checked.
    dirty: bool,
}

impl FontBook {
    /// `font` now belongs to `document`, unless it is global.
    fn hold(&mut self, font: &RegisteredFont, document: Option<DocumentId>) {
        match document {
            None => {
                self.global.insert(font.id.clone());
            }
            // Registered by someone else before (a free `register`, another process part): not ours to
            // unregister, unless a document of ours registered it.
            Some(d) => {
                let ours = !font.duplicate || self.per_document.values().any(|f| f.contains(&font.id));
                if ours && !self.global.contains(&font.id) {
                    self.per_document.entry(d).or_default().insert(font.id.clone());
                }
            }
        }
    }

    /// `document` closed: the fonts only it held are unregistered.
    pub(crate) fn document_closed(&mut self, document: DocumentId) {
        self.reported.remove(&document);
        let Some(held) = self.per_document.remove(&document) else { return };
        for id in held {
            if !self.global.contains(&id) && !self.per_document.values().any(|f| f.contains(&id)) {
                unregister(&id);
            }
        }
    }
}

/// The poll's [`crate::Event::FontsReady`] events: documents seen for the first time with all
/// their fonts, and waiting ones the last registrations completed. A document still opening
/// waits (its layers aren't all there).
pub(crate) fn collect_ready(session: &Session, book: &mut FontBook) -> Vec<crate::Event> {
    let open: Vec<DocumentId> = session.documents().iter().map(|st| st.doc.id.into()).collect();
    book.reported.retain(|d, _| open.contains(d));
    // Closed from the editor's own File menu, not through `Host::close`.
    let gone: Vec<DocumentId> = book.per_document.keys().filter(|d| !open.contains(d)).copied().collect();
    for d in gone {
        book.document_closed(d);
    }
    let dirty = std::mem::take(&mut book.dirty);
    let mut out = Vec::new();
    for d in open {
        if book.reported.get(&d) == Some(&true) || (!dirty && book.reported.contains_key(&d)) || session.job_on(d.into()).is_some() {
            continue;
        }
        let Ok(state) = fonts_ready(session, d) else { continue };
        let done = state.is_ready();
        book.reported.insert(d, done);
        if done {
            out.push(crate::Event::FontsReady { document: d, ready: state.ready });
        }
    }
    out
}

impl crate::Host {
    /// Which families open document `document`'s type uses, and which are missing.
    pub fn fonts_ready(&self, document: DocumentId) -> Result<FontsReady> {
        fonts_ready(self.session(), document)
    }

    /// Asks `provider` for every family open document `document` is missing; returns them.
    pub fn request_missing_fonts(&self, document: DocumentId, provider: &dyn FontProvider) -> Result<Vec<MissingFamily>> {
        request_missing(self.session(), document, provider)
    }

    /// Registers font files (`(file name, bytes)`, or a [`FontFile`] with aliases) and redraws the
    /// type that waited for them, mapping loosely spelled missing names to them (one undo step per
    /// document; a clean document stays clean). Returns each file's outcome, in order, and what
    /// was redrawn.
    pub fn add_fonts<F: Into<FontFile>>(&mut self, files: Vec<F>) -> (Vec<Result<RegisteredFont>>, Refreshed) {
        let (out, refreshed) = self.add_font_files(files);
        for font in out.iter().flatten() {
            self.fonts.hold(font, None);
        }
        (out, refreshed)
    }

    /// [`Self::add_fonts`] for one document: the files are held by `document` and unregistered
    /// when it closes, unless another document or [`Self::add_fonts`] holds them too. While
    /// registered they are usable by type in any document (the registry is the process's), and
    /// layers waiting for them anywhere are redrawn. A file's aliases go with it. `Err` for a
    /// document that isn't open.
    pub fn add_document_fonts<F: Into<FontFile>>(&mut self, document: DocumentId, files: Vec<F>) -> Result<(Vec<Result<RegisteredFont>>, Refreshed)> {
        self::document(self.session(), document)?;
        let (out, refreshed) = self.add_font_files(files);
        for font in out.iter().flatten() {
            self.fonts.hold(font, Some(document));
        }
        Ok((out, refreshed))
    }

    fn add_font_files<F: Into<FontFile>>(&mut self, files: Vec<F>) -> (Vec<Result<RegisteredFont>>, Refreshed) {
        let prepared: Vec<_> = files.into_iter().map(|f| prepare_file(f.into())).collect();
        // The faces' families and PostScript names and the aliases, to find the layers drawn by
        // our fallback before the new fonts change what "our layout" is.
        let mut names: Vec<String> = Vec::new();
        for (p, aliases) in prepared.iter().flatten() {
            for f in &p.entry.faces {
                names.push(f.family.clone());
                names.extend(f.postscript.clone());
            }
            names.extend(aliases.iter().cloned());
        }
        let waiting = self.pending_font_layers(&names);
        let out: Vec<Result<RegisteredFont>> = prepared.into_iter().map(|p| p.and_then(|(p, aliases)| register_prepared(p, &aliases))).collect();
        let families: Vec<String> = out.iter().flatten().flat_map(|f| f.families.iter().chain(&f.aliases).cloned()).collect();
        let refreshed = self.refresh_fonts(&families, Some(&waiting), true);
        (out, refreshed)
    }

    /// The type layers (in every open document) that use one of `families` (or PostScript
    /// names) and show our own layout now: call it *before* [`register`]ing them and pass the
    /// result to [`Self::refresh_fonts`], so only layers drawn by our fallback are redrawn (a
    /// PSD's own pixels are kept).
    pub fn pending_font_layers(&self, families: &[String]) -> Vec<(DocumentId, HostLayerId)> {
        pending_layers(self.session(), families)
    }

    /// After [`register`]ing a batch: in every open document, missing names a registered family
    /// now matches loosely are mapped to it (`auto_resolve`), and type layers drawn without their
    /// fonts are redrawn (no history step; a clean document stays clean). Layers showing pixels
    /// from another app (a PSD's own rendering) are left as they are, as the editor does.
    /// `waiting` is what [`Self::pending_font_layers`] returned before the fonts were
    /// registered; `None` redraws every type layer that uses `families`.
    pub fn refresh_fonts(&mut self, families: &[String], waiting: Option<&[(DocumentId, HostLayerId)]>, auto_resolve: bool) -> Refreshed {
        self.fonts.dirty = true;
        refresh(self.session_mut(), families, waiting, auto_resolve)
    }

    /// `type.layoutDigest` of open document `document` (one layer, or every type layer): equal
    /// digests mean every glyph is placed alike, bit for bit (the same on native and wasm with
    /// the same fonts and [`use_system_fonts`]`(false)`).
    pub fn layout_digest(&mut self, document: DocumentId, layer: Option<HostLayerId>, font_hashes: bool) -> Result<Value> {
        layout_digest(self.session_mut(), document, layer, font_hashes)
    }
}

fn document(session: &Session, doc: DocumentId) -> Result<&Document> {
    let id: DocId = doc.into();
    session.documents().iter().find(|st| st.doc.id == id).map(|st| st.doc.as_ref()).ok_or(Error::NoDocument(doc))
}

fn type_layers(doc: &Document) -> Vec<&TextLayer> {
    doc.walk()
        .into_iter()
        .filter_map(|(_, _, l)| match &l.content {
            LayerContent::Text(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// Checks a font file without registering it (what [`register`] would refuse, and why).
pub fn check(name: &str, bytes: Vec<u8>) -> Result<Vec<FontFace>> {
    if bytes.is_empty() {
        return Err(bad("the font file is empty"));
    }
    let p = photocraft_text::user_fonts::prepare(name, bytes).map_err(Error::File)?;
    Ok(p.entry
        .faces
        .into_iter()
        .map(|f| FontFace { family: f.family, style: f.style, postscript: f.postscript, weight: f.weight, italic: f.italic })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_font_files_are_errors() {
        assert!(register("x.ttf", Vec::new()).is_err());
        assert!(register("x.ttf", vec![0u8; 64]).is_err());
        assert!(register("x.woff2", b"wOF2....".to_vec()).is_err());
        assert!(check("x.otf", vec![1, 2, 3]).is_err());
        assert!(check("x.otf", Vec::new()).is_err());
    }

    #[test]
    fn document_fonts_go_when_the_last_holder_closes_and_global_ones_stay() {
        let inter = || vec![("Inter.ttf".to_string(), photocraft_text::fonts::INTER_REGULAR.as_slice().to_vec())];
        let has = |id: &str| photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.has_user_font(id);
        let mut host = crate::Host::new();
        let new_doc = |host: &mut crate::Host| {
            host.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
            host.active_document().unwrap()
        };
        let (a, b) = (new_doc(&mut host), new_doc(&mut host));
        let (fonts, _) = host.add_document_fonts(a, inter()).unwrap();
        let id = fonts.first().unwrap().as_ref().unwrap().id.clone();
        assert!(has(&id));
        host.add_document_fonts(b, inter()).unwrap();
        host.close(a, true).unwrap();
        assert!(has(&id), "b still holds it");
        host.close(b, true).unwrap();
        assert!(!has(&id), "the last holder closed");
        // Held globally: stays.
        let c = new_doc(&mut host);
        host.add_document_fonts(c, inter()).unwrap();
        host.add_fonts(inter());
        host.close(c, true).unwrap();
        assert!(has(&id), "global");
        unregister(&id);
        // A document that isn't open.
        assert!(host.add_document_fonts(a, inter()).is_err());
    }

    /// JetBrains Mono (which draws unlike the fallback, Inter) renamed in memory to a fourteen-letter family.
    fn mono(family: &str) -> Vec<u8> {
        let enc = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
        let (from, to) = (enc("JetBrains Mono"), enc(family));
        assert_eq!(from.len(), to.len());
        let mut b = photocraft_text::fonts::JETBRAINS_MONO_REGULAR.clone();
        for i in 0..b.len().saturating_sub(from.len()) {
            if b[i..i + from.len()] == from[..] {
                b[i..i + from.len()].copy_from_slice(&to);
            }
        }
        b
    }

    /// A new document with one type layer asking for `style`.
    fn type_document(host: &mut crate::Host, style: Value) -> (DocumentId, HostLayerId) {
        host.execute("file.new", json!({"width": 256, "height": 64})).unwrap();
        let layer = host.execute("type.create", json!({"text": "Iiii alias", "x": 5, "y": 30, "size": 12})).unwrap()["layer"].clone();
        let mut p = style;
        p["layer"] = layer.clone();
        host.execute("type.setStyle", p).unwrap();
        (host.active_document().unwrap(), HostLayerId(layer.as_u64().unwrap()))
    }

    fn fonts_ready_events(events: &[crate::Event], document: DocumentId) -> usize {
        events.iter().filter(|e| matches!(e, crate::Event::FontsReady { document: d, .. } if *d == document)).count()
    }

    /// The layer's digest, and the SHA-256 of the font files its glyphs use.
    fn layout_of(host: &mut crate::Host, document: DocumentId, layer: HostLayerId) -> (String, Vec<String>) {
        let d = host.layout_digest(document, Some(layer), true).unwrap();
        let fonts = d["fonts"].as_array().unwrap().iter().map(|f| f["sha256"].as_str().unwrap().to_string()).collect();
        (d["digest"].as_str().unwrap().to_string(), fonts)
    }

    #[test]
    fn a_document_asking_for_an_alias_uses_the_file_and_it_goes_with_the_last_holder() {
        let has = |name: &str| photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner).fonts.has_family(name);
        let mut host = crate::Host::new();
        let (doc, layer) = type_document(&mut host, json!({"font": "Scanlation Alias A"}));
        let fallback = layout_of(&mut host, doc, layer);
        let missing = host.fonts_ready(doc).unwrap();
        assert!(!missing.is_ready());
        assert!(missing.missing.iter().any(|m| m.family == "Scanlation Alias A"), "{missing:?}");
        assert_eq!(fonts_ready_events(&host.poll(), doc), 0, "it waits");

        let file = FontFile::new("a.ttf", mono("AliasRealFontA")).with_aliases(vec!["Scanlation Alias A".into()]);
        let (out, refreshed) = host.add_document_fonts(doc, vec![file.clone()]).unwrap();
        let font = out.into_iter().next().unwrap().unwrap();
        assert_eq!(font.families, vec!["AliasRealFontA".to_string()]);
        assert_eq!(font.aliases, vec!["Scanlation Alias A".to_string()]);
        assert_eq!(refreshed.redrawn, vec![(doc, layer)], "the layer drawn by the fallback");
        assert!(host.fonts_ready(doc).unwrap().is_ready());
        assert_eq!(fonts_ready_events(&host.poll(), doc), 1);
        struct Nothing;
        impl FontProvider for Nothing {
            fn request(&self, family: &str, _: &[String]) {
                panic!("{family} isn't missing");
            }
        }
        assert!(host.request_missing_fonts(doc, &Nothing).unwrap().is_empty());

        let by_alias = layout_of(&mut host, doc, layer);
        assert_eq!(by_alias.1, vec![font.id.clone()], "the registered file draws it");
        assert_ne!(by_alias.0, fallback.0, "not the fallback");
        let (real_doc, real_layer) = type_document(&mut host, json!({"font": "AliasRealFontA"}));
        assert_eq!(layout_of(&mut host, real_doc, real_layer), by_alias, "the same layout as by the real family");

        // A second holder: the alias stays until the last one closes, and then goes with the file.
        host.add_document_fonts(real_doc, vec![file]).unwrap();
        host.close(doc, true).unwrap();
        assert!(has("Scanlation Alias A") && has("AliasRealFontA"));
        host.close(real_doc, true).unwrap();
        assert!(!has("Scanlation Alias A"), "the alias went");
        assert!(!has("AliasRealFontA"), "the file went");
    }

    #[test]
    fn an_old_postscript_name_as_an_alias_counts_and_draws() {
        let mut host = crate::Host::new();
        let (doc, layer) = type_document(&mut host, json!({"font": "OldAliasFam", "postscriptName": "OldAliasFam-Bold"}));
        assert!(!host.fonts_ready(doc).unwrap().is_ready());
        let file = FontFile::new("b.ttf", mono("AliasRealFontB")).with_aliases(vec!["OldAliasFam-Bold".into()]);
        let (out, refreshed) = host.add_document_fonts(doc, vec![file]).unwrap();
        let font = out.into_iter().next().unwrap().unwrap();
        assert_eq!(refreshed.redrawn, vec![(doc, layer)]);
        assert!(host.fonts_ready(doc).unwrap().is_ready());
        assert_eq!(fonts_ready_events(&host.poll(), doc), 1);
        assert_eq!(layout_of(&mut host, doc, layer).1, vec![font.id]);
        host.close(doc, true).unwrap();
    }

    #[test]
    fn aliases_are_bounded_and_never_take_another_font() {
        let mut host = crate::Host::new();
        let too_many = FontFile::new("c.ttf", mono("AliasRealFontC")).with_aliases(vec!["x".into(); MAX_FONT_ALIASES + 1]);
        let too_long = FontFile::new("c.ttf", mono("AliasRealFontC")).with_aliases(vec!["x".repeat(MAX_FONT_ALIAS_CHARS + 1)]);
        let (out, _) = host.add_fonts(vec![too_many, too_long]);
        assert!(out.iter().all(|r| matches!(r, Err(Error::File(_)))), "{out:?}");

        let taken = FontFile::new("c.ttf", mono("AliasRealFontC")).with_aliases(vec!["Inter".into(), String::new()]);
        let (out, _) = host.add_fonts(vec![taken]);
        let font = out.into_iter().next().unwrap().unwrap();
        assert!(font.aliases.is_empty());
        assert_eq!(font.refused_aliases, vec!["Inter".to_string()], "an alias it didn't get is said");
        unregister(&font.id);
        assert_eq!(family_status("Inter"), FamilyStatus::Loaded);

        // The free function, for a worker thread; `(&str, bytes)` and `(String, bytes)` still work.
        let free = register_file(FontFile::new("c.ttf", mono("AliasRealFontC")).with_aliases(vec!["Free Alias C".into()])).unwrap();
        assert_eq!(family_status("Free Alias C"), FamilyStatus::Loaded);
        assert_eq!(unregister(&free.id).iter().filter(|f| *f == "Free Alias C").count(), 1);
        assert_eq!(family_status("Free Alias C"), FamilyStatus::Missing);
        let (out, _) = host.add_fonts(vec![("c.ttf", vec![1u8, 2])]);
        assert!(out[0].is_err());
    }

    #[test]
    fn fonts_ready_comes_once_and_only_with_every_family_there() {
        let mut host = crate::Host::new();
        host.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        let plain = host.active_document().unwrap();
        let ready = |events: &[crate::Event]| events.iter().filter(|e| matches!(e, crate::Event::FontsReady { .. })).count();
        assert_eq!(ready(&host.poll()), 1, "no type: ready at once");
        assert_eq!(ready(&host.poll()), 0, "once");
        host.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        let layer = host.execute("type.create", json!({"text": "HI", "x": 5, "y": 30, "size": 12})).unwrap()["layer"].clone();
        host.execute("type.setStyle", json!({"layer": layer, "fontFamily": "No Such Family Anywhere"})).ok();
        let waiting = host.active_document().unwrap();
        if !host.fonts_ready(waiting).unwrap().is_ready() {
            assert_eq!(ready(&host.poll()), 0, "a family is missing");
            assert_eq!(ready(&host.poll()), 0);
        }
        host.close(plain, true).unwrap();
    }

    #[test]
    fn a_bundled_font_registers_once_and_unregisters() {
        // Any font the build bundles (craft-fonts); without them there is nothing to try.
        let Some((name, bytes)) = photocraft_text::CRAFT_FONTS.first().map(|f| (format!("{}.ttf", f.family), f.bytes.to_vec())) else { return };
        let a = register(&name, bytes.clone()).unwrap();
        assert!(!a.families.is_empty());
        let b = register(&name, bytes).unwrap();
        assert!(b.duplicate);
        assert_eq!(a.id, b.id);
        assert_eq!(family_status(&a.families[0]), FamilyStatus::Loaded);
        unregister(&a.id);
    }

    #[test]
    fn documents_without_type_are_ready_and_unknown_ones_are_errors() {
        let mut s = Session::new();
        assert!(fonts_ready(&s, DocumentId(u64::MAX)).is_err());
        let d = photocraft_doc::Document::new("t", photocraft_doc::Size::new(4, 4), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8);
        let i = s.add_document(d, None);
        let id: DocumentId = s.documents()[i].doc.id.into();
        let r = fonts_ready(&s, id).unwrap();
        assert!(r.is_ready() && r.ready.is_empty());
        let none = refresh(&mut s, &["Nope".into()], None, true);
        assert!(none.redrawn.is_empty() && none.resolved.is_empty());
        assert!(pending_layers(&s, &[]).is_empty());
    }
}
