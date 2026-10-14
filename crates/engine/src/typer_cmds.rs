//! TypeR session and library commands (`typer.*`, see `docs/typer.md`): the script and its
//! line/page cursor, page files, the style library (styles, folders, TypeR JSON import/export)
//! and the settings. Queries and session state: none of them touches a document or history
//! (`journal: false`).
//!
//! Placement (`typer.paste`, `typer.apply`, …) is in `typer_place_cmds`, bubble and shaping
//! queries in `typer_query_cmds`.

use photocraft_typer::exchange::{self, ExportOptions, ImportKind, ImportOptions};
use photocraft_typer::psjson::{self, MapOptions};
use photocraft_typer::{Folder, Id, Moved, State, Style, presets};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const MENU: &[&str] = &["Type", "TypeR"];

type EnabledFn = fn(&Session) -> std::result::Result<(), String>;
type RunFn = fn(&mut Session, &Value) -> Result<Value>;

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
    let m = |id: &'static str, label: &'static str, params: &'static str, enabled: EnabledFn, run: RunFn| CommandSpec {
        id,
        label,
        menu: MENU,
        shortcut: None,
        params,
        enabled,
        run,
        journal: false,
    };
    let always = crate::commands::always;
    vec![
        // Script and cursor
        q(
            "typer.state",
            "TypeR State",
            r#"{"lines":bool=false} → {currentLine,line,number,text,style,styleName,page,image,usableLines,lineCount,textScale,lines?}"#,
            always,
            state,
        ),
        q(
            "typer.script.set",
            "Set TypeR Script",
            r###"{"text":string (the translated chapter; "Page N" lines mark pages, "##" lines are notes, "TAG:" picks a style, "//" continues the previous style)} → state"###,
            always,
            script_set,
        ),
        q(
            "typer.parseScript",
            "Parse TypeR Script",
            r#"{"text":string? (default: the current script)} → {lines:[{rawText,rawIndex,text,style,usedStyle,ignore,index,page,last,…}]} (doesn't change the script)"#,
            always,
            parse_script,
        ),
        q(
            "typer.line.set",
            "Go to TypeR Line",
            r#"{"index":int (raw line, 0-based; an ignored line moves to the first usable one)} → state"#,
            has_script,
            line_set,
        ),
        m(
            "typer.line.next",
            "Next Line",
            r#"{"add":bool=false (after a paste: records the line's style and stays on the last line of a synced page)} → {moved,…state}"#,
            has_script,
            line_next,
        ),
        m("typer.line.previous", "Previous Line", "{} → {moved,…state}", has_script, line_previous),
        m("typer.page.next", "Next Page", "{} → {moved,…state} (first line after the next \"Page N\" marker)", has_script, page_next),
        m("typer.page.previous", "Previous Page", "{} → {moved,…state}", has_script, page_previous),
        q(
            "typer.images.set",
            "Set TypeR Page Files",
            r#"{"images":[string] (page file names, sorted naturally; page N maps to the file whose name ends in N, else the N-th)} → state"#,
            always,
            images_set,
        ),
        q("typer.images.clear", "Clear TypeR Page Files", "{} → state", always, images_clear),
        // Styles
        q(
            "typer.style.list",
            "List TypeR Styles",
            "{} → {folders:[{id,name,parentId,depth,label,count}],styles:[{id,name,folder,prefixes,prefixColor,prefixesDisabled,size,unit,textType}],current,default}",
            always,
            style_list,
        ),
        q(
            "typer.style.select",
            "Select TypeR Style",
            r#"{"id":id} | {"index":int (0-based, within the current style's folder, like numpad 1-9)} → state"#,
            has_styles,
            style_select,
        ),
        m("typer.style.next", "Next Style", "{} → state (wraps)", has_styles, style_next),
        m("typer.style.previous", "Previous Style", "{} → state (wraps)", has_styles, style_previous),
        m("typer.style.nextSizePreset", "Next Size Preset", "{} → {changed,size} (cycles the current style's size presets)", has_styles, style_next_preset),
        q(
            "typer.style.setSizePreset",
            "Set TypeR Size Preset",
            r#"{"id":id?,"size":number (one of the style's presets),"now":ms?} → {changed,size}"#,
            has_styles,
            style_set_preset,
        ),
        q(
            "typer.style.setSize",
            "Set TypeR Style Size",
            r#"{"id":id?,"size":number>0 (replaces the active preset; quick size edit),"now":ms?} → {changed,size}"#,
            has_styles,
            style_set_size,
        ),
        q(
            "typer.style.resolve",
            "Resolve TypeR Style",
            r#"{"id":id?,"dpi":number? (default: the active document's),"pageWidth":px? (default: the active document's),"textScale":%?} → {charStyle,paragraph,antialias,sizePt,stroke,pointText,warnings}"#,
            has_styles,
            style_resolve,
        ),
        q(
            "typer.style.save",
            "Save TypeR Style",
            r#"{"style":{id?,name,folder?,prefixes?:[string]|"a;b",prefixColor?,prefixesDisabled?,textType?,textProps?,stroke?,sizePresets?,…} (TypeR style JSON; merged into the style with that id),"now":ms?} → {id}"#,
            always,
            style_save,
        ),
        q("typer.style.delete", "Delete TypeR Style", r#"{"id":id} → {deleted}"#, has_styles, style_delete),
        q(
            "typer.style.duplicate",
            "Duplicate TypeR Style",
            r#"{"id":id? (default: current),"folder":id|null? (default: same folder)} → {id} (the copy becomes current)"#,
            has_styles,
            style_duplicate,
        ),
        q("typer.style.togglePrefixes", "Toggle TypeR Style Tags", r#"{"id":id? (default: current)} → {prefixesDisabled}"#, has_styles, style_toggle_prefixes),
        q(
            "typer.style.reorder",
            "Move TypeR Style",
            r#"{"id":id,"folder":id|null (null = Unsorted),"order":[id]? (the folder's styles in this order)} → state"#,
            has_styles,
            style_reorder,
        ),
        // Folders
        q(
            "typer.folder.save",
            "Save TypeR Folder",
            r#"{"folder":{id?,name,parentId?,order?},"styleIds":[id]? (exactly these styles in the folder)} → {id}"#,
            always,
            folder_save,
        ),
        q(
            "typer.folder.delete",
            "Delete TypeR Folder",
            r#"{"id":id,"permanent":bool=false (also delete its styles; else they move to Unsorted)} → {deleted}"#,
            has_folders,
            folder_delete,
        ),
        q("typer.folder.duplicate", "Duplicate TypeR Folder", r#"{"id":id} → {id} (deep copy with subfolders and styles)"#, has_folders, folder_duplicate),
        q("typer.folder.reorder", "Reorder TypeR Folders", r#"{"parentId":id|null,"order":[id]} → state"#, has_folders, folder_reorder),
        // Exchange
        q(
            "typer.library.import",
            "Import TypeR Styles",
            r#"{"json":string|object (TypeR full, library or folder export),"kind":"folder"|"library"|"full"? (default: TypeR's routing),"keepLocal":bool=true (full: keep local styles missing from the import or edited later),"now":ms?} → {kind,folders,styles,keptLocal,settings,folderId,script}"#,
            always,
            library_import,
        ),
        q(
            "typer.library.export",
            "Export TypeR Styles",
            r#"{"folders":[id|null]? (null = Unsorted; default all),"withSettings":bool=false} → TypeR export JSON"#,
            always,
            library_export,
        ),
        q("typer.library.exportFolder", "Export TypeR Folder", r#"{"id":id} → {name,exportedStyles,schema}"#, has_folders, library_export_folder),
        // Settings
        q("typer.settings.get", "TypeR Settings", "{} → settings (TypeR keys)", always, settings_get),
        q(
            "typer.settings.set",
            "Set TypeR Settings",
            r#"{"key":string,"value":any} | {"values":{key:value}} (keys: pastePointText, ignoreLinePrefixes, ignoreTags, defaultStyleId, direction, middleEast, autoClosePSD, autoScrollStyle, currentFolderTagPriority, resizeTextBoxOnCenter, multiBubbleMode, internalPadding, interpretMarkdown, showTips, showQuickStyleSize, styleSizeStep, textSizeIncrement, textScale, resetLineCounterOnPage, multiTabEnabled, inlineTextShapeR, dehyphenateTextShapeR, longestPrefixMatch, openStoredSelections) → settings"#,
            always,
            settings_set,
        ),
    ]
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_script(s: &Session) -> std::result::Result<(), String> {
    if s.typer.state.lines().iter().any(|l| !l.ignore) { Ok(()) } else { Err("no TypeR script with text lines".into()) }
}

fn has_styles(s: &Session) -> std::result::Result<(), String> {
    if s.typer.state.library.styles.is_empty() { Err("the TypeR library has no styles".into()) } else { Ok(()) }
}

fn has_folders(s: &Session) -> std::result::Result<(), String> {
    if s.typer.state.library.folders.is_empty() { Err("the TypeR library has no folders".into()) } else { Ok(()) }
}

/// Milliseconds since the Unix epoch: `"now"` from the params (the web shell passes it; wasm has
/// no clock here), else the system clock on native.
fn now_ms(p: &Value) -> Option<u64> {
    if let Some(n) = p.get("now").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0) {
        return Some(n as u64);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| u64::try_from(d.as_millis()).ok())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// An id param (string or number). `Ok(None)` when absent.
fn id_param(p: &Value, key: &str, cmd: &str) -> Result<Option<Id>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Id::from_value(v).map(Some).ok_or_else(|| bad(cmd, format!("\"{key}\" must be a style or folder id"))),
    }
}

/// A list of ids.
fn ids_param(p: &Value, key: &str, cmd: &str) -> Result<Option<Vec<Id>>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => {
            a.iter().map(|v| Id::from_value(v).ok_or_else(|| bad(cmd, format!("\"{key}\" must list ids")))).collect::<Result<Vec<_>>>().map(Some)
        }
        Some(_) => Err(bad(cmd, format!("\"{key}\" must be a list of ids"))),
    }
}

/// `id` param, else the current style.
fn style_id(s: &Session, p: &Value, cmd: &str) -> Result<Id> {
    let id = id_param(p, "id", cmd)?.or_else(|| s.typer.state.current_style_id.clone()).ok_or_else(|| bad(cmd, "no style selected; pass \"id\""))?;
    if s.typer.state.library.style(&id).is_none() {
        return Err(bad(cmd, format!("no style {id}")));
    }
    Ok(id)
}

/// The cursor, line, style and page, for every navigation result.
pub fn summary(st: &State) -> Value {
    let line = st.current_line();
    let style = st.current_style();
    let usable = st.lines().iter().filter(|l| !l.ignore).count();
    json!({
        "currentLine": st.current_line_index,
        "line": line,
        "number": line.map(|l| l.index),
        "text": line.map(|l| l.text.as_str()),
        "style": st.current_style_id,
        "styleName": style.map(|s| s.name.as_str()),
        "page": st.current_page(),
        "image": st.current_image().and_then(|i| st.images.get(i)),
        "usableLines": usable,
        "lineCount": st.lines().len(),
        "textScale": st.settings.text_scale,
    })
}

fn with_moved(st: &State, moved: Moved) -> Value {
    let mut v = summary(st);
    if let Some(o) = v.as_object_mut() {
        o.insert("moved".into(), Value::Bool(moved == Moved::Yes));
    }
    v
}

fn state(s: &mut Session, p: &Value) -> Result<Value> {
    let st = &s.typer.state;
    let mut v = summary(st);
    if p.get("lines").and_then(Value::as_bool) == Some(true)
        && let Some(o) = v.as_object_mut()
    {
        o.insert("lines".into(), serde_json::to_value(st.lines()).unwrap_or(Value::Null));
    }
    Ok(v)
}

fn script_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.script.set";
    let text = p.get("text").and_then(Value::as_str).ok_or_else(|| bad(CMD, "pass \"text\" (the script)"))?;
    s.typer.state.set_text(text).map_err(|e| bad(CMD, e))?;
    Ok(summary(&s.typer.state))
}

