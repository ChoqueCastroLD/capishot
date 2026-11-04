//! Opening documents from bytes and saving them to bytes: PSD/PSB (incremental: layers
//! unchanged since the document's last PSD save aren't encoded again, and the bytes are a full
//! save's), `.pcraft`, and the flat formats. No file system: the host owns where bytes come
//! from and go (its server, a cache, a download), the same on native and wasm.
//!
//! Saving is split so a big PSD can be encoded off the editor's thread: [`begin_save`] takes a
//! snapshot (cheap), [`PendingSave::encode`] does the work anywhere, [`finish_save`] marks the
//! document saved at the snapshot's revision (later edits keep it dirty). [`save_bytes`] does
//! all three in a row.

use std::sync::Arc;

use photocraft_doc::Document;
use photocraft_engine::{HistoryMode, Session};

use crate::error::bad;
use crate::{DocumentId, Error, Result};

/// Encoded bytes of one document at most (PSB can be larger, but a host holding more than this
/// in one buffer is a bug on one side or the other): 4 GB.
pub const MAX_FILE_BYTES: usize = 4 << 30;

/// How to open bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenBytesOptions {
    /// The document's name; its extension picks the decoder when the bytes don't tell
    /// (`page.psd`). Empty: `document`.
    pub name: String,
    /// Open as a hidden document (never active, not shown; [`crate`]'s view switches to it in
    /// one frame later). Otherwise it becomes the active document.
    pub hidden: bool,
}

impl Default for OpenBytesOptions {
    fn default() -> Self {
        OpenBytesOptions { name: "document.psd".into(), hidden: false }
    }
}

/// A document opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// Its id.
    pub document: DocumentId,
    /// Human-readable notes about anything approximated or dropped while decoding.
    pub warnings: Vec<String>,
    /// The PSD had damaged data that was recovered around (the document is usable; tell the
    /// person before overwriting the original).
    pub damaged: bool,
    /// Font families its type layers use that no registered font provides yet
    /// ([`crate::fonts`]): register them and the type is redrawn with them.
    pub missing_fonts: Vec<String>,
}

/// A file format to save as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SaveFormat {
    /// Layered Photoshop document (PSB is chosen automatically past PSD's 30 000 px limit).
    Psd,
    /// Large document format, always.
    Psb,
    /// Capishot's own layered format (everything kept).
    Pcraft,
    /// Flat PNG.
    Png,
    /// Flat JPEG at `quality` 1..=100.
    Jpeg {
        /// 1..=100.
        quality: u8,
    },
    /// Flat lossless WebP.
    Webp,
    /// Flat lossy WebP at `quality` 1..=100.
    WebpLossy {
        /// 1..=100.
        quality: u8,
    },
    /// TIFF (flat).
    Tiff,
}

impl SaveFormat {
    /// The file extension the exporter chooses the encoder by.
    pub fn extension(self) -> &'static str {
        match self {
            SaveFormat::Psd => "psd",
            SaveFormat::Psb => "psb",
            SaveFormat::Pcraft => photocraft_format::EXTENSION,
            SaveFormat::Png => "png",
            SaveFormat::Jpeg { .. } => "jpg",
            SaveFormat::Webp | SaveFormat::WebpLossy { .. } => "webp",
            SaveFormat::Tiff => "tif",
        }
    }

    /// `psd`, `psb`, `pcraft`, `png`, `jpg`/`jpeg`, `webp` or `tif`/`tiff` (any case, leading
    /// dot allowed); lossy formats at their default quality (JPEG 90, WebP lossless).
    pub fn parse(s: &str) -> Result<SaveFormat> {
        let f = s.trim().trim_start_matches('.').to_ascii_lowercase();
        Ok(match f.as_str() {
            "psd" => SaveFormat::Psd,
            "psb" => SaveFormat::Psb,
            "pcraft" => SaveFormat::Pcraft,
            "png" => SaveFormat::Png,
            "jpg" | "jpeg" => SaveFormat::Jpeg { quality: 90 },
            "webp" => SaveFormat::Webp,
            "tif" | "tiff" => SaveFormat::Tiff,
            _ => return Err(bad("save", format!("`{s}` isn't a format documents save as (psd, psb, pcraft, png, jpg, webp, tiff)"))),
        })
    }

    /// Keeps the layers (saving it marks the document saved).
    pub fn is_layered(self) -> bool {
        matches!(self, SaveFormat::Psd | SaveFormat::Psb | SaveFormat::Pcraft)
    }
}

