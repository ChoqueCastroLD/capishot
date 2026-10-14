// Ported from TypeR app_src/librarySerialization.js, app_src/components/modal/settings.jsx (import
// routing), app_src/components/modal/export.jsx and app_src/context.jsx (`import` merge)
// (develop 71bc061), MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! TypeR JSON import and export (K2–K4): full exports, library-only exports and folder exports,
//! with TypeR 3.0's validator in front of every import.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::id::Id;
use crate::library::{Folder, Library, Style};
use crate::state::State;

/// Format version written into exports (`"schema"`). Readers ignore unknown keys.
pub const SCHEMA: u32 = 1;
/// Largest import accepted, in bytes of JSON text.
pub const MAX_IMPORT_BYTES: usize = 32 << 20;
/// Objects and arrays may nest at most this deep (TypeR: `depth < 60`).
pub const MAX_DEPTH: usize = 60;

const FORBIDDEN_KEYS: [&str; 3] = ["__proto__", "prototype", "constructor"];

/// Why an import was refused.
pub fn invalid(why: impl Into<String>) -> String {
    format!("not a valid TypeR style library: {}", why.into())
}

fn is_object(v: &Value) -> bool {
    v.is_object()
}

fn check(cond: bool, why: &str) -> Result<(), String> {
    if cond { Ok(()) } else { Err(invalid(why)) }
}