fn parse_script(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.parseScript";
    let lines = match p.get("text") {
        None | Some(Value::Null) => s.typer.state.lines().to_vec(),
        Some(Value::String(text)) => {
            let mut probe = s.typer.state.clone();
            probe.set_text(text).map_err(|e| bad(CMD, e))?;
            probe.lines().to_vec()
        }
        Some(_) => return Err(bad(CMD, "\"text\" must be a string")),
    };
    Ok(json!({"lines": lines}))
}

fn line_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.line.set";
    let i = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "pass \"index\" (a line number from 0)"))?;
    let n = s.typer.state.lines().len();
    let i = usize::try_from(i).ok().filter(|i| *i < n).ok_or_else(|| bad(CMD, format!("no line {i} (the script has {n})")))?;
    s.typer.state.set_line(i);
    Ok(summary(&s.typer.state))
}

fn line_next(s: &mut Session, p: &Value) -> Result<Value> {
    let add = p.get("add").and_then(Value::as_bool).unwrap_or(false);
    let moved = s.typer.state.next_line(add);
    Ok(with_moved(&s.typer.state, moved))
}

fn line_previous(s: &mut Session, _: &Value) -> Result<Value> {
    let moved = s.typer.state.previous_line();
    Ok(with_moved(&s.typer.state, moved))
}

