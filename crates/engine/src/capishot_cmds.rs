//! Capishot commands built on [psd-compiler](https://github.com/ChoqueCastroLD/psd-compiler-cli):
//! File › Export › Compile PSD… (the active document) and File › Automate › Compile Folder…
//! (every PSD/PSB of a folder, e.g. a chapter).
//!
//! The browser has no files to write: the web shell runs [`compile_active_with_writer`] with its
//! download writer, and compiles the PSDs the user picks into one ZIP with
//! [`compile_files_to_zip`]. Type is re-rendered with the fonts the app holds
//! ([`photocraft_io::psdc::CompileOptions::with_app_fonts`]).

use rayon::prelude::*;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::file_cmds::{f64_param, str_param};
use crate::{EngineError, Result, Session};

const COMPILE: &str = "file.export.compilePsd";
const COMPILE_FOLDER: &str = "file.automate.compileFolder";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: COMPILE,
            label: "Compile PSD…",
            menu: &["File", "Export"],
            shortcut: None,
            params: r##"{"path":file (.png|.jpg|.webp|.tif),"rerenderText":bool=false (re-render type layers from their text),"fonts":[folder]?,"systemFonts":bool=true,"quality":1..100?} → {path,width,height,warnings}"##,
            enabled: crate::vector_cmds::has_doc,
            run: compile_active,
            journal: true,
        },
        CommandSpec {
            id: COMPILE_FOLDER,
            label: "Compile Folder…",
            menu: &["File", "Automate"],
            shortcut: None,
            params: r##"{"input":folder (its .psd/.psb files),"output":folder,"format":"png|jpg|webp|tif"="png","rerenderText":bool=true,"fonts":[folder]?,"systemFonts":bool=true,"quality":1..100?} → {files,failed,warnings}"##,
            enabled: crate::commands::always,
            run: compile_folder,
            journal: true,
        },
    ]
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn extension(path: &str) -> String {
    path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn quality(p: &Value, cmd: &str) -> Result<Option<u8>> {
    match f64_param(p, "quality") {
        None => Ok(None),
        Some(q) if (1.0..=100.0).contains(&q) => Ok(Some(q.round() as u8)),
        Some(q) => Err(bad(cmd, format!("quality {q} is not in 1..100"))),
    }
}

/// The compile options of `p`; re-rendered type also gets the app's fonts.
fn options(p: &Value, rerender_default: bool, cmd: &str) -> Result<photocraft_io::psdc::CompileOptions> {
    let font_dirs = match p.get("fonts") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.into()],
        Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(Into::into).ok_or_else(|| bad(cmd, "\"fonts\" must be folder paths"))).collect::<Result<_>>()?,
        Some(_) => return Err(bad(cmd, "\"fonts\" must be a folder or a list of folders")),
    };
    let opts = photocraft_io::psdc::CompileOptions {
        rerender_text: p.get("rerenderText").and_then(Value::as_bool).unwrap_or(rerender_default),
        font_dirs,
        system_fonts: p.get("systemFonts").and_then(Value::as_bool).unwrap_or(true),
        ..Default::default()
    };
    Ok(if opts.rerender_text { opts.with_app_fonts() } else { opts })
}

fn check_format(ext: &str, cmd: &str) -> Result<()> {
    if matches!(ext, "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff") { Ok(()) } else { Err(bad(cmd, format!("can't compile to \"{ext}\": use png, jpg, webp or tif"))) }
}

fn authorize(s: &Session, cmd: &str, p: &Value) -> Result<()> {
    match s.authorize {
        Some(authorize) => authorize(cmd, p),
        None => Ok(()),
    }
}

fn compile_active(s: &mut Session, p: &Value) -> Result<Value> {
    compile_active_with_writer(s, p, &mut crate::file_cmds::write_file)
}

