//! Front ends of Capishot's psd-compiler commands (`photocraft_engine::capishot_cmds`), invoked
//! from the menu without parameters:
//!
//! * File › Export › Compile PSD… asks where to save, then compiles the active document. In the
//!   browser the answer is the download's name and the image is handed to the download writer
//!   ([`crate::Services::write`]), as Save for Web does.
//! * File › Automate › Compile Folder… opens a form (input and output folders, format) on the
//!   desktop. A page can't pick a folder, so in the browser it asks for the PSD/PSB files instead
//!   and downloads one ZIP of the compiled pages.

use photocraft_engine::capishot_cmds;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;

pub(crate) const COMPILE: &str = "file.export.compilePsd";
pub(crate) const COMPILE_FOLDER: &str = "file.automate.compileFolder";
/// The download's name for the pages compiled in the browser.
const ZIP_NAME: &str = "compiled-pages.zip";

fn no_params(p: &Value) -> bool {
    p.as_object().is_none_or(|o| o.is_empty())
}

/// Menu items fronted here; `None` when not ours (or the engine command runs as is).
pub(crate) fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let web = cfg!(target_arch = "wasm32");
    match id {
        COMPILE => compile_psd(app, params, web),
        COMPILE_FOLDER => compile_folder(app, params, web),
        _ => None,
    }
}

/// Compile PSD: without a `path`, ask for one. On the web a `path` is the download's name.
fn compile_psd(app: &mut PhotocraftApp, params: &Value, web: bool) -> Option<Result<Value, String>> {
    if params.get("path").is_some() {
        if !web {
            return None;
        }
        let r = compile_to_writer(app, params);
        return Some(finish(app, r));
    }
    let Some(st) = app.session.active() else { return Some(Err("no document".into())) };
    let doc = st.doc.id;
    let stem = st.doc.name.rsplit_once('.').map_or(st.doc.name.as_str(), |(a, _)| a);
    let suggested = format!("{stem}.png");
    let base = if params.is_object() { params.clone() } else { json!({}) };
    Some(app.pick_save(&suggested, move |app, path| {
        app.with_document(doc, |app| {
            let mut p = base;
            p["path"] = json!(path);
            let r = if web { compile_to_writer(app, &p) } else { app.run(COMPILE, p) };
            finish(app, r)
        })
    }))
}

/// The active document compiled and handed to the platform writer (the browser's download).
fn compile_to_writer(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let write = app.services.write.as_mut().ok_or("no download service")?;
    capishot_cmds::compile_active_with_writer(&mut app.session, p, &mut |path, bytes| write(path, bytes).map_err(photocraft_engine::EngineError::Other))
        .map_err(|e| e.to_string())
}

/// Compile Folder: the desktop form, or (web) a pick of PSD files compiled into a ZIP download.
fn compile_folder(app: &mut PhotocraftApp, params: &Value, web: bool) -> Option<Result<Value, String>> {
    if !web {
        return no_params(params).then(|| Ok(json!({"dialog": folder_form(app)})));
    }
    if params.get("input").is_some() {
        return Some(Err("a web page can't read a folder: run Compile Folder… and pick the PSD files".into()));
    }
    let p = if params.is_object() { params.clone() } else { json!({}) };
    Some(app.pick_files_bytes(&["psd", "psb"], move |app, files| compile_picked(app, &files, &p)))
}

/// Compiles the picked files into one ZIP and hands it to the platform writer.
fn compile_picked(app: &mut PhotocraftApp, files: &[(String, Vec<u8>)], p: &Value) -> Result<Value, String> {
    let r = capishot_cmds::compile_files_to_zip(&app.session, files, p).map_err(|e| e.to_string()).and_then(|(zip, mut report)| {
        let write = app.services.write.as_mut().ok_or("no download service")?;
        write(ZIP_NAME, &zip)?;
        report["path"] = json!(ZIP_NAME);
        Ok(report)
    });
    finish(app, r)
}

fn folder_form(app: &mut PhotocraftApp) -> u64 {
    let dir = app.session.active().and_then(|d| d.path.as_deref()).and_then(|p| p.rfind(['/', '\\']).map(|i| p[..i].to_string())).unwrap_or_else(|| ".".into());
    let label = photocraft_engine::commands::find(COMPILE_FOLDER).map_or(COMPILE_FOLDER, |c| c.label);
    let mut f = Map::new();
    f.insert("__command".into(), json!(COMPILE_FOLDER));
    f.insert("__label".into(), json!(label));
    f.insert("__form".into(), json!(true));
    f.insert("__choices".into(), json!({"format": ["png", "jpg", "webp", "tif"]}));
    for (k, v) in
        [("input", json!(dir)), ("output", json!(format!("{dir}/compiled"))), ("format", json!("png")), ("rerenderText", json!(true)), ("quality", json!(90))]
    {
        f.insert(k.into(), v);
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

/// Reports the outcome in the status bar.
fn finish(app: &mut PhotocraftApp, r: Result<Value, String>) -> Result<Value, String> {
    match &r {
        Ok(v) => {
            let warnings = v["warnings"].as_array().map_or(0, Vec::len);
            let failed = v["failed"].as_array().map_or(0, Vec::len);
            let what = match v["files"].as_array() {
                Some(files) => format!("Compiled {} page(s) into {}", files.len(), v["path"].as_str().unwrap_or("")),
                None => format!("Compiled {}", v["path"].as_str().unwrap_or("")),
            };
            app.ui.status = match (failed, warnings) {
                (0, 0) => what,
                (0, w) => format!("{what} ({w} warning(s))"),
                (f, _) => format!("{what}; {f} file(s) failed"),
            };
            app.ui.status_error = failed > 0;
        }
        Err(e) if e != crate::file_dialog::CANCELLED => {
            app.ui.status = e.clone();
            app.ui.status_error = true;
        }
        Err(_) => {}
    }
    r
}

#[cfg(test)]
mod tests;