/// How to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveBytesOptions {
    /// The format.
    pub format: SaveFormat,
    /// PSD/PSB: write the real merged image (Photoshop's Maximize Compatibility). Off saves
    /// time and bytes, but other apps then see a blank preview.
    pub maximize_compatibility: bool,
    /// PSD/PSB: embed a thumbnail.
    pub image_previews: bool,
    /// A layered save marks the document saved (not dirty). Off for a copy (a backup, an
    /// upload of a draft) that shouldn't change what the person sees as unsaved.
    pub mark_saved: bool,
}

impl Default for SaveBytesOptions {
    fn default() -> Self {
        SaveBytesOptions { format: SaveFormat::Psd, maximize_compatibility: true, image_previews: true, mark_saved: true }
    }
}

impl SaveBytesOptions {
    /// Defaults for `format`.
    pub fn new(format: SaveFormat) -> Self {
        SaveBytesOptions { format, ..SaveBytesOptions::default() }
    }
}

/// Saved bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Saved {
    /// The file.
    pub bytes: Vec<u8>,
    /// Human-readable notes about anything the format couldn't keep.
    pub warnings: Vec<String>,
    /// PSD/PSB: layer channel sets taken from the last save unchanged / encoded again (the
    /// incremental save's work); `(0, 0)` for other formats.
    pub reused_layers: usize,
    /// See `reused_layers`.
    pub encoded_layers: usize,
}

impl std::fmt::Debug for Saved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("bytes", &self.bytes.len())
            .field("warnings", &self.warnings)
            .field("reused_layers", &self.reused_layers)
            .field("encoded_layers", &self.encoded_layers)
            .finish()
    }
}

/// Opens `bytes` (PSD, PSB, `.pcraft`, PNG, JPEG, WebP, TIFF, … whatever File › Open reads) as
/// a new document. PSD bytes are decoded in place from the shared buffer (not copied first).
pub fn open_bytes(session: &mut Session, bytes: Arc<Vec<u8>>, opts: &OpenBytesOptions) -> Result<Opened> {
    open_bytes_with(session, bytes, opts, &|| false, &|_| {})
}

/// [`open_bytes`] with a cancel check (polled while decoding: `true` stops with
/// [`Error::Cancelled`], nothing opened) and a progress sink (`0.0..=1.0`). Decoding is the long
/// part; call [`decode`] on a worker and [`add_decoded`] on the editor's thread to keep the
/// editor drawing.
pub fn open_bytes_with(
    session: &mut Session,
    bytes: Arc<Vec<u8>>,
    opts: &OpenBytesOptions,
    cancelled: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Result<Opened> {
    let decoded = decode(bytes, &opts.name, &DecodeSettings::of(session), cancelled, progress)?;
    Ok(add_decoded(session, decoded, opts.hidden))
}

/// Decoded bytes, not open yet ([`decode`] → [`add_decoded`]).
pub struct Decoded {
    document: Document,
    warnings: Vec<String>,
    damaged: bool,
    source_read_only: bool,
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded").field("name", &self.document.name).field("warnings", &self.warnings).field("damaged", &self.damaged).finish()
    }
}

/// The person's import settings (Preferences › File Handling, Camera Raw), taken from a session
/// so [`decode`] can run on any thread.
#[derive(Clone, Debug, Default)]
pub struct DecodeSettings(photocraft_io::ImportOptions);

impl DecodeSettings {
    /// `session`'s import settings.
    pub fn of(session: &Session) -> Self {
        DecodeSettings(photocraft_engine::jobs::import_options(session.prefs()))
    }
}

