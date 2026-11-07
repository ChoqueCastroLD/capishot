//! File › Export › Export Open Documents as ZIP… (`file.export.openDocsZip`): the open documents
//! (or the ones picked, in the order given) flattened as Export As does, encoded as PNG, JPEG or
//! WebP, or saved with their layers as PSD (as File › Save writes one, merged image included),
//! optionally resized and converted to sRGB, and written into one ZIP of pages named after their
//! documents or numbered (`001.png`, `002.png`…).
//!
//! The archive is made a page at a time ([`ZipExport::run`]): each page's entry goes to the sink
//! as soon as it is encoded (a file being written on the desktop, a download growing in the
//! browser), so the whole ZIP is never held in memory. A page that fails is reported and the
//! others are kept; a cancelled export writes nothing (the caller drops what the sink got).

use std::sync::Arc;

use photocraft_doc::Document;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The command id.
pub const ID: &str = "file.export.openDocsZip";

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

/// The image format of the pages.
#[derive(Clone, Debug, PartialEq)]
pub enum Format {
    Png,
    Jpg { quality: u8 },
    Webp { quality: u8, lossless: bool },
    /// Layered PSD (PSB when too big for PSD), with its merged image.
    Psd,
}

impl Format {
    pub fn ext(&self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpg { .. } => "jpg",
            Format::Webp { .. } => "webp",
            Format::Psd => "psd",
        }
    }
}

/// How pages are resized (never enlarged).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resize {
    /// At most this many pixels wide.
    MaxWidth(u32),
    /// This percentage of the size.
    Percent(f64),
}

/// One page of the archive: its entry name and the document it is made of.
#[derive(Clone)]
pub struct Page {
    pub entry: String,
    pub name: String,
    pub doc: Arc<Document>,
    /// The document's file: linked smart objects a PSD embeds are resolved against it.
    pub path: Option<String>,
}

/// An export ready to run anywhere (a worker): checked and captured from the session.
#[derive(Clone)]
pub struct ZipExport {
    pub pages: Vec<Page>,
    pub format: Format,
    pub resize: Option<Resize>,
    pub srgb: bool,
}

/// The entry names of `names` (documents, in order): kept (sanitized, without their extension,
/// unique) or numbered with `prefix`, zero-padded to at least three digits.
pub fn entry_names(names: &[String], ext: &str, sequence: bool, prefix: &str) -> Vec<String> {
    if sequence {
        let width = names.len().to_string().len().max(3);
        let prefix: String = prefix.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control()).collect();
        return (1..=names.len()).map(|i| format!("{prefix}{i:0width$}.{ext}")).collect();
    }
    let mut taken = std::collections::HashSet::new();
    names
        .iter()
        .map(|n| {
            let stem = n.rsplit_once('.').map_or(n.as_str(), |(a, _)| a);
            let base = crate::file_cmds::sanitize(if stem.is_empty() { "page" } else { stem });
            let mut entry = format!("{base}.{ext}");
            let mut k = 1;
            while !taken.insert(entry.to_lowercase()) {
                k += 1;
                entry = format!("{base}-{k}.{ext}");
            }
            entry
        })
        .collect()
}

/// The ZIP's default name: the first document's folder, else "capitulo".
pub fn default_zip_name(s: &Session) -> String {
    let folder = s.documents().first().and_then(|d| d.path.as_deref()).and_then(|p| {
        let dir = p.rsplit_once(['/', '\\']).map(|(d, _)| d)?;
        dir.rsplit(['/', '\\']).next().filter(|f| !f.is_empty() && !f.ends_with(':')).map(str::to_string)
    });
    format!("{}.zip", crate::file_cmds::sanitize(&folder.unwrap_or_else(|| "capitulo".into())))
}

fn quality(p: &Value, default: u8) -> Result<u8> {
    match p.get("quality") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(q) if (1.0..=100.0).contains(&q) => Ok(q.round() as u8),
            _ => Err(bad("\"quality\" must be 1..100")),
        },
    }
}