fn page_next(s: &mut Session, _: &Value) -> Result<Value> {
    let moved = s.typer.state.next_page();
    Ok(with_moved(&s.typer.state, moved))
}

fn page_previous(s: &mut Session, _: &Value) -> Result<Value> {
    let moved = s.typer.state.previous_page();
    Ok(with_moved(&s.typer.state, moved))
}

/// Most page files a chapter can list.
const MAX_IMAGES: usize = 10_000;

fn images_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.images.set";
    let list = p.get("images").and_then(Value::as_array).ok_or_else(|| bad(CMD, "pass \"images\": [file names]"))?;
    if list.len() > MAX_IMAGES {
        return Err(bad(CMD, format!("{} files; the limit is {MAX_IMAGES}", list.len())));
    }
    let names = list
        .iter()
        .map(|v| v.as_str().filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(|| bad(CMD, "\"images\" must be file names")))
        .collect::<Result<Vec<_>>>()?;
    s.typer.state.set_images(names);
    Ok(summary(&s.typer.state))
}

fn images_clear(s: &mut Session, _: &Value) -> Result<Value> {
    s.typer.state.set_images(Vec::new());
    Ok(summary(&s.typer.state))
}

fn style_json(st: &Style) -> Value {
    json!({
        "id": st.id,
        "name": st.name,
        "folder": st.folder,
        "prefixes": st.prefixes,
        "prefixColor": st.prefix_color(),
        "prefixesDisabled": st.prefixes_disabled(),
        "size": st.size(),
        "unit": st.type_unit().unwrap_or("pixelsUnit"),
        "textType": match st.text_type() { photocraft_typer::TextType::Inherit => "inherit", photocraft_typer::TextType::Point => "point", photocraft_typer::TextType::Paragraph => "paragraph" },
        "sizePresets": presets::normalize_presets(st),
        "fontName": st.text_style().and_then(|t| t.get("fontName")).and_then(Value::as_str),
    })
}