/// Decodes `bytes` named `name` into a document (no session needed: any thread).
pub fn decode(
    bytes: Arc<Vec<u8>>,
    name: &str,
    settings: &DecodeSettings,
    cancelled: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Result<Decoded> {
    if bytes.is_empty() {
        return Err(Error::File("the file is empty".into()));
    }
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::File(format!("the file is larger than {} GB", MAX_FILE_BYTES >> 30)));
    }
    let name = if name.trim().is_empty() { "document" } else { name };
    let ctl = photocraft_raster::Interrupt::new(cancelled, progress);
    let (r, damaged) = photocraft_io::import_shared_for_open_with(name, &bytes, &settings.0, &ctl)?;
    if r.preview_only {
        // A stand-in picture (an Affinity file whose data couldn't be read): opening it as the
        // document would lose the original on save.
        return Err(Error::File(r.warnings.first().cloned().unwrap_or_else(|| "only a preview of this file could be read".into())));
    }
    let mut document = r.document;
    if document.name.trim().is_empty() {
        document.name = name.to_string();
    }
    Ok(Decoded { document, warnings: r.warnings, damaged, source_read_only: r.source_read_only })
}

/// Adds a [`decode`]d document to the session (active, or `hidden`).
pub fn add_decoded(session: &mut Session, decoded: Decoded, hidden: bool) -> Opened {
    let Decoded { mut document, mut warnings, damaged, source_read_only } = decoded;
    // Preferences › Raw Defaults › Resolution, as File › Open applies it.
    photocraft_engine::file_cmds::raw_defaults(session, &warnings, &mut document);
    let index = if hidden { session.add_hidden_document(document, None) } else { session.add_document(document, None) };
    let (id, missing_fonts) = match session.documents().get(index) {
        Some(st) => (st.doc.id, photocraft_engine::type_extra_cmds::missing_fonts(&st.doc)),
        // `add_*document` returns the index it put it at.
        None => (photocraft_doc::DocId(0), Vec::new()),
    };
    if source_read_only {
        mark(session, id, |st| st.source_read_only = true);
    }
    if let Some(st) = session.documents().get(index) {
        warnings.extend(photocraft_engine::type_extra_cmds::missing_font_warnings(&st.doc));
    }
    Opened { document: id.into(), warnings, damaged, missing_fonts }
}

/// A save in progress: a snapshot of the document and its incremental-save cache. `Send`, so it
/// can be encoded on a worker ([`PendingSave::encode`]).
#[derive(Clone)]
pub struct PendingSave {
    document: DocumentId,
    doc: Arc<Document>,
    revision: u64,
    psd_cache: photocraft_io::PsdExportCache,
    pcraft_cache: photocraft_io::PcraftExportCache,
    opts: SaveBytesOptions,
}

impl std::fmt::Debug for PendingSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSave").field("document", &self.document).field("revision", &self.revision).field("opts", &self.opts).finish()
    }
}

impl PendingSave {
    /// The document being saved.
    pub fn document(&self) -> DocumentId {
        self.document
    }

    /// Encodes the snapshot (the long part; any thread).
    pub fn encode(&self) -> Result<Saved> {
        self.encode_with(&|| false, &|_| {})
    }

    /// [`Self::encode`] with a cancel check and a progress sink (`0.0..=1.0`).
    pub fn encode_with(&self, cancelled: &(dyn Fn() -> bool + Sync), progress: &(dyn Fn(f32) + Sync)) -> Result<Saved> {
        let format = self.opts.format;
        let mut eo = photocraft_io::ExportOptions {
            force_psb: format == SaveFormat::Psb,
            image_previews: self.opts.image_previews,
            psd_maximize_compatibility: self.opts.maximize_compatibility,
            psd_cache: Some(self.psd_cache.clone()),
            pcraft_cache: Some(self.pcraft_cache.clone()),
            // No file system here: linked smart objects keep what they embed.
            read_linked_files: false,
            ..photocraft_io::ExportOptions::default()
        };
        match format {
            SaveFormat::Jpeg { quality } | SaveFormat::WebpLossy { quality } => {
                if !(1..=100).contains(&quality) {
                    return Err(bad("save", format!("quality must be 1 to 100, got {quality}")));
                }
                eo.encode.jpeg_quality = quality;
                eo.encode.webp_quality = quality;
                eo.encode.webp_lossless = false;
            }
            SaveFormat::Webp => eo.encode.webp_lossless = true,
            _ => {}
        }
        let name = export_name(&self.doc.name, format.extension());
        let doc = photocraft_io::named_for_file(&self.doc, &name);
        let ctl = photocraft_raster::Interrupt::new(cancelled, progress);
        let r = photocraft_io::export_with(&doc, &name, &eo, &ctl)?;
        let (reused_layers, encoded_layers) = if matches!(format, SaveFormat::Psd | SaveFormat::Psb) {
            let st = self.psd_cache.stats();
            (st.reused, st.encoded)
        } else {
            (0, 0)
        };
        Ok(Saved { bytes: r.bytes, warnings: r.warnings, reused_layers, encoded_layers })
    }
}

