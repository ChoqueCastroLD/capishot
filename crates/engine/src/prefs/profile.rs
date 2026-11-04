//! The user's profile: their personalisation (theme, shortcuts, workspace, preferences, font
//! library, font projects, presets, brushes, TypeR's styles) as named sections of preferences,
//! for Preferences › Profile & Themes. A `.proj.capishot` (`crate::font_projects::export`)
//! carries them in `profile.json`; this module captures and applies them, and packs and reads a
//! profile without font files for agents (`profile.export`, `profile.import`).
//!
//! A section is a map of preference paths (as `prefs.get` names them) to values. Applying one
//! sets only the paths its section owns, so a profile can't reach anything else (agent control,
//! plug-in folders, scratch disks, recent files). Applying either replaces each value or merges
//! it into the current one ([`merge`]), and first keeps the profile as it was in
//! `profileBackup`, which `profile.undoImport` restores.

use serde_json::{Map, Value, json};

use super::cmds::bad;
use super::validate::{get_path, set_path};
use super::{CommandSpec, Result, Session};

/// Every section this module captures and applies, with its English label.
pub const SECTIONS: [(&str, &str); 9] = [
    ("theme", "Theme and custom themes"),
    ("shortcuts", "Keyboard shortcuts, menus and toolbar"),
    ("workspace", "Workspace and panel layout"),
    ("preferences", "General preferences"),
    ("fonts", "Favourite fonts, font folders and names"),
    ("fontProjects", "Font projects"),
    ("presets", "Presets: swatches, gradients, styles, patterns, shapes, tool presets"),
    ("brushes", "Brushes"),
    ("typer", "TypeR styles"),
];

/// The preference paths each section owns. `section.*` stands for each key of that section
/// that no other section owns and [`PRIVATE`] doesn't list.
fn paths(section: &str) -> &'static [&'static str] {
    match section {
        "theme" => &[
            "interface.theme",
            "interface.appearanceMode",
            "interface.darkTheme",
            "interface.lightTheme",
            "interface.favoriteThemes",
            "interface.customThemes",
            "interface.customTheme",
        ],
        "shortcuts" => &["shortcuts", "alternateShortcuts", "shortcutSets", "shortcutSet", "menus", "toolbar"],
        "workspace" => &["workspace.*", "workspaces", "workspaceLocked", "panelLayout"],
        "fonts" => &["type.favoriteFonts", "type.fontFolders", "type.fontAliases"],
        "fontProjects" => &["fontProjects"],
        "presets" => &["presets"],
        "preferences" => &[
            "general.*",
            "interface.*",
            "tools.*",
            "historyLog.*",
            "fileHandling.*",
            "export.*",
            "cursors.*",
            "transparencyAndGamut.*",
            "unitsAndRulers.*",
            "guidesGridAndSlices.*",
            "type.*",
            "enhancedControls.*",
            "rawDefaults.*",
            "historyOptions",
            "layerMaskOverlay",
            "colorSettings",
            "userDictionary",
            "viewExtras",
            "toolOptions",
            "dialogs",
            "brushPicker",
        ],
        _ => &[],
    }
}

/// Paths of this machine, never carried: where files are, which files were open.
const PRIVATE: [&str; 3] = ["historyLog.filePath", "fileHandling.recentFiles", "fileHandling.lutFolder"];

/// The sections kept in the backup taken before an import (all but brushes, which live in their
/// own store).
const BACKED_UP: [&str; 8] = ["theme", "shortcuts", "workspace", "preferences", "fonts", "fontProjects", "presets", "typer"];

fn owned_elsewhere(section: &str, path: &str) -> bool {
    PRIVATE.contains(&path) || SECTIONS.iter().any(|(other, _)| *other != section && paths(other).contains(&path))
}

/// Does `section` own `path`?
fn owns(section: &str, path: &str) -> bool {
    paths(section).iter().any(|p| match p.strip_suffix(".*") {
        Some(prefix) => path.strip_prefix(prefix).and_then(|k| k.strip_prefix('.')).is_some_and(|k| !k.is_empty() && !k.contains('.')),
        None => *p == path,
    }) && !owned_elsewhere(section, path)
}

/// The current values of `section` (`None` for a section this module doesn't know).
fn capture_section(s: &mut Session, root: &Value, section: &str) -> Option<Value> {
    match section {
        "brushes" => s.execute("edit.presets.exportImportPresets", json!({"action": "export", "kinds": ["brushes"]})).ok().map(|v| v["data"].clone()),
        "typer" => s.execute("typer.library.export", json!({"withSettings": true})).ok(),
        _ if paths(section).is_empty() => None,
        _ => {
            let mut out = Map::new();
            for p in paths(section) {
                match p.strip_suffix(".*") {
                    Some(prefix) => {
                        for (k, v) in get_path(root, prefix).and_then(Value::as_object).into_iter().flatten() {
                            let path = format!("{prefix}.{k}");
                            if owns(section, &path) {
                                out.insert(path, v.clone());
                            }
                        }
                    }
                    None => {
                        if let Some(v) = get_path(root, p).filter(|_| owns(section, p)) {
                            out.insert((*p).to_string(), v.clone());
                        }
                    }
                }
            }
            Some(Value::Object(out))
        }
    }
}

