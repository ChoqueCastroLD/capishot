//! TypeR imports that read other files (see `docs/typer.md`): FontScanR (`typer.fontScan`,
//! F3), which turns the text layers of a chapter's PSDs into proposed styles, and the import of
//! the chosen proposals into a folder; and Photoshop tool presets (`.tpl`, K6) as a folder of
//! styles (`typer.library.importTpl`). Session state only (`journal: false`).
//!
//! The scan reads open documents' text layers, or [`ScanFile`]s the shell made from picked PSD
//! files with [`scan_psd`] (the layer records and their type data only; no pixels are decoded).

use photocraft_doc::{Document, Effect, FxPaint, Layer, LayerContent};
use photocraft_typer::exchange::{ImportKind, ImportOptions};
use photocraft_typer::fontscan::{self, ScanFile, ScanLayer};
use photocraft_typer::{Folder, Id, Style, exchange, psjson};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

type EnabledFn = fn(&Session) -> std::result::Result<(), String>;
type RunFn = fn(&mut Session, &Value) -> Result<Value>;

/// Most files and text layers one scan takes.
pub const MAX_SCAN_FILES: usize = 2_000;
pub const MAX_SCAN_LAYERS: usize = 200_000;

pub fn specs() -> Vec<CommandSpec> {
    let q = |id: &'static str, label: &'static str, params: &'static str, enabled: EnabledFn, run: RunFn| CommandSpec {
        id,
        label,
        menu: &[],
        shortcut: None,
        params,
        enabled,
        run,
        journal: false,
    };
    let always = crate::commands::always;
    vec![
        q(
            "typer.fontScan",
            "TypeR FontScanR",
            r#"{"documents":[index]|"all"? (default: every open document unless "scans" is given),"scans":[{file,layers:[{layerName,antiAlias,typeUnit,paragraphStyle,stroke,runs:[textStyle]}]}]? (from picked PSD files)} → {groups:[{key,defaultName,fontPostScriptName,fontName,fontStyleName,syntheticBold,syntheticItalic,usageCount,fileCount,sizes,topSize,installed,textProps,stroke}],files,layers}"#,
            always,
            font_scan,
        ),
        q(
            "typer.fontScan.import",
            "Import TypeR FontScanR Styles",
            r#"{"styles":[{name,textProps,stroke}] (groups from typer.fontScan, renamed as wanted),"folder":id? (an existing folder) | "newFolder":string? (a new folder; default: Unsorted),"now":ms?} → {folder,styles:[id]}"#,
            always,
            font_scan_import,
        ),
        q(
            "typer.library.importTpl",
            "Import TypeR Styles from Tool Presets",
            r#"{"hex":string (a Photoshop tool preset file, .tpl, as hex),"name":string? (its file name; the new folder's name),"now":ms?} → {kind,folders,styles,folderId,…} (one style per Type tool preset; errors carry TPL_INVALID_FILE, TPL_TRUNCATED_DATA, TPL_UNSUPPORTED_DATA, TPL_MISSING_TOOL_DATA or TPL_NO_TEXT_PRESETS)"#,
            always,
            import_tpl,
        ),
    ]
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// A layer's outer stroke effect as a TypeR stroke, if it has an enabled one.
pub fn stroke_json(effects: &[Effect]) -> Option<Value> {
    effects.iter().find_map(|e| match e {
        Effect::Stroke(sf) if sf.common.enabled => {
            let rgb = match &sf.paint {
                FxPaint::Color(col) => col.to_rgb(),
                _ => [0.0, 0.0, 0.0],
            };
            let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round();
            Some(json!({"enabled": true, "size": sf.size, "opacity": (sf.common.opacity * 100.0).round(), "position": "outer",
                "color": {"r": ch(rgb[0]), "g": ch(rgb[1]), "b": ch(rgb[2])}}))
        }
        _ => None,
    })
}

/// A text layer as FontScanR sees it: every run's TypeR `textStyle`, the first paragraph's
/// style, the anti-aliasing and the stroke (sizes in pixels at `dpi`).
pub fn scan_text_layer(name: &str, t: &photocraft_doc::TextLayer, effects: &[Effect], dpi: f64) -> ScanLayer {
    let para = t.paragraph_runs().first().map(|r| r.style.clone()).unwrap_or_default();
    let mut paragraph_style = None;
    let mut anti_alias = String::new();
    let mut runs = Vec::new();
    for r in t.char_runs() {
        let props = psjson::text_props_from(&r.style, &para, t.antialias, dpi);
        let lt = &props["layerText"];
        if paragraph_style.is_none() {
            paragraph_style = lt["paragraphStyleRange"][0].get("paragraphStyle").cloned();
            anti_alias = lt["antiAlias"].as_str().unwrap_or_default().to_string();
        }
        if let Some(ts) = lt["textStyleRange"][0].get("textStyle") {
            runs.push(ts.clone());
        }
    }
    ScanLayer { layer_name: name.to_string(), anti_alias, type_unit: "pixelsUnit".into(), paragraph_style, stroke: stroke_json(effects), runs }
}