fn check_tree(v: &Value, depth: usize) -> Result<(), String> {
    check(depth < MAX_DEPTH, "nested too deeply")?;
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                check(!FORBIDDEN_KEYS.contains(&k.as_str()), "forbidden key (__proto__, prototype or constructor)")?;
                check_tree(x, depth + 1)?;
            }
        }
        Value::Array(a) => {
            for x in a {
                check_tree(x, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_unique(items: &[Value], required: bool, what: &str) -> Result<(), String> {
    let mut ids: Vec<Id> = Vec::new();
    for item in items {
        check(is_object(item), &format!("{what} entries must be objects"))?;
        if !required && item.get("id").is_none() {
            continue;
        }
        let id = item.get("id").and_then(Id::from_value).ok_or_else(|| invalid(format!("{what} need valid ids")))?;
        check(!ids.contains(&id), &format!("duplicate {what} id {id}"))?;
        ids.push(id);
    }
    Ok(())
}

/// JavaScript's `Number(v)` for the values a library holds.
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = crate::pages::js_trim(s);
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        _ => f64::NAN,
    }
}

/// TypeR 3.0's import validator (`validateImportData`): the root is an object; no
/// `__proto__`/`prototype`/`constructor` keys and nesting below [`MAX_DEPTH`]; list fields are
/// lists; folders have unique valid ids, names and no parent cycles; styles have unique ids (when
/// in `styles`), names, an object `textProps`, string prefixes and positive size presets; tabs,
/// shortcuts, `uiLayout` and `usedLineStyles` are well formed.
pub fn validate(data: &Value) -> Result<(), String> {
    let o = data.as_object().ok_or_else(|| invalid("the root is not an object"))?;
    check_tree(data, 0)?;
    for field in ["folders", "styles", "exportedStyles", "tabs", "images", "ignoreLinePrefixes", "ignoreTags", "customThemes"] {
        if let Some(v) = o.get(field) {
            check(v.is_array(), &format!("\"{field}\" must be a list"))?;
        }
    }
    for field in ["text", "language", "direction"] {
        if let Some(v) = o.get(field) {
            check(v.is_string(), &format!("\"{field}\" must be text"))?;
        }
    }
    for field in ["ignoreLinePrefixes", "ignoreTags"] {
        if let Some(Value::Array(a)) = o.get(field) {
            check(a.iter().all(Value::is_string), &format!("\"{field}\" entries must be text"))?;
        }
    }
    if let Some(Value::Array(folders)) = o.get("folders") {
        check_unique(folders, true, "folder")?;
        let parents: Vec<(Id, Option<&Value>)> = folders.iter().filter_map(|f| Some((f.get("id").and_then(Id::from_value)?, f.get("parentId")))).collect();
        let parent_of = |id: &Id| parents.iter().find(|(i, _)| i == id).map(|(_, p)| *p);
        for f in folders {
            check(f.get("name").is_some_and(Value::is_string), "folder names must be text")?;
            let Some(start) = f.get("id").and_then(Id::from_value) else { continue };
            let mut visited = vec![start];
            let mut parent = f.get("parentId");
            // Walk while the parent is a known folder; a repeat is a cycle.
            while let Some(p) = parent.filter(|p| !p.is_null()).and_then(Id::from_value) {
                let Some(next) = parent_of(&p) else { break };
                check(!visited.contains(&p), "folders form a cycle")?;
                visited.push(p);
                parent = next;
            }
        }
    }
    let (styles, required) = match (o.get("styles"), o.get("exportedStyles")) {
        (Some(Value::Array(s)), _) => (Some(s), true),
        (_, Some(Value::Array(s))) => (Some(s), false),
        _ => (None, false),
    };
    if let Some(styles) = styles {
        check_unique(styles, required, "style")?;
        for s in styles {
            check(s.get("name").is_some_and(Value::is_string), "style names must be text")?;
            if let Some(tp) = s.get("textProps") {
                check(tp.is_object(), "\"textProps\" must be an object")?;
            }
            if let Some(p) = s.get("prefixes") {
                check(p.as_array().is_some_and(|a| a.iter().all(Value::is_string)), "\"prefixes\" must be a list of text")?;
            }
            if let Some(p) = s.get("sizePresets") {
                check(
                    p.as_array().is_some_and(|a| a.iter().all(|v| js_number(v).is_finite() && js_number(v) > 0.0)),
                    "\"sizePresets\" must be positive numbers",
                )?;
            }
        }
    }
    if let Some(Value::Array(tabs)) = o.get("tabs") {
        check(!tabs.is_empty(), "\"tabs\" is empty")?;
        check_unique(tabs, true, "tab")?;
        for t in tabs {
            check(t.get("text").is_some_and(Value::is_string), "tab text must be text")?;
            if let Some(i) = t.get("images") {
                check(i.is_array(), "tab images must be a list")?;
            }
            if let Some(i) = t.get("currentLineIndex") {
                check(i.as_f64().is_some_and(|n| n >= 0.0 && n.fract() == 0.0), "tab line index must be a whole number")?;
            }
        }
    }
    if let Some(sc) = o.get("shortcut") {
        let m = sc.as_object().ok_or_else(|| invalid("\"shortcut\" must be an object"))?;
        check(m.values().all(|v| v.as_array().is_some_and(|a| a.iter().all(Value::is_string))), "shortcuts must be lists of keys")?;
    }
    for field in ["uiLayout", "usedLineStyles"] {
        if let Some(v) = o.get(field) {
            check(v.is_object(), &format!("\"{field}\" must be an object"))?;
        }
    }
    Ok(())
}

/// Parse and validate import text.
pub fn parse(text: &str) -> Result<Value, String> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(invalid(format!("the file is larger than {} MB", MAX_IMPORT_BYTES >> 20)));
    }
    let v: Value = serde_json::from_str(text).map_err(|e| invalid(format!("JSON error: {e}")))?;
    validate(&v)?;
    Ok(v)
}

/// How an import is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportKind {
    /// A folder export (`exportedStyles`): one new folder, new style ids.
    Folder,
    /// Folders and styles only: added with new ids, parents preserved.
    Library,
    /// A full export or backup: folders replaced, styles merged by `edited`, settings applied.
    Full,
}