/// `stem.ext` for a document named `doc_name` (its own extension replaced).
fn export_name(doc_name: &str, ext: &str) -> String {
    let stem = doc_name.rsplit_once('.').map_or(doc_name, |(stem, _)| stem);
    let stem = if stem.trim().is_empty() { "document" } else { stem };
    format!("{stem}.{ext}")
}

/// Starts saving open document `doc` (a snapshot; nothing is encoded yet).
pub fn begin_save(session: &Session, doc: DocumentId, opts: &SaveBytesOptions) -> Result<PendingSave> {
    if let SaveFormat::Jpeg { quality } | SaveFormat::WebpLossy { quality } = opts.format
        && !(1..=100).contains(&quality)
    {
        return Err(bad("save", format!("quality must be 1 to 100, got {quality}")));
    }
    let id: photocraft_doc::DocId = doc.into();
    let st = session.documents().iter().find(|st| st.doc.id == id).ok_or(Error::NoDocument(doc))?;
    Ok(PendingSave {
        document: doc,
        doc: st.doc.clone(),
        revision: st.revision,
        psd_cache: st.psd_cache.clone(),
        pcraft_cache: st.pcraft_cache.clone(),
        opts: opts.clone(),
    })
}

/// Ends a save whose bytes the host has stored: a layered save (with `mark_saved`) marks the
/// document saved at the snapshot's revision, so edits made while it encoded keep it dirty. A
/// document closed meanwhile is no error (nothing to mark).
pub fn finish_save(session: &mut Session, pending: &PendingSave) {
    if !(pending.opts.mark_saved && pending.opts.format.is_layered()) {
        return;
    }
    let revision = pending.revision;
    mark(session, pending.document.into(), |st| {
        // Never back: a later save may have finished first.
        if st.saved_revision < revision {
            st.saved_revision = revision;
        }
    });
}

/// [`begin_save`], [`PendingSave::encode`] and [`finish_save`] in a row, on this thread.
pub fn save_bytes(session: &mut Session, doc: DocumentId, opts: &SaveBytesOptions) -> Result<Saved> {
    let pending = begin_save(session, doc, opts)?;
    let saved = pending.encode()?;
    finish_save(session, &pending);
    Ok(saved)
}

/// Is open document `doc` changed since it was opened or last saved (layered)?
pub fn is_dirty(session: &Session, doc: DocumentId) -> Result<bool> {
    let id: photocraft_doc::DocId = doc.into();
    session.documents().iter().find(|st| st.doc.id == id).map(photocraft_engine::DocState::is_dirty).ok_or(Error::NoDocument(doc))
}

/// Closes open document `doc`. Unsaved changes refuse unless `force`.
pub fn close(session: &mut Session, doc: DocumentId, force: bool) -> Result<()> {
    let id: photocraft_doc::DocId = doc.into();
    let index = session.index_of_document(id).ok_or(Error::NoDocument(doc))?;
    if !force && session.documents().get(index).is_some_and(photocraft_engine::DocState::is_dirty) {
        return Err(Error::Disabled { command: "close".into(), reason: "the document has unsaved changes (force closes it anyway)".into() });
    }
    session.close(index).map(|_| ()).ok_or(Error::NoDocument(doc))
}