fn style_list(s: &mut Session, _: &Value) -> Result<Value> {
    let st = &s.typer.state;
    let lib = &st.library;
    let folders: Vec<Value> = lib
        .flatten_tree()
        .into_iter()
        .map(|e| {
            let count = lib.styles.iter().filter(|s| s.folder.as_ref() == Some(&e.id)).count();
            let mut v = serde_json::to_value(&e).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.insert("count".into(), json!(count));
            }
            v
        })
        .collect();
    Ok(json!({
        "folders": folders,
        "styles": lib.styles.iter().map(style_json).collect::<Vec<_>>(),
        "unsorted": lib.styles.iter().filter(|s| s.folder.is_none()).count(),
        "current": st.current_style_id,
        "default": st.settings.default_style_id,
    }))
}

fn style_select(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.select";
    if let Some(id) = id_param(p, "id", CMD)? {
        s.typer.state.select_style(&id).map_err(|e| bad(CMD, e))?;
    } else if let Some(i) = p.get("index").and_then(Value::as_u64) {
        let i = usize::try_from(i).map_err(|_| bad(CMD, "index out of range"))?;
        s.typer.state.select_style_in_folder(i).map_err(|e| bad(CMD, e))?;
    } else {
        return Err(bad(CMD, "pass \"id\" or \"index\""));
    }
    Ok(summary(&s.typer.state))
}