/// Checks `p` and captures the documents.
pub fn prepare(s: &Session, p: &Value) -> Result<ZipExport> {
    if !p.is_object() && !p.is_null() {
        return Err(bad("params must be a JSON object"));
    }
    let format = match p.get("format").and_then(Value::as_str).unwrap_or("png").to_ascii_lowercase().as_str() {
        "png" => Format::Png,
        "jpg" | "jpeg" => Format::Jpg { quality: quality(p, 90)? },
        "webp" => Format::Webp { quality: quality(p, 90)?, lossless: p.get("lossless").and_then(Value::as_bool).unwrap_or(false) },
        "psd" => Format::Psd,
        f => return Err(bad(format!("unknown format \"{f}\" (png, jpg, webp or psd)"))),
    };
    let resize = match p.get("resize") {
        None | Some(Value::Null) => None,
        Some(r) => {
            if let Some(w) = r.get("maxWidth").and_then(Value::as_f64) {
                if !(1.0..=100_000.0).contains(&w) {
                    return Err(bad("\"resize.maxWidth\" must be 1..100000 px"));
                }
                Some(Resize::MaxWidth(w.round() as u32))
            } else if let Some(pc) = r.get("percent").and_then(Value::as_f64) {
                if !(1.0..=100.0).contains(&pc) {
                    return Err(bad("\"resize.percent\" must be 1..100"));
                }
                Some(Resize::Percent(pc))
            } else {
                return Err(bad("\"resize\" takes {\"maxWidth\": px} or {\"percent\": 1..100}"));
            }
        }
    };
    let srgb = p.get("srgb").and_then(Value::as_bool).unwrap_or(false);
    let docs: Vec<&crate::doc_state::DocState> = match p.get("docs") {
        None | Some(Value::Null) => s.documents().iter().collect(),
        Some(Value::Array(ids)) => {
            let mut out = Vec::new();
            for v in ids {
                let id = v.as_u64().ok_or_else(|| bad("\"docs\" must be document ids"))?;
                let d = s.documents().iter().find(|d| d.doc.id.0 == id).ok_or_else(|| bad(format!("no open document {id}")))?;
                out.push(d);
            }
            out
        }
        Some(_) => return Err(bad("\"docs\" must be a list of document ids")),
    };
    if docs.is_empty() {
        return Err(EngineError::Other("no documents to export".into()));
    }
    let sequence = match p.get("naming").and_then(Value::as_str).unwrap_or("keep") {
        "keep" => false,
        "sequence" => true,
        n => return Err(bad(format!("unknown naming \"{n}\" (keep or sequence)"))),
    };
    let prefix = p.get("prefix").and_then(Value::as_str).unwrap_or("");
    let names: Vec<String> = docs.iter().map(|d| d.doc.name.clone()).collect();
    let entries = entry_names(&names, format.ext(), sequence, prefix);
    let pages = docs.iter().zip(entries).map(|(d, entry)| Page { entry, name: d.doc.name.clone(), doc: d.doc.clone(), path: d.path.clone() }).collect();
    Ok(ZipExport { pages, format, resize, srgb })
}

impl ZipExport {
    /// One page flattened and encoded, or (PSD) saved with its layers; resizing and the sRGB
    /// conversion apply to the whole layered document, as Image Size and Convert to Profile do.
    pub fn render(&self, page: &Page) -> std::result::Result<Vec<u8>, String> {
        let mut doc = (*page.doc).clone();
        let scale = match self.resize {
            Some(Resize::MaxWidth(w)) if doc.size.width > w => Some(w),
            Some(Resize::Percent(pc)) if pc < 100.0 => Some(((f64::from(doc.size.width) * pc / 100.0).round() as u32).max(1)),
            _ => None,
        };
        if scale.is_some() || self.srgb {
            let mut t = Session::new();
            t.add_document(doc, None);
            if let Some(w) = scale {
                t.execute("image.imageSize", json!({"width": w, "resample": "bicubicSharper"})).map_err(|e| e.to_string())?;
            }
            if self.srgb {
                t.execute("edit.convertToProfile", json!({"profile": "srgb", "intent": "relative"})).map_err(|e| e.to_string())?;
            }
            doc = (*t.active().ok_or("the page closed")?.doc).clone();
        }
        let mut opts = photocraft_io::ExportOptions::default();
        match self.format {
            Format::Png => {}
            Format::Jpg { quality } => opts.encode.jpeg_quality = quality,
            Format::Webp { quality, lossless } => {
                opts.encode.webp_quality = quality;
                opts.encode.webp_lossless = lossless;
            }
            Format::Psd => opts.document_path = page.path.as_ref().map(std::path::PathBuf::from),
        }
        // A PNG page is made a band of rows at a time: pages one after another on a phone.
        let out = match self.format {
            Format::Png => photocraft_io::export_png_in_bands(&doc, &opts),
            _ => photocraft_io::export(&doc, &format!("page.{}", self.format.ext()), &opts),
        }
        .map_err(|e| e.to_string())?;
        Ok(out.bytes)
    }

