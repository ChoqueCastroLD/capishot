//! The font menu's favourite font families: the star beside each font, kept until un-starred. They
//! live in the preferences, so they survive a restart, and are plain family names: a favourite
//! whose font is uninstalled is kept, though the menu lists installed fonts only, and shows it
//! again once the font is back (`type.font.favorite` with `"on": false` removes it). (The
//! recently used fonts are the shell's, `PhotocraftApp::note_recent_font`.)

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The most favourites kept (a hand-edited preferences file can't grow the menu without bound).
pub const MAX_FAVORITE_FONTS: usize = 1000;
/// The longest family name accepted, in characters.
const MAX_FAMILY_CHARS: usize = 256;

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// The `family` param: a non-empty name without control characters, trimmed.
fn family(p: &Value, cmd: &str) -> Result<String> {
    let name = p.get("family").and_then(Value::as_str).ok_or_else(|| bad(cmd, "pass `family`, a font family name"))?.trim();
    if name.is_empty() {
        return Err(bad(cmd, "`family` is empty"));
    }
    if name.chars().count() > MAX_FAMILY_CHARS {
        return Err(bad(cmd, format!("`family` is longer than {MAX_FAMILY_CHARS} characters")));
    }
    if name.chars().any(char::is_control) {
        return Err(bad(cmd, "`family` contains control characters"));
    }
    Ok(name.to_string())
}

/// The favourites as the menu shows them: valid names, once each, at most [`MAX_FAVORITE_FONTS`].
pub fn favorite_fonts(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in &s.prefs().type_.favorite_fonts {
        if out.len() >= MAX_FAVORITE_FONTS {
            break;
        }
        if !f.trim().is_empty() && !out.contains(f) {
            out.push(f.clone());
        }
    }
    out
}

/// Star or un-star a family (`on` sets it; without it the star toggles).
fn favorite(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "type.font.favorite";
    let name = family(p, CMD)?;
    let mut list = favorite_fonts(s);
    let on = match p.get("on") {
        None => !list.contains(&name),
        Some(v) => v.as_bool().ok_or_else(|| bad(CMD, "`on` must be true or false"))?,
    };
    list.retain(|f| *f != name);
    if on {
        if list.len() >= MAX_FAVORITE_FONTS {
            return Err(bad(CMD, format!("at most {MAX_FAVORITE_FONTS} favourite fonts")));
        }
        list.push(name.clone());
    }
    let stored = list.clone();
    s.prefs.edit(|p| p.type_.favorite_fonts = stored);
    Ok(json!({"family": name, "favorite": on, "favorites": list}))
}

/// At most this many folders, and this long a folder name.
const MAX_FOLDERS: usize = 200;
const MAX_FOLDER_CHARS: usize = 80;

fn folder_name(p: &Value, key: &str, cmd: &str) -> Result<String> {
    let name = p.get(key).and_then(Value::as_str).map(str::trim).ok_or_else(|| bad(cmd, format!("`{key}` must be a folder name")))?;
    if name.is_empty() || name.chars().count() > MAX_FOLDER_CHARS || name.chars().any(char::is_control) {
        return Err(bad(cmd, format!("`{key}` must be 1 to {MAX_FOLDER_CHARS} printable characters")));
    }
    Ok(name.to_string())
}

fn folders_json(s: &Session) -> Value {
    json!({"folders": s.prefs().type_.font_folders})
}

/// `type.fontFolder.create`: a new folder, at the top or in a top-level folder.
fn folder_create(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "type.fontFolder.create";
    let name = folder_name(p, "name", CMD)?;
    let parent = match p.get("parent") {
        None | Some(Value::Null) => None,
        Some(_) => Some(folder_name(p, "parent", CMD)?),
    };
    let folders = &s.prefs().type_.font_folders;
    if folders.iter().any(|f| f.name == name) {
        return Err(bad(CMD, format!("there is already a folder {name:?}")));
    }
    if folders.len() >= MAX_FOLDERS {
        return Err(bad(CMD, format!("at most {MAX_FOLDERS} folders")));
    }
    if let Some(parent) = &parent {
        match folders.iter().find(|f| f.name == *parent) {
            None => return Err(bad(CMD, format!("no folder {parent:?}"))),
            Some(f) if f.parent.is_some() => return Err(bad(CMD, "folders nest one level deep")),
            Some(_) => {}
        }
    }
    s.prefs.edit(|pr| pr.type_.font_folders.push(crate::prefs::FontFolder { name, parent, fonts: Vec::new() }));
    Ok(folders_json(s))
}

/// `type.fontFolder.rename`.
fn folder_rename(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "type.fontFolder.rename";
    let name = folder_name(p, "name", CMD)?;
    let to = folder_name(p, "to", CMD)?;
    let folders = &s.prefs().type_.font_folders;
    if !folders.iter().any(|f| f.name == name) {
        return Err(bad(CMD, format!("no folder {name:?}")));
    }
    if name != to && folders.iter().any(|f| f.name == to) {
        return Err(bad(CMD, format!("there is already a folder {to:?}")));
    }
    s.prefs.edit(|pr| {
        for f in &mut pr.type_.font_folders {
            if f.name == name {
                f.name.clone_from(&to);
            }
            if f.parent.as_deref() == Some(name.as_str()) {
                f.parent = Some(to.clone());
            }
        }
    });
    Ok(folders_json(s))
}

/// `type.fontFolder.delete`: the folder goes; its fonts stay (unfiled) and its folders move up.
fn folder_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "type.fontFolder.delete";
    let name = folder_name(p, "name", CMD)?;
    if !s.prefs().type_.font_folders.iter().any(|f| f.name == name) {
        return Err(bad(CMD, format!("no folder {name:?}")));
    }
    s.prefs.edit(|pr| {
        pr.type_.font_folders.retain(|f| f.name != name);
        for f in &mut pr.type_.font_folders {
            if f.parent.as_deref() == Some(name.as_str()) {
                f.parent = None;
            }
        }
    });
    Ok(folders_json(s))
}