fn style_next(s: &mut Session, _: &Value) -> Result<Value> {
    s.typer.state.step_style(true);
    Ok(summary(&s.typer.state))
}

fn style_previous(s: &mut Session, _: &Value) -> Result<Value> {
    s.typer.state.step_style(false);
    Ok(summary(&s.typer.state))
}

/// Run `f` on a style, refresh, and report `{changed, size}`.
fn edit_size(s: &mut Session, id: &Id, f: impl FnOnce(&mut Style) -> bool) -> Result<Value> {
    let (changed, size) = s.typer.state.edit_library(|lib| match lib.style_mut(id) {
        Some(st) => (f(st), st.size()),
        None => (false, None),
    });
    Ok(json!({"changed": changed, "size": size}))
}

fn style_next_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let id = style_id(s, p, "typer.style.nextSizePreset")?;
    let now = now_ms(p);
    edit_size(s, &id, |st| presets::cycle_preset(st, now))
}

fn size_param(p: &Value, cmd: &str) -> Result<f64> {
    p.get("size").and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0 && *v <= 10_000.0).ok_or_else(|| bad(cmd, "pass \"size\" (a number above 0)"))
}

fn style_set_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.setSizePreset";
    let id = style_id(s, p, CMD)?;
    let size = size_param(p, CMD)?;
    let now = now_ms(p);
    edit_size(s, &id, |st| presets::set_preset(st, size, now))
}

fn style_set_size(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.setSize";
    let id = style_id(s, p, CMD)?;
    let size = size_param(p, CMD)?;
    let now = now_ms(p);
    edit_size(s, &id, |st| presets::update_active_preset(st, size, now))
}

fn style_resolve(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.resolve";
    let id = style_id(s, p, CMD)?;
    let doc = s.active().map(|d| (f64::from(d.doc.resolution_dpi), f64::from(d.doc.size.width)));
    let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0);
    let st = &s.typer.state;
    let style = st.library.style(&id).ok_or_else(|| bad(CMD, format!("no style {id}")))?;
    let opts = MapOptions {
        dpi: num("dpi").or(doc.map(|d| d.0)).unwrap_or(72.0),
        text_scale: num("textScale").or(st.settings.text_scale),
        page_width: num("pageWidth").or(doc.map(|d| d.1)),
        direction: st.settings.direction,
    };
    let m = psjson::map_style(style, &opts);
    Ok(json!({
        "charStyle": m.char_style,
        "paragraph": m.paragraph,
        "antialias": m.antialias,
        "sizePt": m.size_pt,
        "stroke": m.stroke,
        "pointText": photocraft_typer::payload::resolve_point_text(Some(style), st.settings.paste_point_text),
        "warnings": m.warnings,
        "dpi": opts.dpi,
    }))
}