/// The text layers of an open document.
pub fn scan_document(doc: &Document, file: &str) -> ScanFile {
    let dpi = f64::from(doc.resolution_dpi);
    let layers = doc
        .walk()
        .into_iter()
        .filter_map(|(_, _, l): (_, _, &Layer)| match &l.content {
            LayerContent::Text(t) => Some(scan_text_layer(&l.name, t, &l.effects.items, dpi)),
            _ => None,
        })
        .collect();
    ScanFile { file: file.to_string(), layers }
}

/// The text layers of a PSD file's bytes, without decoding any pixels: each layer's type data
/// (`TySh`) and layer effects (`lfx2`, for the stroke).
pub fn scan_psd(name: &str, bytes: &[u8]) -> std::result::Result<ScanFile, String> {
    let psd = photocraft_psd::PsdFile::from_bytes(bytes).map_err(|e| format!("{name}: {e}"))?;
    let dpi = psd.resolution().map_or(72.0, |r| if r.h_res_unit == 2 { r.h_res() * 2.54 } else { r.h_res() });
    let dpi = if dpi.is_finite() && dpi > 0.0 { dpi } else { 72.0 };
    let mut layers = Vec::new();
    for rec in psd.layers() {
        let Some(tysh) = rec.blocks.iter().find(|b| &b.key == b"TySh") else { continue };
        let Some(t) = photocraft_text::psd::text_layer_from_tysh(&tysh.data, dpi as f32) else { continue };
        let effects =
            rec.blocks.iter().find(|b| &b.key == b"lfx2").and_then(|b| photocraft_io::effects_map::parse_lfx2(&b.data)).map(|(_, fx)| fx).unwrap_or_default();
        layers.push(scan_text_layer(&rec.name(), &t, &effects, dpi));
        if layers.len() >= MAX_SCAN_LAYERS {
            break;
        }
    }
    Ok(ScanFile { file: name.to_string(), layers })
}

/// Is the group's font available (exact PostScript face, else its family)?
fn installed(ps: &str, family: &str) -> bool {
    let Some(engine) = photocraft_text::try_shared() else { return true };
    let mut eng = engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (!ps.is_empty() && eng.fonts.resolve_postscript(ps).exact) || (!family.is_empty() && eng.fonts.has_family(family))
}

fn font_scan(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.fontScan";
    let mut files: Vec<ScanFile> = Vec::new();
    if let Some(scans) = p.get("scans").filter(|v| !v.is_null()) {
        let list = scans.as_array().ok_or_else(|| bad(CMD, "\"scans\" must be a list of scanned files"))?;
        if list.len() > MAX_SCAN_FILES {
            return Err(bad(CMD, format!("{} files; the limit is {MAX_SCAN_FILES}", list.len())));
        }
        for v in list {
            files.push(serde_json::from_value(v.clone()).map_err(|e| bad(CMD, format!("a scanned file: {e}")))?);
        }
    }
    let docs = s.documents();
    let picked: Vec<usize> = match p.get("documents") {
        None | Some(Value::Null) if files.is_empty() => (0..docs.len()).collect(),
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(a)) if a == "all" => (0..docs.len()).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .filter(|i| *i < docs.len())
                    .ok_or_else(|| bad(CMD, "\"documents\" must list open document indexes"))
            })
            .collect::<Result<_>>()?,
        Some(_) => return Err(bad(CMD, "\"documents\" must be \"all\" or a list of document indexes")),
    };
    for i in picked {
        if let Some(d) = docs.get(i) {
            files.push(scan_document(&d.doc, &d.doc.name));
        }
    }
    let layers: usize = files.iter().map(|f| f.layers.len()).sum();
    if layers > MAX_SCAN_LAYERS {
        return Err(bad(CMD, format!("{layers} text layers; the limit is {MAX_SCAN_LAYERS}")));
    }
    let groups: Vec<Value> = fontscan::build_groups(&files)
        .into_iter()
        .map(|g| {
            let ok = installed(&g.font_post_script_name, &g.font_name);
            let mut v = serde_json::to_value(&g).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.insert("installed".into(), Value::Bool(ok));
            }
            v
        })
        .collect();
    Ok(json!({"groups": groups, "files": files.len(), "layers": layers}))
}