/// File › Export › Compile PSD… through a platform writer: a file on the desktop, a download in
/// the browser (where `path` is the download's name).
pub fn compile_active_with_writer(s: &mut Session, p: &Value, write: &mut dyn FnMut(&str, &[u8]) -> Result<()>) -> Result<Value> {
    let job = prepare_compile_active(s, p)?;
    let (path, bytes, result) = job.run()?;
    write(&path, &bytes)?;
    Ok(result)
}

/// Compile PSD of the active document, ready to run anywhere ([`CompileActive::run`]): checked
/// and captured here, rendered and encoded later (on a worker, so the page keeps drawing).
pub struct CompileActive {
    doc: std::sync::Arc<photocraft_doc::Document>,
    path: String,
    ext: String,
    quality: Option<u8>,
    opts: photocraft_io::psdc::CompileOptions,
}

/// Checks Compile PSD's params `p` and captures the active document (its moved selection as the
/// canvas shows it, as [`Session::execute`] does before running the command).
pub fn prepare_compile_active(s: &mut Session, p: &Value) -> Result<CompileActive> {
    authorize(s, COMPILE, p)?;
    crate::float_cmds::before_command(s, COMPILE)?;
    let path = str_param(p, "path", COMPILE)?.to_string();
    let ext = extension(&path);
    check_format(&ext, COMPILE)?;
    let quality = quality(p, COMPILE)?;
    let opts = options(p, false, COMPILE)?;
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    Ok(CompileActive { doc, path, ext, quality, opts })
}

impl CompileActive {
    /// Renders and encodes: the file's path, its bytes and the command's result.
    pub fn run(self) -> Result<(String, Vec<u8>, Value)> {
        let c = photocraft_io::psdc::compile_document(&self.doc, &self.opts).map_err(|e| EngineError::Other(e.to_string()))?;
        let bytes = c.encode(&self.ext, self.quality).map_err(|e| EngineError::Other(e.to_string()))?;
        let result = json!({"path": self.path, "width": c.width, "height": c.height, "warnings": c.warnings});
        Ok((self.path, bytes, result))
    }
}

/// The output format of a batch compile (`format`, default png).
fn batch_format(p: &Value, cmd: &str) -> Result<String> {
    let ext = p.get("format").and_then(Value::as_str).unwrap_or("png").trim_start_matches('.').to_ascii_lowercase();
    check_format(&ext, cmd)?;
    Ok(ext)
}

fn is_psd(name: &str) -> bool {
    matches!(extension(name).as_str(), "psd" | "psb")
}

/// The file name of `path` without folders or extension (`page` when there is none).
fn stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(a, _)| a);
    if stem.is_empty() { "page".into() } else { stem.to_string() }
}

fn compile_folder(_: &mut Session, p: &Value) -> Result<Value> {
    let input = str_param(p, "input", COMPILE_FOLDER)?.to_string();
    let output = str_param(p, "output", COMPILE_FOLDER)?.to_string();
    let ext = batch_format(p, COMPILE_FOLDER)?;
    let quality = quality(p, COMPILE_FOLDER)?;
    let opts = options(p, true, COMPILE_FOLDER)?;
    let rd = std::fs::read_dir(&input).map_err(|e| EngineError::Other(format!("{input}: {e}")))?;
    let mut sources: Vec<std::path::PathBuf> =
        rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|f| f.is_file() && f.to_str().is_some_and(is_psd)).collect();
    sources.sort();
    std::fs::create_dir_all(&output).map_err(|e| EngineError::Other(format!("{output}: {e}")))?;
    // Fonts are loaded once, then the pages compile on all cores.
    let compiler = photocraft_io::psdc::Compiler::new(&opts);
    let results: Vec<std::result::Result<(String, Vec<String>), String>> = sources
        .par_iter()
        .map(|src| {
            let name = src.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "page".into());
            let out = crate::file_cmds::join(&output, &format!("{name}.{ext}"));
            let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
            let c = compiler.compile(&bytes).map_err(|e| format!("{}: {e}", src.display()))?;
            let encoded = c.encode(&ext, quality).map_err(|e| format!("{}: {e}", src.display()))?;
            crate::file_cmds::write_file(&out, &encoded).map_err(|e| format!("{out}: {e}"))?;
            Ok((out, c.warnings.into_iter().map(|w| format!("{name}: {w}")).collect()))
        })
        .collect();
    let (mut files, mut failed, mut warnings) = (Vec::new(), Vec::new(), Vec::new());
    for r in results {
        match r {
            Ok((f, w)) => {
                files.push(f);
                warnings.extend(w);
            }
            Err(e) => failed.push(e),
        }
    }
    Ok(json!({"files": files, "failed": failed, "warnings": warnings}))
}