/// TypeR's routing: folder export, library-only (folders + styles and no settings keys), else full.
pub fn classify(data: &Value) -> ImportKind {
    if data.get("exportedStyles").is_some() {
        return ImportKind::Folder;
    }
    let truthy = |k: &str| match data.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => true,
    };
    let settings_keys = ["ignoreLinePrefixes", "ignoreTags", "defaultStyleId", "language", "autoClosePSD", "autoScrollStyle", "pastePointText", "textItemKind"];
    let library_only = data.get("folders").is_some()
        && data.get("styles").is_some()
        && data.get("includesSettings") != Some(&Value::Bool(true))
        && !settings_keys.iter().any(|k| truthy(k));
    if library_only { ImportKind::Library } else { ImportKind::Full }
}

/// Options of [`import`].
#[derive(Clone, Copy, Debug)]
pub struct ImportOptions {
    /// Force a kind instead of TypeR's routing.
    pub kind: Option<ImportKind>,
    /// Full import: keep local styles that are missing from the import or edited later (TypeR
    /// asks; Capishot defaults to keeping them).
    pub keep_local: bool,
    /// Now, in ms since the Unix epoch, for the `edited` stamp of imported styles (`None` keeps
    /// what the file says).
    pub now: Option<u64>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { kind: None, keep_local: true, now: None }
    }
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub kind: Option<ImportKind>,
    pub folders: usize,
    pub styles: usize,
    /// Full import: local styles kept because they were missing from the import or newer.
    pub kept_local: usize,
    pub settings: Vec<String>,
    /// Folder import: the new folder.
    pub folder_id: Option<Id>,
    pub script: bool,
}