/// The current values of `sections` (unknown ones are left out).
pub fn capture(s: &mut Session, sections: &[&str]) -> Map<String, Value> {
    let root = s.prefs_value();
    sections.iter().filter_map(|sec| Some((sec.to_string(), capture_section(s, &root, sec)?))).collect()
}

/// Merge `incoming` into `cur`: a map takes the incoming keys (one level deep, so a preset set's
/// groups merge too), a list adds the incoming items, replacing those with the same `id` or
/// `name`; anything else is replaced.
pub fn merge(cur: &mut Value, incoming: Value) {
    merge_at(cur, incoming, 1);
}

fn merge_at(cur: &mut Value, incoming: Value, depth: u8) {
    match (cur, incoming) {
        (Value::Object(c), Value::Object(inc)) => {
            for (k, v) in inc {
                match c.get_mut(&k) {
                    Some(existing) if depth > 0 => merge_at(existing, v, depth - 1),
                    _ => {
                        c.insert(k, v);
                    }
                }
            }
        }
        (Value::Array(c), Value::Array(inc)) => {
            let key = |v: &Value| ["id", "name"].iter().find_map(|k| v.get(*k).and_then(Value::as_str).map(str::to_string));
            for item in inc {
                match key(&item).and_then(|k| c.iter().position(|e| key(e).as_deref() == Some(k.as_str()))) {
                    Some(i) => {
                        if let Some(slot) = c.get_mut(i) {
                            *slot = item;
                        }
                    }
                    None if !c.contains(&item) => c.push(item),
                    None => {}
                }
            }
        }
        (cur, inc) => *cur = inc,
    }
}

fn get_mut<'a>(root: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    path.split('.').try_fold(root, |v, k| v.get_mut(k))
}

/// Apply `sections` of `profile` (replacing or merging), without a backup. Returns the sections
/// applied.
fn apply_sections(s: &mut Session, profile: &Map<String, Value>, sections: &[String], merge_values: bool) -> Result<Vec<String>> {
    const CMD: &str = "profile.apply";
    let mut root = s.prefs_value();
    let mut applied = Vec::new();
    let mut prefs_changed = false;
    for sec in sections {
        let Some(data) = profile.get(sec.as_str()).filter(|v| !v.is_null()) else { continue };
        match sec.as_str() {
            "brushes" => {
                s.execute("edit.presets.exportImportPresets", json!({"action": "import", "kinds": ["brushes"], "data": data}))?;
            }
            "typer" => {
                // Replacing takes the library as it was exported; merging lets TypeR tell a full
                // export (folders replaced, styles merged) from a library one (added).
                let kind = if merge_values { Value::Null } else { json!("full") };
                s.execute("typer.library.import", json!({"json": data, "kind": kind, "keepLocal": merge_values}))?;
            }
            _ if paths(sec).is_empty() => continue,
            _ => {
                let entries = data.as_object().ok_or_else(|| bad(CMD, format!("section `{sec}` must be an object of preference paths")))?;
                for (path, v) in entries {
                    if !owns(sec, path) {
                        continue;
                    }
                    match get_mut(&mut root, path) {
                        Some(cur) if merge_values => merge(cur, v.clone()),
                        Some(cur) => *cur = v.clone(),
                        None => set_path(&mut root, path, v.clone()).map_err(|e| bad(CMD, e))?,
                    }
                }
                prefs_changed = true;
            }
        }
        applied.push(sec.clone());
    }
    if prefs_changed {
        s.load_prefs_json(&root.to_string()).map_err(|e| bad(CMD, e))?;
        s.presets_changed();
    }
    Ok(applied)
}

fn section_list(p: &Value, profile: &Map<String, Value>) -> Vec<String> {
    match p.get("sections").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => profile.keys().cloned().collect(),
    }
}

/// `profile.apply`: keep the current profile (plus the shell's `extra`) as the backup, then apply.
fn apply_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "profile.apply";
    let profile = p.get("profile").and_then(Value::as_object).ok_or_else(|| bad(CMD, "missing `profile` (sections by name)"))?;
    let sections = section_list(p, profile);
    let merge_values = p.get("merge").and_then(Value::as_bool).unwrap_or(false);
    let mut backup = capture(s, &BACKED_UP);
    if let Some(extra) = p.get("extra").and_then(Value::as_object) {
        backup.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    s.prefs.edit(|pr| pr.profile_backup = Value::Object(backup));
    let applied = apply_sections(s, profile, &sections, merge_values)?;
    let ignored: Vec<&String> = sections.iter().filter(|x| !applied.contains(x)).collect();
    Ok(json!({"applied": applied, "ignored": ignored}))
}