fn font_scan_import(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.fontScan.import";
    let list = p.get("styles").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or_else(|| bad(CMD, "pass \"styles\": the groups to import"))?;
    if list.len() > 10_000 {
        return Err(bad(CMD, "too many styles"));
    }
    let now = p.get("now").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0);
    let lib = &s.typer.state.library;
    // Where they go: an existing folder, a new one, or Unsorted.
    let (folder, new_folder) = match (p.get("folder").filter(|v| !v.is_null()), p.get("newFolder").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => return Err(bad(CMD, "pass \"folder\" or \"newFolder\", not both")),
        (Some(f), None) => {
            let id = Id::from_value(f).ok_or_else(|| bad(CMD, "\"folder\" must be a folder id"))?;
            if lib.folder(&id).is_none() {
                return Err(bad(CMD, format!("no folder {id}")));
            }
            (Some(id), None)
        }
        (None, Some(n)) => {
            let name = n
                .as_str()
                .map(str::trim)
                .filter(|n| !n.is_empty() && n.chars().count() <= 200)
                .ok_or_else(|| bad(CMD, "\"newFolder\" must be a folder name"))?;
            let id = lib.fresh_id("f");
            (Some(id.clone()), Some(Folder { id, name: name.to_string(), parent_id: None, order: 0.0 }))
        }
        (None, None) => (None, None),
    };
    // Validate everything first: nothing is added on error.
    let mut styles: Vec<Style> = Vec::new();
    let mut ids: Vec<Id> = Vec::new();
    for g in list {
        let name = g.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "every style needs a name"))?;
        let props = g.get("textProps").filter(|v| v.is_object()).ok_or_else(|| bad(CMD, format!("\"{name}\" has no textProps")))?;
        let mut v = json!({"name": name, "textProps": props, "prefixes": []});
        if let Some(st) = g.get("stroke").filter(|v| v.is_object()) {
            v["stroke"] = st.clone();
        }
        if let Some(now) = now {
            v["edited"] = json!(now.floor() as u64);
        }
        if let Some(f) = &folder {
            v["folder"] = f.to_value();
        }
        let id = fresh_style_id(lib, &ids);
        v["id"] = id.to_value();
        exchange::validate(&json!({"styles": [v.clone()]})).map_err(|e| bad(CMD, format!("\"{name}\": {e}")))?;
        styles.push(Style::from_json(&v).ok_or_else(|| bad(CMD, format!("\"{name}\" is not a valid style")))?);
        ids.push(id);
    }
    s.typer.state.edit_library(|lib| {
        if let Some(f) = new_folder {
            lib.save_folder(f, None, false);
        }
        for st in styles {
            lib.save_style(st);
        }
    });
    Ok(json!({"folder": folder, "styles": ids}))
}

/// Hex text → bytes (whitespace ignored, as TypeR's converter does).
fn from_hex(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    let val = |c: u8| char::from(c).to_digit(16).and_then(|d| u8::try_from(d).ok());
    digits.as_chunks::<2>().0.iter().map(|[a, b]| Some(val(*a)? << 4 | val(*b)?)).collect()
}

/// Import a `.tpl` file's Type tool presets as a library (a new folder named after the file).
pub fn import_tpl_bytes(s: &mut Session, bytes: &[u8], name: &str, now: Option<f64>) -> std::result::Result<Value, String> {
    let lib = photocraft_typer::tpl::convert(bytes, name, now).map_err(|e| e.to_string())?;
    let opts = ImportOptions { kind: Some(ImportKind::Library), keep_local: true, now: now.map(|n| n as u64) };
    let report = exchange::import(&mut s.typer.state, &lib, opts)?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn import_tpl(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.library.importTpl";
    let hex = p.get("hex").and_then(Value::as_str).ok_or_else(|| bad(CMD, "pass \"hex\": the .tpl file's bytes as hex"))?;
    if hex.len() > photocraft_typer::tpl::MAX_TPL_BYTES * 2 + 1024 {
        return Err(bad(CMD, "TPL_INVALID_FILE: the file is too large"));
    }
    let bytes = from_hex(hex).ok_or_else(|| bad(CMD, "TPL_INVALID_FILE: the data is not hex"))?;
    let name = match p.get("name") {
        None | Some(Value::Null) => "TPL",
        Some(Value::String(n)) => n.as_str(),
        Some(_) => return Err(bad(CMD, "\"name\" must be the file name")),
    };
    let now = p.get("now").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0);
    import_tpl_bytes(s, &bytes, name, now).map_err(|e| bad(CMD, e))
}

/// A style id that is neither in the library nor among `taken`.
fn fresh_style_id(lib: &photocraft_typer::Library, taken: &[Id]) -> Id {
    (0u64..)
        .map(|n| Id::Str(format!("fs{n}")))
        .find(|id| lib.style(id).is_none() && lib.folder(id).is_none() && !taken.contains(id))
        .unwrap_or_else(|| lib.fresh_id("s"))
}

#[cfg(test)]
#[path = "typer_import_cmds/tests.rs"]
mod tests;