fn list(data: &Value, key: &str) -> Vec<Value> {
    data.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

fn stamp(style: &mut Style, now: Option<u64>) {
    if let Some(n) = now {
        style.edited = Some(n.into());
    }
}

fn default_text_type(style: &mut Style) {
    if style.props.get("textType").and_then(Value::as_str).is_none_or(str::is_empty) {
        style.props.insert("textType".into(), Value::String("inherit".into()));
    }
}

/// Import validated TypeR JSON into `state`.
pub fn import(state: &mut State, data: &Value, opts: ImportOptions) -> Result<ImportReport, String> {
    validate(data)?;
    // Work on a copy so a bad value halfway through changes nothing.
    let mut next = state.clone();
    let report = import_into(&mut next, data, opts)?;
    *state = next;
    Ok(report)
}

fn import_into(state: &mut State, data: &Value, opts: ImportOptions) -> Result<ImportReport, String> {
    let kind = opts.kind.unwrap_or_else(|| classify(data));
    let mut report = ImportReport { kind: Some(kind), ..ImportReport::default() };
    match kind {
        ImportKind::Folder => {
            let exported = data.get("exportedStyles").and_then(Value::as_array).ok_or_else(|| invalid("a folder export needs \"exportedStyles\""))?;
            let name = data.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("Imported").to_string();
            let lib = &mut state.library;
            let folder_id = lib.fresh_id("f");
            let order = lib.folders.iter().filter(|f| f.parent_id.is_none()).count() as f64;
            lib.folders.push(Folder { id: folder_id.clone(), name, parent_id: None, order });
            for s in exported {
                let Some(o) = s.as_object() else { continue };
                let id = lib.fresh_id("s");
                let mut style = Style::from_map_with_id(o, id);
                style.folder = Some(folder_id.clone());
                default_text_type(&mut style);
                stamp(&mut style, opts.now);
                lib.styles.push(style);
                report.styles += 1;
            }
            report.folders = 1;
            report.folder_id = Some(folder_id);
        }
        ImportKind::Library => {
            let folders = list(data, "folders");
            let lib = &mut state.library;
            let mut id_map: Vec<(Id, Id)> = Vec::new();
            for f in &folders {
                let Some(old) = f.get("id").and_then(Id::from_value) else { continue };
                let new = lib.fresh_id("f");
                // Reserve the id before the next one is picked.
                lib.folders.push(Folder { id: new.clone(), name: String::new(), parent_id: None, order: 0.0 });
                id_map.push((old, new));
            }
            let mapped = |v: Option<&Value>| Id::opt_from_value(v).and_then(|old| id_map.iter().find(|(o, _)| *o == old).map(|(_, n)| n.clone()));
            for f in &folders {
                let Some(new) = mapped(f.get("id")) else { continue };
                if let Some(slot) = lib.folders.iter_mut().find(|x| x.id == new) {
                    slot.name = f.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                    slot.parent_id = mapped(f.get("parentId"));
                    slot.order = f.get("order").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
                }
                report.folders += 1;
            }
            for s in list(data, "styles") {
                let Some(o) = s.as_object() else { continue };
                let id = lib.fresh_id("s");
                let mut style = Style::from_map_with_id(o, id);
                style.folder = mapped(o.get("folder"));
                default_text_type(&mut style);
                stamp(&mut style, opts.now);
                lib.styles.push(style);
                report.styles += 1;
            }
        }
        ImportKind::Full => {
            if let Some(Value::Array(folders)) = data.get("folders") {
                state.library.folders = folders.iter().filter_map(Folder::from_json).collect();
                report.folders = state.library.folders.len();
            }
            if let Some(Value::Array(styles)) = data.get("styles") {
                let incoming: Vec<Style> = styles.iter().filter_map(Style::from_json).collect();
                let (merged, kept) = merge_styles(&state.library.styles, incoming, opts.keep_local);
                report.styles = merged.len() - kept;
                report.kept_local = kept;
                state.library.styles = merged;
            }
            let mut settings = data.as_object().cloned().unwrap_or_default();
            if !settings.contains_key("pastePointText")
                && let Some(kind) = settings.get("textItemKind").cloned()
            {
                let point = match kind {
                    Value::Bool(b) => b,
                    Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    Value::String(s) => !s.is_empty(),
                    _ => false,
                };
                settings.insert("pastePointText".into(), Value::Bool(point));
            }
            report.settings = state.settings.merge(&settings)?;
            let script = data.get("text").and_then(Value::as_str).map(str::to_string).or_else(|| active_tab_text(data));
            if let Some(text) = script {
                state.set_text(&text)?;
                report.script = true;
            }
        }
    }
    state.library.normalize();
    state.refresh(false);
    Ok(report)
}

/// The script of the active tab of a TypeR 3.0 backup.
fn active_tab_text(data: &Value) -> Option<String> {
    let tabs = data.get("tabs")?.as_array()?;
    let current = data.get("currentTabId");
    let tab = tabs.iter().find(|t| current.is_some() && t.get("id") == current).or_else(|| tabs.first())?;
    tab.get("text")?.as_str().map(str::to_string)
}

fn edited(s: &Style) -> Option<f64> {
    s.edited.as_ref().and_then(serde_json::Number::as_f64).filter(|e| *e != 0.0)
}

/// TypeR's style merge for a full import. Returns the styles and how many local ones were kept.
pub fn merge_styles(local: &[Style], incoming: Vec<Style>, keep_local: bool) -> (Vec<Style>, usize) {
    let mut out: Vec<Style> = Vec::new();
    // Ask (here: `keep_local`) only if some local style is missing from the import or newer.
    let conflicted = |s: &Style| match incoming.iter().find(|i| i.id == s.id) {
        None => true,
        Some(i) => match (edited(s), edited(i)) {
            (Some(_), None) => true,
            (Some(a), Some(b)) => a > b,
            _ => false,
        },
    };
    let keep = keep_local && local.iter().any(conflicted);
    let mut kept = 0;
    if keep {
        for s in local.iter().filter(|s| conflicted(s)) {
            out.push(s.clone());
            kept += 1;
        }
    }
    for s in incoming {
        if !keep {
            out.push(s);
            continue;
        }
        let old = local.iter().find(|o| o.id == s.id);
        let take = match (old.and_then(edited), edited(&s)) {
            (None, _) => true,
            (Some(o), Some(n)) => n >= o,
            (Some(_), None) => false,
        };
        if take {
            out.push(s);
        }
    }
    (out, kept)
}

/// Options of [`export`].
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// Folders to include (`None` inside = Unsorted); `None` = everything.
    pub folders: Option<Vec<Option<Id>>>,
    /// Also write the settings TypeR exports with a library.
    pub with_settings: bool,
    /// App version written as `version`.
    pub version: String,
}