/// A compiled page: its encoded image and its warnings.
type Page = (Vec<u8>, Vec<String>);

/// File › Automate › Compile Folder… for files the user picked rather than a folder (the
/// browser): compiles every PSD/PSB of `files` (name, contents) into one ZIP of pages named
/// `<stem>.<format>`, in name order. `p` takes Compile Folder's options (`format`,
/// `rerenderText`, `quality`; no `input`/`output`). Returns the ZIP and `{files, failed,
/// warnings}`; an error when no page compiled.
pub fn compile_files_to_zip(s: &Session, files: &[(String, Vec<u8>)], p: &Value) -> Result<(Vec<u8>, Value)> {
    authorize(s, COMPILE_FOLDER, p)?;
    let ext = batch_format(p, COMPILE_FOLDER)?;
    let quality = quality(p, COMPILE_FOLDER)?;
    let opts = options(p, true, COMPILE_FOLDER)?;
    let mut sources: Vec<&(String, Vec<u8>)> = files.iter().filter(|(name, _)| is_psd(name)).collect();
    let skipped: Vec<String> = files.iter().filter(|(name, _)| !is_psd(name)).map(|(name, _)| format!("{name}: not a PSD or PSB file")).collect();
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    // Unique entry names: two picks named alike get `name-2`, `name-3`…
    let mut taken = std::collections::HashSet::new();
    let entries: Vec<String> = sources
        .iter()
        .map(|(name, _)| {
            let base = crate::file_cmds::sanitize(&stem(name));
            let mut entry = format!("{base}.{ext}");
            let mut n = 1;
            while !taken.insert(entry.to_lowercase()) {
                n += 1;
                entry = format!("{base}-{n}.{ext}");
            }
            entry
        })
        .collect();
    let compiler = photocraft_io::psdc::Compiler::new(&opts);
    let results: Vec<std::result::Result<Page, String>> = sources
        .par_iter()
        .map(|(name, bytes)| {
            let c = compiler.compile(bytes).map_err(|e| format!("{name}: {e}"))?;
            let encoded = c.encode(&ext, quality).map_err(|e| format!("{name}: {e}"))?;
            Ok((encoded, c.warnings.into_iter().map(|w| format!("{}: {w}", stem(name))).collect()))
        })
        .collect();
    let mut zip = photocraft_format::zip::ZipWriter::new();
    let (mut written, mut failed, mut warnings) = (Vec::new(), skipped, Vec::new());
    for (entry, r) in entries.into_iter().zip(results) {
        match r {
            Ok((bytes, w)) => {
                zip.add(&entry, &bytes).map_err(|e| EngineError::Other(format!("{entry}: {e}")))?;
                written.push(entry);
                warnings.extend(w);
            }
            Err(e) => failed.push(e),
        }
    }
    if written.is_empty() {
        let why = if failed.is_empty() { "no PSD or PSB files were given".to_string() } else { failed.join("; ") };
        return Err(EngineError::Other(format!("no page compiled: {why}")));
    }
    let zip = zip.finish().map_err(|e| EngineError::Other(e.to_string()))?;
    Ok((zip, json!({"files": written, "failed": failed, "warnings": warnings})))
}

#[cfg(test)]
mod tests;
