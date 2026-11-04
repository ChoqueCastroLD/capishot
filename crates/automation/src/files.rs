//! File I/O shared by the MCP server and the CLI: open/save any supported
//! format, with `.pcraft` (ZIP or directory bundle) handled natively.

use std::path::Path;

use photocraft_doc::Document;
use photocraft_format::{PcraftWriter, SaveOptions};
use photocraft_io::ExportOptions;

use crate::AutomationError;

/// Result of opening a file.
pub struct Opened {
    pub document: Document,
    pub warnings: Vec<String>,
    pub source_read_only: bool,
}

/// Decode a document that was read through a filesystem capability.
pub fn open_bytes(name: &str, bytes: &[u8]) -> Result<Opened, AutomationError> {
    if Path::new(name).extension().is_some_and(|extension| extension.eq_ignore_ascii_case(photocraft_format::EXTENSION)) {
        let document = photocraft_format::load_from_bytes(bytes)?;
        return Ok(Opened { warnings: photocraft_io::open_warnings(&document), document, source_read_only: false });
    }
    let r = photocraft_io::import(name, bytes)?;
    Ok(Opened { document: r.document, warnings: r.warnings, source_read_only: r.source_read_only })
}

/// Open a document from disk. Directory bundles and `.pcraft` ZIPs load
/// natively; everything else goes through `photocraft-io` (PSD, PNG, …).
pub fn open(path: &Path) -> Result<Opened, AutomationError> {
    if path.is_dir() {
        let document = photocraft_format::load_path(path)?;
        return Ok(Opened { warnings: photocraft_io::open_warnings(&document), document, source_read_only: false });
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // A PSD/PSB is parsed from the open file (channel data read as it is decoded), so a CLI
    // convert or info of a 2 GB file does not hold the file in memory next to the document.
    #[cfg(any(unix, windows))]
    {
        let r =
            photocraft_io::import_file(&name, path, &photocraft_io::ImportOptions::default(), &photocraft_raster::Interrupt::NONE).map_err(|e| match e {
                photocraft_io::IoError::Read(msg) => AutomationError::Io(msg),
                e => e.into(),
            })?;
        Ok(Opened { document: r.document, warnings: r.warnings, source_read_only: r.source_read_only })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let bytes = photocraft_format::read_file(path).map_err(|e| AutomationError::Io(format!("{}: {e}", path.display())))?;
        open_bytes(&name, &bytes)
    }
}

/// Previews for a `.pcraft` bundle.
pub fn previews(doc: &Document) -> SaveOptions {
    SaveOptions { thumbnail: Some(photocraft_compose::thumbnail(doc, 256)), composite: Some(photocraft_compose::thumbnail(doc, 1024)) }
}

/// Encode the name the saved native bundle will report on reopening. Save As updates
/// the session identity only *after* a successful write; without a separate snapshot
/// the manifest serializes the previous name instead.
fn named_for_output(doc: &Document, path: &str) -> Document {
    let mut named = doc.clone();
    if let Some(name) = path.rsplit(['/', '\\']).next().filter(|name| !name.is_empty()) {
        named.name = name.to_owned();
    }
    named
}

/// Save or export `doc` to `path`, choosing the format from the extension
/// (or `format_override`, an extension such as `"png"`). `writer` makes
/// repeated `.pcraft` saves incremental. Returns warnings about lost data.
pub fn save(
    doc: &Document,
    path: &Path,
    format_override: Option<&str>,
    opts: &ExportOptions,
    writer: Option<&mut PcraftWriter>,
) -> Result<Vec<String>, AutomationError> {
    let ext = format_override
        .map(|f| f.trim_start_matches('.').to_ascii_lowercase())
        .or_else(|| path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()))
        .ok_or_else(|| AutomationError::BadRequest(format!("cannot tell the format of `{}`; pass a format", path.display())))?;
    if ext == photocraft_format::EXTENSION {
        let named = named_for_output(doc, &path.to_string_lossy());
        let mut local = PcraftWriter::new();
        let w = writer.unwrap_or(&mut local);
        w.save_path(&named, path, &previews(&named))?;
        return Ok(Vec::new());
    }
    let r = photocraft_io::export(doc, &ext, opts)?;
    // Crash-safe: a failed write never destroys the previous file.
    photocraft_format::atomic_write(path, &r.bytes).map_err(|e| AutomationError::Io(e.to_string()))?;
    Ok(r.warnings)
}