/// Runs `f` on document `id`'s state without a switch anyone sees (the session hands out the
/// active document mutably only: a quiet scope, a hidden document too).
fn mark(session: &mut Session, id: photocraft_doc::DocId, f: impl FnOnce(&mut photocraft_engine::DocState)) {
    let Ok(scope) = session.begin_scope(Some(id), HistoryMode::Step) else { return };
    if let Some(st) = session.active_mut() {
        f(st);
    }
    session.end_scope(scope);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Arc<Vec<u8>> {
        let px = vec![200u8; (w * h * 4) as usize];
        let image = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, px).unwrap();
        Arc::new(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
    }

    #[test]
    fn open_save_psd_and_reopen() {
        let mut s = Session::new();
        let o = open_bytes(&mut s, png(16, 8), &OpenBytesOptions { name: "page.png".into(), hidden: false }).unwrap();
        assert!(!o.damaged);
        let first = save_bytes(&mut s, o.document, &SaveBytesOptions::default()).unwrap();
        assert!(first.bytes.starts_with(b"8BPS"));
        assert!(!is_dirty(&s, o.document).unwrap());
        // Nothing changed: the second save reuses every layer and writes the same bytes.
        let second = save_bytes(&mut s, o.document, &SaveBytesOptions::default()).unwrap();
        assert_eq!(second.encoded_layers, 0);
        assert_eq!(first.bytes, second.bytes);
        let again = open_bytes(&mut s, Arc::new(second.bytes), &OpenBytesOptions { name: "page.psd".into(), hidden: true }).unwrap();
        assert_ne!(again.document, o.document);
        let st = s.documents().iter().find(|st| DocumentId::from(st.doc.id) == again.document).unwrap();
        assert_eq!((st.doc.size.width, st.doc.size.height), (16, 8));
        assert!(st.hidden);
    }

    #[test]
    fn flat_and_native_formats() {
        let mut s = Session::new();
        let o = open_bytes(&mut s, png(4, 4), &OpenBytesOptions::default()).unwrap();
        for f in ["png", "jpg", "webp", "tiff", "pcraft", "psb"] {
            let saved = save_bytes(&mut s, o.document, &SaveBytesOptions::new(SaveFormat::parse(f).unwrap())).unwrap();
            assert!(!saved.bytes.is_empty(), "{f}");
        }
    }

    #[test]
    fn bad_input_is_an_error() {
        let mut s = Session::new();
        assert!(open_bytes(&mut s, Arc::new(Vec::new()), &OpenBytesOptions::default()).is_err());
        assert!(open_bytes(&mut s, Arc::new(b"8BPS garbage".to_vec()), &OpenBytesOptions::default()).is_err());
        assert!(open_bytes(&mut s, Arc::new(vec![0xff; 64]), &OpenBytesOptions { name: String::new(), hidden: false }).is_err());
        assert!(SaveFormat::parse("exe").is_err());
        assert!(save_bytes(&mut s, DocumentId(u64::MAX), &SaveBytesOptions::default()).is_err());
        let o = open_bytes(&mut s, png(4, 4), &OpenBytesOptions::default()).unwrap();
        assert!(save_bytes(&mut s, o.document, &SaveBytesOptions::new(SaveFormat::Jpeg { quality: 0 })).is_err());
        assert!(close(&mut s, DocumentId(u64::MAX), true).is_err());
        close(&mut s, o.document, false).unwrap();
    }

    #[test]
    fn a_cancelled_open_opens_nothing() {
        let mut s = Session::new();
        let r = open_bytes_with(&mut s, png(4, 4), &OpenBytesOptions::default(), &|| true, &|_| {});
        assert_eq!(r, Err(Error::Cancelled));
        assert!(s.documents().is_empty());
    }

    #[test]
    fn edits_during_a_background_save_keep_the_document_dirty() {
        let mut s = Session::new();
        let o = open_bytes(&mut s, png(4, 4), &OpenBytesOptions::default()).unwrap();
        let pending = begin_save(&s, o.document, &SaveBytesOptions::default()).unwrap();
        // An edit lands while the save encodes.
        mark(&mut s, o.document.into(), |st| st.revision += 1);
        pending.encode().unwrap();
        finish_save(&mut s, &pending);
        assert!(is_dirty(&s, o.document).unwrap());
    }
}