    /// Makes the archive: each page rendered, its entry handed to `sink` at once; `step(i, n)`
    /// before page `i` of `n` returns false to cancel (then [`EngineError::Cancelled`]). Returns
    /// `{files, failed}`; an error when no page could be made.
    pub fn run(&self, step: &mut dyn FnMut(usize, usize) -> bool, sink: &mut dyn FnMut(Vec<u8>) -> Result<()>) -> Result<Value> {
        let mut zip = photocraft_format::zip::ZipWriter::new();
        let (mut files, mut failed) = (Vec::new(), Vec::new());
        let n = self.pages.len();
        for (i, page) in self.pages.iter().enumerate() {
            if !step(i, n) {
                return Err(EngineError::Cancelled);
            }
            match self.render(page) {
                Ok(bytes) => {
                    zip.add(&page.entry, &bytes).map_err(|e| EngineError::Other(format!("{}: {e}", page.entry)))?;
                    sink(zip.take_written())?;
                    files.push(page.entry.clone());
                }
                Err(e) => failed.push(json!({"document": page.name, "error": e})),
            }
        }
        if !step(n, n) {
            return Err(EngineError::Cancelled);
        }
        if files.is_empty() {
            let why: Vec<String> = failed.iter().map(|f| format!("{}: {}", f["document"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or(""))).collect();
            return Err(EngineError::Other(format!("no page could be exported: {}", why.join("; "))));
        }
        sink(zip.finish().map_err(|e| EngineError::Other(e.to_string()))?)?;
        Ok(json!({"files": files, "failed": failed}))
    }
}

/// Writes the archive to `path` as it is made (a `.part` file renamed when complete, removed on
/// failure or cancel: nothing partial is left); `step` as for [`ZipExport::run`].
#[cfg(not(target_arch = "wasm32"))]
pub fn run_to_file(export: &ZipExport, path: &str, step: &mut dyn FnMut(usize, usize) -> bool) -> Result<Value> {
    use std::io::Write;
    let part = format!("{path}.part");
    let mut f = std::fs::File::create(&part).map_err(|e| EngineError::Other(format!("{part}: {e}")))?;
    let r = export.run(step, &mut |chunk| f.write_all(&chunk).map_err(|e| EngineError::Other(format!("{path}: {e}"))));
    let r = r.and_then(|v| f.flush().map(|()| v).map_err(|e| EngineError::Other(format!("{path}: {e}"))));
    drop(f);
    match r {
        Ok(v) => {
            std::fs::rename(&part, path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            Ok(v)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn export_zip(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(authorize) = s.authorize {
        authorize(ID, p)?;
    }
    let export = prepare(s, p)?;
    let path = p.get("path").and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or_else(|| bad("missing \"path\" (the ZIP to write; UIs ask for one)"))?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut v = run_to_file(&export, path, &mut |_, _| true)?;
        v["path"] = json!(path);
        Ok(v)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (export, path);
        Err(EngineError::Other("in the browser the ZIP is a download: run it from the app".into()))
    }
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    if s.documents().is_empty() { Err("no documents open".into()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Export Open Documents as ZIP…",
        menu: &["File", "Export"],
        shortcut: None,
        params: r##"{"docs"?:[document id] (default every open document, in tab order; the pages follow this order),"format":"png|jpg|webp|psd"="png" (psd: each document with its layers and merged image, as Save writes it),"quality"?:1..100 (JPEG, lossy WebP; default 90),"lossless"?:bool (WebP),"resize"?:{"maxWidth":px}|{"percent":1..100} (never enlarges),"srgb"?:bool (convert to sRGB),"naming":"keep|sequence"="keep" (the document names, or 001, 002… in order),"prefix"?:str (before the numbers),"path":file.zip} → {path,files,failed:[{document,error}]}. Each document flattened as Export As does (psd: layered), one page per document; the ZIP is written as the pages are made. A page that fails is reported in "failed" and the others are kept"##,
        enabled,
        journal: true,
        run: export_zip,
    }]
}

#[cfg(test)]
#[path = "zip_export_cmds/tests.rs"]
mod tests;