/// `type.fontFolder.move`: files `family` in `folder` (`null`: unfiled), out of any other.
fn folder_move(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "type.fontFolder.move";
    let fam = family(p, CMD)?;
    let to = match p.get("folder") {
        None | Some(Value::Null) => None,
        Some(_) => Some(folder_name(p, "folder", CMD)?),
    };
    if let Some(to) = &to
        && !s.prefs().type_.font_folders.iter().any(|f| f.name == *to)
    {
        return Err(bad(CMD, format!("no folder {to:?}")));
    }
    s.prefs.edit(|pr| {
        for f in &mut pr.type_.font_folders {
            f.fonts.retain(|x| *x != fam);
            if to.as_deref() == Some(f.name.as_str()) {
                f.fonts.push(fam.clone());
            }
        }
    });
    Ok(folders_json(s))
}

fn library(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"favorites": favorite_fonts(s), "folders": s.prefs().type_.font_folders}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "type.font.favorite",
            label: "Add to Favorites",
            menu: &[],
            shortcut: None,
            params: r#"{"family":str,"on":bool?=toggle} → family, favorite, favorites"#,
            enabled: always,
            run: favorite,
            journal: false,
        },
        CommandSpec {
            id: "type.font.library",
            label: "Font Library",
            menu: &[],
            shortcut: None,
            params: "{} → favorites, folders",
            enabled: always,
            run: library,
            journal: false,
        },
        CommandSpec {
            id: "type.fontFolder.create",
            label: "New Font Folder",
            menu: &[],
            shortcut: None,
            params: r#"{"name":str,"parent":str? (a top-level folder: folders nest one level)} → folders"#,
            enabled: always,
            run: folder_create,
            journal: false,
        },
        CommandSpec {
            id: "type.fontFolder.rename",
            label: "Rename Font Folder",
            menu: &[],
            shortcut: None,
            params: r#"{"name":str,"to":str} → folders"#,
            enabled: always,
            run: folder_rename,
            journal: false,
        },
        CommandSpec {
            id: "type.fontFolder.delete",
            label: "Delete Font Folder",
            menu: &[],
            shortcut: None,
            params: r#"{"name":str} (its fonts stay, unfiled; its folders move up) → folders"#,
            enabled: always,
            run: folder_delete,
            journal: false,
        },
        CommandSpec {
            id: "type.fontFolder.move",
            label: "Move Font to Folder",
            menu: &[],
            shortcut: None,
            params: r#"{"family":str,"folder":str|null (null: unfiled)} → folders"#,
            enabled: always,
            run: folder_move,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests;
