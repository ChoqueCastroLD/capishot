//! Capishot commands built on [psd-compiler](https://github.com/ChoqueCastroLD/psd-compiler-cli):
//! File › Export › Compile PSD… (the active document) and File › Automate › Compile Folder…
//! (every PSD/PSB of a folder, e.g. a chapter). Desktop only.

use serde_json::{Value, json};

use crate::commands::CommandSpec;
#[cfg_attr(target_arch = "wasm32", allow(unused_imports))]
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

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn extension(path: &str) -> String {
    path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn quality(p: &Value, cmd: &str) -> Result<Option<u8>> {
    match f64_param(p, "quality") {
        None => Ok(None),
        Some(q) if (1.0..=100.0).contains(&q) => Ok(Some(q.round() as u8)),
        Some(q) => Err(bad(cmd, format!("quality {q} is not in 1..100"))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn options(p: &Value, rerender_default: bool, cmd: &str) -> Result<photocraft_io::psdc::CompileOptions> {
    let font_dirs = match p.get("fonts") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.into()],
        Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(Into::into).ok_or_else(|| bad(cmd, "\"fonts\" must be folder paths"))).collect::<Result<_>>()?,
        Some(_) => return Err(bad(cmd, "\"fonts\" must be a folder or a list of folders")),
    };
    Ok(photocraft_io::psdc::CompileOptions {
        rerender_text: p.get("rerenderText").and_then(Value::as_bool).unwrap_or(rerender_default),
        font_dirs,
        system_fonts: p.get("systemFonts").and_then(Value::as_bool).unwrap_or(true),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn check_format(ext: &str, cmd: &str) -> Result<()> {
    if matches!(ext, "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff") { Ok(()) } else { Err(bad(cmd, format!("can't compile to \"{ext}\": use png, jpg, webp or tif"))) }
}

#[cfg(not(target_arch = "wasm32"))]
fn compile_active(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path", COMPILE)?.to_string();
    let ext = extension(&path);
    check_format(&ext, COMPILE)?;
    let opts = options(p, false, COMPILE)?;
    let quality = quality(p, COMPILE)?;
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let c = photocraft_io::psdc::compile_document(&doc, &opts).map_err(|e| EngineError::Other(e.to_string()))?;
    let bytes = c.encode(&ext, quality).map_err(|e| EngineError::Other(e.to_string()))?;
    crate::file_cmds::write_file(&path, &bytes)?;
    Ok(json!({"path": path, "width": c.width, "height": c.height, "warnings": c.warnings}))
}

#[cfg(not(target_arch = "wasm32"))]
fn compile_folder(_: &mut Session, p: &Value) -> Result<Value> {
    use rayon::prelude::*;
    let input = str_param(p, "input", COMPILE_FOLDER)?.to_string();
    let output = str_param(p, "output", COMPILE_FOLDER)?.to_string();
    let ext = p.get("format").and_then(Value::as_str).unwrap_or("png").trim_start_matches('.').to_ascii_lowercase();
    check_format(&ext, COMPILE_FOLDER)?;
    let opts = options(p, true, COMPILE_FOLDER)?;
    let quality = quality(p, COMPILE_FOLDER)?;
    let rd = std::fs::read_dir(&input).map_err(|e| EngineError::Other(format!("{input}: {e}")))?;
    let mut sources: Vec<std::path::PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|f| f.is_file() && f.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "psd" | "psb")))
        .collect();
    sources.sort();
    std::fs::create_dir_all(&output).map_err(|e| EngineError::Other(format!("{output}: {e}")))?;
    // Fonts are scanned once, then the pages compile on all cores.
    let results: Vec<std::result::Result<(String, Vec<String>), String>> = sources
        .par_iter()
        .map(|src| {
            let name = src.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "page".into());
            let out = crate::file_cmds::join(&output, &format!("{name}.{ext}"));
            let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
            let c = photocraft_io::psdc::compile(&bytes, &opts).map_err(|e| format!("{}: {e}", src.display()))?;
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

#[cfg(target_arch = "wasm32")]
fn compile_active(_: &mut Session, _: &Value) -> Result<Value> {
    Err(EngineError::Other("Compile PSD needs the desktop app".into()))
}

#[cfg(target_arch = "wasm32")]
fn compile_folder(_: &mut Session, _: &Value) -> Result<Value> {
    Err(EngineError::Other("Compile Folder needs the desktop app".into()))
}

#[cfg(test)]
mod tests;