/// A TypeR full export (`{folders, styles, includesSettings, version, schema, [settings…]}`).
/// A selected folder whose parent isn't selected becomes top level.
pub fn export(state: &State, opts: &ExportOptions) -> Value {
    let lib = &state.library;
    let selected = |f: &Option<Id>| opts.folders.as_ref().is_none_or(|sel| sel.contains(f));
    let folders: Vec<Value> = lib
        .folders
        .iter()
        .filter(|f| selected(&Some(f.id.clone())))
        .map(|f| {
            let mut f = f.clone();
            if f.parent_id.as_ref().is_some_and(|p| !selected(&Some(p.clone()))) {
                f.parent_id = None;
            }
            f.to_json()
        })
        .collect();
    let styles: Vec<Value> = lib.styles.iter().filter(|s| selected(&s.folder)).map(Style::to_json).collect();
    let mut out = json!({
        "folders": folders,
        "styles": styles,
        "includesSettings": opts.with_settings,
        "version": opts.version,
        "schema": SCHEMA,
    });
    if opts.with_settings
        && let (Some(o), Value::Object(all)) = (out.as_object_mut(), state.settings.to_json())
    {
        for k in crate::settings::EXPORTED_KEYS {
            if let Some(v) = all.get(*k) {
                o.insert((*k).to_string(), v.clone());
            }
        }
    }
    out
}