/// Encode a document for a capability-scoped write. Unlike [`save`], this
/// never obtains ambient filesystem authority and emits `.pcraft` as a ZIP.
pub fn save_bytes(doc: &Document, name: &str, format_override: Option<&str>, opts: &ExportOptions) -> Result<(Vec<u8>, Vec<String>), AutomationError> {
    let ext = format_override
        .map(|format| format.trim_start_matches('.').to_ascii_lowercase())
        .or_else(|| Path::new(name).extension().map(|extension| extension.to_string_lossy().to_ascii_lowercase()))
        .ok_or_else(|| AutomationError::BadRequest(format!("cannot tell the format of `{name}`; pass a format")))?;
    if ext == photocraft_format::EXTENSION {
        let named = named_for_output(doc, name);
        return Ok((photocraft_format::save_to_bytes(&named, &previews(&named))?, Vec::new()));
    }
    let result = photocraft_io::export(doc, &ext, opts)?;
    Ok((result.bytes, result.warnings))
}

/// A part of the composite: `rect` (document pixels, clipped to the canvas) scaled by `scale`
/// (0 < scale <= 1), with only `layers` drawn when given (see [`only_layers`]). Returns the PNG
/// and its size.
pub fn render_region_png(doc: &Document, rect: photocraft_geom::Rect, scale: f64, layers: Option<&[photocraft_doc::LayerId]>) -> Result<(Vec<u8>, u32, u32), AutomationError> {
    let filtered;
    let doc = match layers {
        Some(ids) => {
            filtered = only_layers(doc, ids)?;
            &filtered
        }
        None => doc,
    };
    let (w, h) = region_size(rect, scale);
    let img = photocraft_compose::render_reduced_rect(doc, rect, w, h).to_rgba8();
    let image = photocraft_codecs::Image::from_u8(img.width, img.height, photocraft_codecs::ChannelLayout::Rgba, img.pixels)
        .map_err(|e| AutomationError::Other(e.to_string()))?;
    let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| AutomationError::Other(e.to_string()))?;
    Ok((png, img.width, img.height))
}

/// The output size of `rect` at `scale`, at least 1×1.
pub fn region_size(rect: photocraft_geom::Rect, scale: f64) -> (u32, u32) {
    let side = |n: u32| ((f64::from(n) * scale).round() as u32).clamp(1, n.max(1));
    (side(rect.width()), side(rect.height()))
}

/// `doc` with only `ids` drawn: each listed layer is shown (with what a listed group holds, as
/// it is), and so is every group around one; every other layer is hidden. An id not in the
/// document is an error.
pub fn only_layers(doc: &Document, ids: &[photocraft_doc::LayerId]) -> Result<Document, AutomationError> {
    use std::collections::HashSet;
    let mut keep: HashSet<photocraft_doc::LayerId> = HashSet::new();
    let mut inside: HashSet<photocraft_doc::LayerId> = HashSet::new();
    for id in ids {
        let path = doc.path_of(*id).ok_or_else(|| AutomationError::BadRequest(format!("no layer {} in the document", id.0)))?;
        for n in 1..=path.len() {
            if let Some(l) = path.get(..n).and_then(|p| doc.layer_at(p)) {
                keep.insert(l.id);
            }
        }
        if let Some(l) = doc.layer(*id) {
            inside.extend(walk_ids(l));
        }
    }
    let mut out = doc.clone();
    let all: Vec<photocraft_doc::LayerId> = doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
    for id in all {
        if let Some(l) = out.layer_mut(id) {
            if ids.contains(&id) {
                l.visible = true;
            } else if !keep.contains(&id) && !inside.contains(&id) {
                l.visible = false;
            }
        }
    }
    Ok(out)
}

/// The ids of the layers inside `l` (a group), at any depth.
fn walk_ids(l: &photocraft_doc::Layer) -> Vec<photocraft_doc::LayerId> {
    let mut out = Vec::new();
    let mut stack: Vec<&photocraft_doc::Layer> = l.children().map(|c| c.iter().collect()).unwrap_or_default();
    while let Some(c) = stack.pop() {
        out.push(c.id);
        if let Some(ch) = c.children() {
            stack.extend(ch.iter());
        }
    }
    out
}

/// Flattened document as PNG, scaled to fit `max_side` (0 = full size).
pub fn render_png(doc: &Document, max_side: u32) -> Result<Vec<u8>, AutomationError> {
    let side = if max_side == 0 { doc.size.width.max(doc.size.height) } else { max_side };
    let img = photocraft_compose::thumbnail(doc, side.max(1));
    let image = photocraft_codecs::Image::from_u8(img.width, img.height, photocraft_codecs::ChannelLayout::Rgba, img.pixels)
        .map_err(|e| AutomationError::Other(e.to_string()))?;
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| AutomationError::Other(e.to_string()))
}