fn style_save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.save";
    let given = p.get("style").and_then(Value::as_object).ok_or_else(|| bad(CMD, "pass \"style\": {name, …}"))?;
    let lib = &s.typer.state.library;
    let id = id_param(&Value::Object(given.clone()), "id", CMD)?;
    // Merge into the existing style (TypeR's `{...style, ...payload}`).
    let mut merged = match id.as_ref().and_then(|id| lib.style(id)) {
        Some(existing) => existing.to_json(),
        None => json!({}),
    };
    if let Some(o) = merged.as_object_mut() {
        for (k, v) in given {
            o.insert(k.clone(), v.clone());
        }
        let id = id.clone().unwrap_or_else(|| lib.fresh_id("s"));
        o.insert("id".into(), id.to_value());
        if let Some(Value::String(list)) = o.get("prefixes").cloned() {
            o.insert("prefixes".into(), json!(photocraft_typer::library::split_list(&list)));
        }
    }
    if merged.get("name").and_then(Value::as_str).is_none_or(|n| n.trim().is_empty()) {
        return Err(bad(CMD, "a style needs a name"));
    }
    exchange::validate(&json!({ "styles": [merged.clone()] })).map_err(|e| bad(CMD, e))?;
    let mut style = Style::from_json(&merged).ok_or_else(|| bad(CMD, "invalid style"))?;
    if !style.props.contains_key("textProps") {
        style.props.insert("textProps".into(), photocraft_typer::library::default_text_props());
    }
    if let Some(f) = &style.folder
        && lib.folder(f).is_none()
    {
        return Err(bad(CMD, format!("no folder {f}")));
    }
    if let Some(now) = now_ms(p) {
        style.edited = Some(now.into());
    }
    let id = s.typer.state.edit_library(|lib| lib.save_style(style));
    Ok(json!({"id": id}))
}

fn style_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.delete";
    let id = id_param(p, "id", CMD)?.ok_or_else(|| bad(CMD, "pass \"id\""))?;
    let deleted = s.typer.state.edit_library(|lib| lib.delete_style(&id));
    if !deleted {
        return Err(bad(CMD, format!("no style {id}")));
    }
    Ok(json!({"deleted": true}))
}

fn style_duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.duplicate";
    let id = style_id(s, p, CMD)?;
    let folder = match p.get("folder") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => Some(Some(Id::from_value(v).ok_or_else(|| bad(CMD, "\"folder\" must be a folder id or null"))?)),
    };
    if let Some(Some(f)) = &folder
        && s.typer.state.library.folder(f).is_none()
    {
        return Err(bad(CMD, format!("no folder {f}")));
    }
    let new_id = s.typer.state.edit_library(|lib| lib.duplicate_style(&id, folder)).ok_or_else(|| bad(CMD, format!("no style {id}")))?;
    s.typer.state.select_style(&new_id).map_err(|e| bad(CMD, e))?;
    Ok(json!({"id": new_id}))
}

fn style_toggle_prefixes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.togglePrefixes";
    let id = style_id(s, p, CMD)?;
    let disabled = s.typer.state.edit_library(|lib| lib.toggle_prefixes(&id)).ok_or_else(|| bad(CMD, format!("no style {id}")))?;
    Ok(json!({"prefixesDisabled": disabled}))
}

fn style_reorder(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.style.reorder";
    let id = id_param(p, "id", CMD)?.ok_or_else(|| bad(CMD, "pass \"id\""))?;
    let folder = id_param(p, "folder", CMD)?;
    let order = ids_param(p, "order", CMD)?.unwrap_or_default();
    s.typer.state.edit_library(|lib| lib.move_style(&id, folder, &order)).map_err(|e| bad(CMD, e))?;
    Ok(summary(&s.typer.state))
}

fn folder_save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.folder.save";
    let f = p.get("folder").filter(|f| f.is_object()).ok_or_else(|| bad(CMD, "pass \"folder\": {name, parentId?}"))?;
    let name = f.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "a folder needs a name"))?;
    if name.chars().count() > 200 {
        return Err(bad(CMD, "the folder name is longer than 200 characters"));
    }
    let lib = &s.typer.state.library;
    let id = id_param(f, "id", CMD)?.unwrap_or_else(|| lib.fresh_id("f"));
    let parent_id = id_param(f, "parentId", CMD)?;
    if let Some(pid) = &parent_id
        && (lib.folder(pid).is_none() || pid == &id || lib.descendants(&id).contains(pid))
    {
        return Err(bad(CMD, format!("{pid} can't be this folder's parent")));
    }
    let order = f.get("order").and_then(Value::as_f64).filter(|o| o.is_finite());
    let style_ids = ids_param(p, "styleIds", CMD)?;
    let folder = Folder { id: id.clone(), name: name.to_string(), parent_id, order: order.unwrap_or(0.0) };
    s.typer.state.edit_library(|lib| lib.save_folder(folder, style_ids.as_deref(), order.is_some()));
    Ok(json!({"id": id}))
}

