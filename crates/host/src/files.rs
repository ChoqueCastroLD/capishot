//! Opening documents from bytes and saving them to bytes: PSD/PSB (incremental: layers
//! unchanged since the document's last PSD save aren't encoded again, and the bytes are a full
//! save's), `.pcraft`, and the flat formats. No file system: the host owns where bytes come
//! from and go (its server, a cache, a download), the same on native and wasm.
//!
//! [`crate::Host::open`] / [`crate::Host::save`] work inline; [`crate::Host::start_open`] /
//! [`crate::Host::start_save`] decode or encode on a worker. A save takes a snapshot first and
//! marks the document saved at the snapshot's revision, so edits made while it encodes keep the
//! document dirty.

use std::sync::Arc;

use photocraft_doc::Document;
use photocraft_engine::{HistoryMode, Session};
use photocraft_raster::Interrupt;

use crate::error::bad;
use crate::{DocumentId, Error, Result};

/// Encoded bytes of one document at most (PSB can be larger, but a host holding more than this
/// in one buffer is a bug on one side or the other): 4 GB.
pub const MAX_FILE_BYTES: usize = 4 << 30;

/// [`crate::Host::open`]'s options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// The document's name (a file name; its extension also tells the decoder the format when
    /// the bytes don't). Default `"document.psd"`.
    pub name: Option<String>,
    /// Open as a hidden helper document: reached by id only, the active document left as it is
    /// (the view switches to it in one frame later).
    pub hidden: bool,
}

impl OpenOptions {
    /// Named `name`, shown.
    pub fn named(name: impl Into<String>) -> Self {
        OpenOptions { name: Some(name.into()), hidden: false }
    }

    pub(crate) fn name(&self) -> String {
        self.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("document.psd").to_string()
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
pub struct SaveOptions {
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

impl Default for SaveOptions {
    fn default() -> Self {
        SaveOptions { format: SaveFormat::Psd, maximize_compatibility: true, image_previews: true, mark_saved: true }
    }
}

impl SaveOptions {
    /// Defaults for `format`.
    pub fn new(format: SaveFormat) -> Self {
        SaveOptions { format, ..SaveOptions::default() }
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

/// Decoded bytes, not open yet ([`decode`] → [`add_decoded`]).
pub(crate) struct Decoded {
    document: Document,
    warnings: Vec<String>,
    damaged: bool,
    source_read_only: bool,
}

impl Decoded {
    /// A document made here, not read from a file.
    pub(crate) fn new(document: Document, warnings: Vec<String>) -> Self {
        Decoded { document, warnings, damaged: false, source_read_only: false }
    }
}

/// The person's import settings (Preferences › File Handling, Camera Raw), taken from a session
/// so [`decode`] can run on any thread.
pub(crate) fn decode_settings(session: &Session) -> photocraft_io::ImportOptions {
    photocraft_engine::jobs::import_options(session.prefs())
}

/// Decodes `bytes` (PSD, PSB, `.pcraft`, PNG, JPEG, WebP, TIFF, … whatever File › Open reads)
/// named `name` into a document; no session needed, so any thread. PSD bytes are decoded in
/// place from the shared buffer (not copied first).
pub(crate) fn decode(bytes: &Arc<Vec<u8>>, name: &str, settings: &photocraft_io::ImportOptions, ctl: &Interrupt) -> Result<Decoded> {
    if bytes.is_empty() {
        return Err(Error::File("the file is empty".into()));
    }
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::File(format!("the file is larger than {} GB", MAX_FILE_BYTES >> 30)));
    }
    let (r, damaged) = photocraft_io::import_shared_for_open_with(name, bytes, settings, ctl)?;
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

/// Adds a [`decode`]d document to the session (active, or `hidden`), as File › Open does: Raw
/// Defaults' resolution and the Color Settings policy applied.
pub(crate) fn add_decoded(session: &mut Session, decoded: Decoded, hidden: bool) -> Opened {
    let Decoded { mut document, mut warnings, damaged, source_read_only } = decoded;
    photocraft_engine::file_cmds::raw_defaults(session, &warnings, &mut document);
    let (index, _) = if hidden { session.open_hidden_document(document, None) } else { session.open_document(document, None) };
    let (id, missing_fonts) = match session.documents().get(index) {
        Some(st) => {
            warnings.extend(photocraft_engine::type_extra_cmds::missing_font_warnings(&st.doc));
            (st.doc.id, photocraft_engine::type_extra_cmds::missing_fonts(&st.doc))
        }
        // `open_*document` returns the index it put it at.
        None => (photocraft_doc::DocId(0), Vec::new()),
    };
    if source_read_only {
        mark(session, id, |st| st.source_read_only = true);
    }
    Opened { document: id.into(), warnings, damaged, missing_fonts }
}

/// A save in progress: a snapshot of the document and its incremental-save cache. `Send`, so it
/// can be encoded on a worker ([`PendingSave::encode`]).
#[derive(Clone)]
pub(crate) struct PendingSave {
    document: DocumentId,
    doc: Arc<Document>,
    revision: u64,
    psd_cache: photocraft_io::PsdExportCache,
    pcraft_cache: photocraft_io::PcraftExportCache,
    opts: SaveOptions,
}

impl std::fmt::Debug for PendingSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSave").field("document", &self.document).field("revision", &self.revision).field("opts", &self.opts).finish()
    }
}

impl PendingSave {
    /// Encodes the snapshot (the long part; any thread), with `ctl`'s cancel check and progress.
    pub(crate) fn encode(&self, ctl: &Interrupt) -> Result<Saved> {
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
        let r = photocraft_io::export_with(&doc, &name, &eo, ctl)?;
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
pub(crate) fn begin_save(session: &Session, doc: DocumentId, opts: &SaveOptions) -> Result<PendingSave> {
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
pub(crate) fn finish_save(session: &mut Session, pending: &PendingSave) {
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

/// Runs `f` on document `id`'s state without a switch anyone sees (the session hands out the
/// active document mutably only: a quiet scope, a hidden document too).
fn mark(session: &mut Session, id: photocraft_doc::DocId, f: impl FnOnce(&mut photocraft_engine::DocState)) {
    let Ok(scope) = session.begin_scope(Some(id), HistoryMode::Step) else { return };
    if let Some(st) = session.active_mut() {
        f(st);
    }
    session.end_scope(scope);
}