/// `profile.undoImport`: the profile as it was before the last import. Returns the backup's
/// sections this module doesn't apply (the shell's `extra`) as `extra`.
fn undo_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    const CMD: &str = "profile.undoImport";
    let Some(backup) = s.prefs().profile_backup.as_object().cloned() else {
        return Err(bad(CMD, "no profile import to undo"));
    };
    let sections: Vec<String> = backup.keys().cloned().collect();
    let restored = apply_sections(s, &backup, &sections, false)?;
    s.prefs.edit(|pr| pr.profile_backup = Value::Null);
    let extra: Map<String, Value> = backup.into_iter().filter(|(k, _)| !restored.contains(k)).collect();
    Ok(json!({"restored": restored, "extra": extra}))
}

fn capture_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let all: Vec<&str> = SECTIONS.iter().map(|(id, _)| *id).collect();
    let sections: Vec<&str> = match p.get("sections").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).collect(),
        None => all,
    };
    Ok(Value::Object(capture(s, &sections)))
}

/// `profile.export`: the sections as a `.proj.capishot` (no font files; the shell adds those).
fn export_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "profile.export";
    let Value::Object(profile) = capture_cmd(s, p)? else { return Err(bad(CMD, "nothing to export")) };
    let meta = match p.get("meta") {
        Some(m) => serde_json::from_value(m.clone()).map_err(|e| bad(CMD, format!("meta: {e}")))?,
        None => crate::package::Meta::default(),
    };
    let name = p.get("name").and_then(Value::as_str).unwrap_or("Capishot profile").to_string();
    let out = crate::font_projects::export(&crate::font_projects::Pack { name: name.clone(), profile, meta, ..Default::default() })?;
    let file: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    Ok(json!({"data": photocraft_paint::tile::b64_encode(&out.bytes), "bytes": out.bytes.len(), "file": format!("{}{}", file.trim(), crate::font_projects::EXTENSION)}))
}

/// `profile.import`: applies the profile sections of a `.proj.capishot` or `.capiproj` (fonts
/// and font projects in it are left to the shell), backing up the current profile first.
fn import_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "profile.import";
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `data` (the file as base64)"))?;
    let bytes = photocraft_paint::tile::b64_decode(data).ok_or_else(|| bad(CMD, "`data` is not base64"))?;
    let imp = crate::font_projects::import(&bytes)?;
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("profile".into(), Value::Object(imp.profile));
        o.remove("data");
    }
    apply_cmd(s, &q)
}

fn has_backup(s: &Session) -> std::result::Result<(), String> {
    if s.prefs().profile_backup.is_object() { Ok(()) } else { Err("no profile import to undo".into()) }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "profile.capture",
            label: "Capture Profile",
            menu: &[],
            shortcut: None,
            params: r##"{"sections":[str]?=all (theme|shortcuts|workspace|preferences|fonts|fontProjects|presets|brushes|typer)} → {section: {path: value}}"##,
            enabled: always,
            run: capture_cmd,
            journal: false,
        },
        CommandSpec {
            id: "profile.apply",
            label: "Apply Profile",
            menu: &[],
            shortcut: None,
            params: r##"{"profile":{section: data},"sections":[str]?=all in profile,"merge":bool?=false,"extra":{section: data}? (kept in the backup)} → {applied, ignored}; backs up the current profile first"##,
            enabled: always,
            run: apply_cmd,
            journal: false,
        },
        CommandSpec {
            id: "profile.export",
            label: "Export Profile",
            menu: &[],
            shortcut: None,
            params: r##"{"sections":[str]?=all,"name":str?,"meta":{title,description,author,version,license,tags,homepage,minCapishotVersion}?} → {data: .proj.capishot as base64, bytes, file} (no font files)"##,
            enabled: always,
            run: export_cmd,
            journal: false,
        },
        CommandSpec {
            id: "profile.import",
            label: "Import Profile",
            menu: &[],
            shortcut: None,
            params: r##"{"data":base64 .proj.capishot|.capiproj,"sections":[str]?=all in it,"merge":bool?=false} → {applied, ignored}; backs up the current profile first (profile.undoImport)"##,
            enabled: always,
            run: import_cmd,
            journal: false,
        },
        CommandSpec {
            id: "profile.undoImport",
            label: "Undo Last Profile Import",
            menu: &[],
            shortcut: None,
            params: r##"{} → {restored, extra}"##,
            enabled: has_backup,
            run: undo_cmd,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
