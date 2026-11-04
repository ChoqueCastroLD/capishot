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
    /// The families it provides, in order, without repeats.
    pub families: Vec<String>,
    /// It was registered before (same bytes): nothing changed.
    pub duplicate: bool,
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
    register_prepared(prepare(name, bytes)?)
}

fn prepare(name: &str, bytes: Vec<u8>) -> Result<photocraft_text::user_fonts::Prepared> {
    photocraft_text::user_fonts::prepare(name, bytes).map_err(|why| Error::File(format!("{}: {why}", photocraft_text::user_fonts::file_name(name))))
}

fn register_prepared(p: photocraft_text::user_fonts::Prepared) -> Result<RegisteredFont> {
    let faces: Vec<FontFace> = p
        .entry
        .faces
        .iter()
        .map(|f| FontFace { family: f.family.clone(), style: f.style.clone(), postscript: f.postscript.clone(), weight: f.weight, italic: f.italic })
        .collect();
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
    let before = eng.fonts.has_user_font(&p.entry.id);
    let families = eng.fonts.register_user_font(&p.entry.id, Arc::clone(&p.data));
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
    Ok(RegisteredFont { id: p.entry.id, name: p.entry.name, faces, families: unique, duplicate: before })
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

impl crate::Host {
    /// Which families open document `document`'s type uses, and which are missing.
    pub fn fonts_ready(&self, document: DocumentId) -> Result<FontsReady> {
        fonts_ready(self.session(), document)
    }

    /// Asks `provider` for every family open document `document` is missing; returns them.
    pub fn request_missing_fonts(&self, document: DocumentId, provider: &dyn FontProvider) -> Result<Vec<MissingFamily>> {
        request_missing(self.session(), document, provider)
    }

    /// Registers font files (`(file name, bytes)`) and redraws the type that waited for them,
    /// mapping loosely spelled missing names to them (one undo step per document; a clean
    /// document stays clean). Returns each file's outcome, in order, and what was redrawn.
    pub fn add_fonts(&mut self, files: Vec<(String, Vec<u8>)>) -> (Vec<Result<RegisteredFont>>, Refreshed) {
        let prepared: Vec<Result<photocraft_text::user_fonts::Prepared>> = files.into_iter().map(|(name, bytes)| prepare(&name, bytes)).collect();
        // The faces' families and PostScript names, to find the layers drawn by our fallback
        // before the new fonts change what "our layout" is.
        let mut names: Vec<String> = Vec::new();
        for f in prepared.iter().flatten().flat_map(|p| &p.entry.faces) {
            names.push(f.family.clone());
            names.extend(f.postscript.clone());
        }
        let waiting = self.pending_font_layers(&names);
        let out: Vec<Result<RegisteredFont>> = prepared.into_iter().map(|p| p.and_then(register_prepared)).collect();
        let families: Vec<String> = out.iter().flatten().flat_map(|f| f.families.iter().cloned()).collect();
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