/// A TypeR folder export (`{name, exportedStyles}`) of the styles directly in folder `id`.
pub fn export_folder(lib: &Library, id: &Id) -> Option<Value> {
    let folder = lib.folder(id)?;
    let styles: Vec<Value> = lib
        .styles
        .iter()
        .filter(|s| s.folder.as_ref() == Some(id))
        .map(|s| {
            let mut s = s.clone();
            default_text_type(&mut s);
            s.to_json()
        })
        .collect();
    let mut o = Map::new();
    o.insert("name".into(), Value::String(folder.name.clone()));
    o.insert("exportedStyles".into(), Value::Array(styles));
    o.insert("schema".into(), json!(SCHEMA));
    Some(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn style_json() -> Value {
        json!({"id": "s", "name": "Dialogue", "folder": null, "textProps": {}, "prefixes": ["REG:"], "prefixesDisabled": true,
            "sizePresets": [12, 16, 20], "autoSizeByPageWidth": true, "sizePresetDefaultIndex": 1, "sizePresetMinWidths": [100, null, 2000]})
    }

    // scripts/testLibraryRoundTrip.js
    #[test]
    fn serializer_and_validator_match_typer() {
        let style = style_json();
        assert_eq!(Style::from_json(&style).unwrap().to_json(), style);
        assert!(validate(&json!({"folders": [], "styles": [style.clone()]})).is_ok());
        let cycle = json!([{"id": "a", "name": "A", "parentId": "b"}, {"id": "b", "name": "B", "parentId": "a"}]);
        let rejected = [
            Value::Null,
            json!([]),
            json!({"styles": "bad"}),
            json!({"folders": cycle.clone()}),
            json!({"folders": [cycle[0].clone(), cycle[0].clone()]}),
            json!({"tabs": [null]}),
            json!({"ignoreTags": [2]}),
            serde_json::from_str::<Value>(r#"{"__proto__":{}}"#).unwrap(),
            json!({"styles": [{"id": "s", "name": "X", "prefixes": "bad"}]}),
        ];
        for data in rejected {
            assert!(validate(&data).is_err(), "{data}");
        }
        // The reducer refuses a cyclic library: state unchanged.
        let mut state = State::new(Library::default(), Settings::default());
        let before = state.clone();
        let mut bad = json!({"folders": cycle, "styles": [style.clone()]});
        bad["styles"][0]["folder"] = json!("a");
        assert!(import(&mut state, &bad, ImportOptions::default()).is_err());
        assert_eq!(state, before);
        // A good library import keeps the presets.
        let mut good = json!({"folders": [{"id": "f", "name": "Folder"}], "styles": [style]});
        good["styles"][0]["folder"] = json!("f");
        import(&mut state, &good, ImportOptions::default()).unwrap();
        assert_eq!(state.library.styles[0].props["sizePresets"], json!([12, 16, 20]));
    }

    #[test]
    fn validator_limits_depth_and_nested_forbidden_keys() {
        let mut deep = json!(1);
        for _ in 0..MAX_DEPTH {
            deep = json!([deep]);
        }
        assert!(validate(&json!({"x": deep})).is_err());
        assert!(validate(&serde_json::from_str::<Value>(r#"{"styles":[{"id":"a","name":"n","textProps":{"constructor":1}}]}"#).unwrap()).is_err());
        assert!(validate(&json!({"styles": [{"id": "a", "name": "n", "sizePresets": [0]}]})).is_err());
        assert!(validate(&json!({"styles": [{"id": "a", "name": "n", "sizePresets": ["12"]}]})).is_ok());
        assert!(validate(&json!({"exportedStyles": [{"name": "n"}, {"name": "m"}]})).is_ok(), "folder exports have no ids");
        assert!(validate(&json!({"styles": [{"name": "n"}]})).is_err(), "library styles need ids");
        assert!(validate(&json!({"folders": [{"id": "a", "name": "A", "parentId": "ghost"}]})).is_ok(), "dangling parents are repaired, not rejected");
        assert!(validate(&json!({"shortcut": {"add": ["WIN", 3]}})).is_err());
        assert!(validate(&json!({"usedLineStyles": []})).is_err());
        assert!(validate(&json!({"tabs": []})).is_err());
        assert!(validate(&json!({"tabs": [{"id": "t", "text": "x", "currentLineIndex": 1.5}]})).is_err());
        assert!(validate(&json!({"text": 5})).is_err());
        assert!(parse("{").is_err());
        assert!(parse(&" ".repeat(MAX_IMPORT_BYTES + 1)).is_err());
    }

    #[test]
    fn routing_follows_typer() {
        assert_eq!(classify(&json!({"name": "F", "exportedStyles": []})), ImportKind::Folder);
        assert_eq!(classify(&json!({"folders": [], "styles": [], "version": "3.0", "includesSettings": false})), ImportKind::Library);
        assert_eq!(classify(&json!({"folders": [], "styles": [], "ignoreTags": ["**"]})), ImportKind::Full);
        assert_eq!(classify(&json!({"folders": [], "styles": [], "ignoreTags": []})), ImportKind::Full, "an empty list is truthy in JS");
        assert_eq!(classify(&json!({"folders": [], "styles": [], "includesSettings": true})), ImportKind::Full);
        assert_eq!(classify(&json!({"styles": []})), ImportKind::Full);
    }

    #[test]
    fn folder_import_makes_a_new_folder_with_new_ids() {
        let mut state = State::new(Library::default(), Settings::default());
        let data = json!({"name": "Series", "exportedStyles": [{"name": "Reg", "textProps": {}, "prefixes": ["R:"]}, {"id": "keepme", "name": "SFX"}]});
        let r = import(&mut state, &data, ImportOptions { now: Some(42), ..ImportOptions::default() }).unwrap();
        assert_eq!(r.kind, Some(ImportKind::Folder));
        let fid = r.folder_id.unwrap();
        assert_eq!(state.library.folder(&fid).unwrap().name, "Series");
        assert_eq!(state.library.styles.len(), 2);
        assert!(state.library.styles.iter().all(|s| s.folder.as_ref() == Some(&fid) && s.edited == Some(42.into())));
        assert!(state.library.styles.iter().all(|s| s.id != Id::str("keepme")));
        assert_eq!(state.library.styles[1].props["textType"], "inherit");
        let back = export_folder(&state.library, &fid).unwrap();
        assert_eq!(back["exportedStyles"][0]["prefixes"], json!(["R:"]));
        assert_eq!(back["exportedStyles"][1]["prefixes"], json!([]));
        assert_eq!(export_folder(&state.library, &Id::str("nope")), None);
    }

    #[test]
    fn library_import_remaps_ids_and_keeps_parents() {
        let mut state = State::new(Library::default(), Settings::default());
        let data = json!({"folders": [{"id": "p", "name": "Parent"}, {"id": "c", "name": "Child", "parentId": "p"}],
            "styles": [{"id": "s1", "name": "A", "folder": "c"}, {"id": "s2", "name": "B", "folder": null}]});
        let r = import(&mut state, &data, ImportOptions::default()).unwrap();
        assert_eq!((r.kind, r.folders, r.styles), (Some(ImportKind::Library), 2, 2));
        let tree = state.library.flatten_tree();
        assert_eq!(tree.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["Parent", "Parent / Child"]);
        let child = tree[1].id.clone();
        assert_ne!(child, Id::str("c"));
        assert_eq!(state.library.styles.iter().find(|s| s.name == "A").unwrap().folder, Some(child));
        // Importing twice duplicates instead of colliding.
        import(&mut state, &data, ImportOptions::default()).unwrap();
        assert_eq!(state.library.folders.len(), 4);
    }

    fn st(id: &str, edited: Option<u64>) -> Style {
        let mut s = Style::from_json(&json!({"id": id, "name": id})).unwrap();
        s.edited = edited.map(Into::into);
        s
    }

    fn ids(v: &[Style]) -> Vec<String> {
        v.iter().map(|s| s.id.to_string()).collect()
    }

    // 2.5 behaviour: the full import merges by `edited`.
    #[test]
    fn full_import_merges_by_edited() {
        let local = vec![st("only-local", Some(5)), st("newer-local", Some(20)), st("older-local", Some(1)), st("same", Some(7))];
        let incoming = vec![st("newer-local", Some(10)), st("older-local", Some(9)), st("same", Some(7)), st("new", None)];
        let (merged, kept) = merge_styles(&local, incoming.clone(), true);
        assert_eq!(ids(&merged), ["only-local", "newer-local", "older-local", "same", "new"]);
        assert_eq!(kept, 2);
        assert_eq!(merged[2].edited, Some(9.into()), "the imported, newer copy wins");
        assert_eq!(merged[1].edited, Some(20.into()), "the newer local copy stays");
        let (replaced, kept) = merge_styles(&local, incoming, false);
        assert_eq!(ids(&replaced), ["newer-local", "older-local", "same", "new"]);
        assert_eq!(kept, 0);
    }

    /// A TypeR 3.0 full export, written by hand from the schema.
    fn typer3_export() -> Value {
        json!({
            "folders": [{"id": "k3x9a1", "name": "One Piece", "parentId": null, "order": 0}, {"id": "q8w2e7", "name": "Ch 1100", "parentId": "k3x9a1", "order": 0}],
            "styles": [
                {"id": "g7h8i9", "name": "Note", "folder": null, "edited": 1760000000002u64, "prefixes": ["NB:"]},
                {"id": "a1b2c3", "name": "Dialogue", "folder": "k3x9a1", "edited": 1760000000000u64, "textType": "inherit",
                 "textProps": crate::library::default_text_props(), "prefixes": [], "prefixColor": "#FFF3B0", "prefixesDisabled": false,
                 "stroke": {"enabled": false, "size": 0, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}},
                 "sizePresets": [14, 18], "autoSizeByPageWidth": true, "sizePresetDefaultIndex": 0, "sizePresetMinWidths": [null, 1600]},
                {"id": "d4e5f6", "name": "SFX", "folder": "q8w2e7", "edited": 1760000000001u64, "textType": "point",
                 "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"fontName": "Komika", "size": 40.5, "syntheticBold": true}}]}, "typeUnit": "pointsUnit"},
                 "prefixes": ["SFX:", "FX:"], "prefixColor": "#FFA6A4"}
            ],
            "profileName": "Default",
            "includesSettings": true,
            "version": "3.0.0",
            "exported": "2026-10-09T12:00:00.000Z",
            "ignoreLinePrefixes": ["##", "//"],
            "ignoreTags": ["**"],
            "defaultStyleId": "a1b2c3",
            "language": "es",
            "autoClosePSD": false,
            "autoScrollStyle": true,
            "currentFolderTagPriority": true,
            "pastePointText": false
        })
    }

    #[test]
    fn typer3_export_round_trips_losslessly() {
        let data = typer3_export();
        let mut state = State::new(Library::default(), Settings::default());
        let r = import(&mut state, &data, ImportOptions::default()).unwrap();
        assert_eq!(r.kind, Some(ImportKind::Full));
        assert_eq!(state.settings.ignore_line_prefixes, ["##", "//"]);
        assert_eq!(state.settings.default_style_id, Some(Id::str("a1b2c3")));
        let out = export(&state, &ExportOptions { folders: None, with_settings: true, version: "x".into() });
        assert_eq!(out["folders"], data["folders"]);
        assert_eq!(out["styles"], data["styles"], "styles re-export byte-equivalent modulo key order");
        for k in crate::settings::EXPORTED_KEYS {
            assert_eq!(out[*k], data[*k], "{k}");
        }
        assert_eq!(out["schema"], json!(SCHEMA));
    }

    #[test]
    fn typer25_export_imports() {
        // 2.5: no parentId/order, `textItemKind` instead of `pastePointText`.
        let data = json!({
            "folders": [{"id": "f1", "name": "Manga"}],
            "styles": [{"id": "s1", "name": "Bubble", "folder": "f1", "edited": 1700000000000u64, "textProps": crate::library::default_text_props(),
                        "prefixes": ["B:"], "prefixColor": "#CDFDD3", "stroke": {"enabled": true, "size": 3, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}}}],
            "version": "2.5.0",
            "exported": "2026-01-06T00:00:00.000Z",
            "ignoreLinePrefixes": ["##"],
            "ignoreTags": [],
            "defaultStyleId": null,
            "language": "auto",
            "autoClosePSD": false,
            "autoScrollStyle": true,
            "currentFolderTagPriority": true,
            "textItemKind": 1
        });
        let mut state = State::new(Library::default(), Settings::default());
        import(&mut state, &data, ImportOptions::default()).unwrap();
        assert!(state.settings.paste_point_text, "textItemKind maps to pastePointText");
        let out = export(&state, &ExportOptions { with_settings: false, ..ExportOptions::default() });
        assert_eq!(out["styles"], data["styles"]);
        assert_eq!(out["folders"], json!([{"id": "f1", "name": "Manga", "parentId": null, "order": 0}]));
        assert!(out.get("ignoreTags").is_none());
    }

    #[test]
    fn partial_export_detaches_unselected_parents() {
        let mut state = State::new(Library::default(), Settings::default());
        import(&mut state, &typer3_export(), ImportOptions::default()).unwrap();
        let out = export(&state, &ExportOptions { folders: Some(vec![Some(Id::str("q8w2e7"))]), with_settings: false, version: String::new() });
        assert_eq!(out["folders"], json!([{"id": "q8w2e7", "name": "Ch 1100", "parentId": null, "order": 0}]));
        assert_eq!(out["styles"].as_array().unwrap().len(), 1);
        let unsorted = export(&state, &ExportOptions { folders: Some(vec![None]), with_settings: false, version: String::new() });
        assert_eq!(unsorted["styles"][0]["name"], "Note");
    }

    #[test]
    fn full_import_restores_the_active_tab_script() {
        let mut state = State::new(Library::default(), Settings::default());
        let data = json!({"tabs": [{"id": "t1", "text": "one"}, {"id": "t2", "text": "Page 1\ntwo"}], "currentTabId": "t2"});
        let r = import(&mut state, &data, ImportOptions::default()).unwrap();
        assert!(r.script);
        assert_eq!(state.text, "Page 1\ntwo");
    }
}