fn folder_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.folder.delete";
    let id = id_param(p, "id", CMD)?.ok_or_else(|| bad(CMD, "pass \"id\""))?;
    let permanent = p.get("permanent").and_then(Value::as_bool).unwrap_or(false);
    if !s.typer.state.edit_library(|lib| lib.delete_folder(&id, permanent)) {
        return Err(bad(CMD, format!("no folder {id}")));
    }
    Ok(json!({"deleted": true}))
}

fn folder_duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.folder.duplicate";
    let id = id_param(p, "id", CMD)?.ok_or_else(|| bad(CMD, "pass \"id\""))?;
    let new_id = s.typer.state.edit_library(|lib| lib.duplicate_folder(&id)).ok_or_else(|| bad(CMD, format!("no folder {id}")))?;
    Ok(json!({"id": new_id}))
}

fn folder_reorder(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.folder.reorder";
    let parent = id_param(p, "parentId", CMD)?;
    let order = ids_param(p, "order", CMD)?.ok_or_else(|| bad(CMD, "pass \"order\": [folder ids]"))?;
    s.typer.state.edit_library(|lib| lib.reorder_folders(parent.as_ref(), &order));
    Ok(summary(&s.typer.state))
}

fn library_import(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.library.import";
    let data = match p.get("json") {
        Some(Value::String(text)) => exchange::parse(text).map_err(|e| bad(CMD, e))?,
        Some(v @ Value::Object(_)) => v.clone(),
        _ => return Err(bad(CMD, "pass \"json\": the TypeR export (text or object)")),
    };
    let kind = match p.get("kind").and_then(Value::as_str) {
        None => None,
        Some("folder") => Some(ImportKind::Folder),
        Some("library") => Some(ImportKind::Library),
        Some("full") => Some(ImportKind::Full),
        Some(k) => return Err(bad(CMD, format!("unknown kind \"{k}\": use folder, library or full"))),
    };
    let opts = ImportOptions { kind, keep_local: p.get("keepLocal").and_then(Value::as_bool).unwrap_or(true), now: now_ms(p) };
    let report = exchange::import(&mut s.typer.state, &data, opts).map_err(|e| bad(CMD, e))?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn library_export(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.library.export";
    let folders = match p.get("folders") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| if v.is_null() { Ok(None) } else { Id::from_value(v).map(Some).ok_or_else(|| bad(CMD, "\"folders\" must list folder ids or null")) })
                .collect::<Result<Vec<_>>>()?,
        ),
        Some(_) => return Err(bad(CMD, "\"folders\" must be a list")),
    };
    let opts = ExportOptions {
        folders,
        with_settings: p.get("withSettings").and_then(Value::as_bool).unwrap_or(false),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    Ok(exchange::export(&s.typer.state, &opts))
}

fn library_export_folder(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.library.exportFolder";
    let id = id_param(p, "id", CMD)?.ok_or_else(|| bad(CMD, "pass \"id\""))?;
    exchange::export_folder(&s.typer.state.library, &id).ok_or_else(|| bad(CMD, format!("no folder {id}")))
}

fn settings_get(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.typer.state.settings.to_json())
}

fn settings_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "typer.settings.set";
    let mut next = s.typer.state.settings.clone();
    if let Some(key) = p.get("key").and_then(Value::as_str) {
        next.set(key, p.get("value").unwrap_or(&Value::Null)).map_err(|e| bad(CMD, e))?;
    } else if let Some(values) = p.get("values").and_then(Value::as_object) {
        for (k, v) in values {
            next.set(k, v).map_err(|e| bad(CMD, e))?;
        }
    } else {
        return Err(bad(CMD, "pass \"key\" and \"value\", or \"values\""));
    }
    if let Some(d) = &next.default_style_id
        && s.typer.state.library.style(d).is_none()
    {
        return Err(bad(CMD, format!("no style {d}")));
    }
    s.typer.state.edit_settings(|st| *st = next);
    Ok(s.typer.state.settings.to_json())
}

#[cfg(test)]
mod tests;
