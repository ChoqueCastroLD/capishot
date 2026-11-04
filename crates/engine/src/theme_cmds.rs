//! The user's colour themes (Preferences › Profile & Themes) and `.theme.capishot` files.
//!
//! A theme is a `theme.css` stylesheet: CSS custom properties (`--accent: #378ef0;`) the shell
//! reads into its design tokens, the same way it reads its built-in themes, so a custom theme is
//! never a second kind of theme. The engine keeps the stylesheets in the preferences
//! (`interface.customThemes`, the one in use in `interface.customTheme`) without reading them;
//! it only caps their size. A `.theme.capishot` ([`export`], [`import`]) is a [`package`] with
//! `manifest.json`, `theme.css` and optionally `icon.png` (at most 256×256) and `preview.png`.

use serde_json::{Map, Value, json};

use crate::commands::CommandSpec;
use crate::package::{self, Limits, Meta, Package};
use crate::prefs::CustomTheme;
use crate::{EngineError, Result, Session};

/// The `format` of a `.theme.capishot` manifest, and its version.
pub const FORMAT: &str = "capishot.theme";
pub const FORMAT_VERSION: u32 = 1;
/// The file name ending of a theme package.
pub const EXTENSION: &str = ".theme.capishot";
/// The largest `theme.css` (bytes).
pub const MAX_CSS: usize = 64 << 10;
/// The most themes kept.
pub const MAX_THEMES: usize = 200;
const MAX_PREVIEW_SIDE: u32 = 2048;
const LIMITS: Limits = Limits { total: 8 << 20, files: 8, file: 4 << 20 };

fn allowed(name: &str) -> bool {
    matches!(name, "theme.css" | "icon.png" | "preview.png")
}

/// A `.theme.capishot` read.
#[derive(Debug, Default)]
pub struct ThemePackage {
    pub name: String,
    pub css: String,
    pub meta: Meta,
    pub icon: Option<Vec<u8>>,
    pub preview: Option<Vec<u8>>,
    /// What was left out or tidied (unknown files, dropped metadata).
    pub warnings: Vec<String>,
}

/// Is `css` small enough, text and free of NULs?
pub fn check_css(css: &str) -> std::result::Result<(), String> {
    if css.len() > MAX_CSS {
        return Err(format!("theme.css is larger than {} KB", MAX_CSS >> 10));
    }
    if css.contains('\0') {
        return Err("theme.css has a NUL character".into());
    }
    Ok(())
}

/// Reads a `.theme.capishot`.
pub fn import(bytes: &[u8]) -> Result<ThemePackage> {
    let pkg = Package::open(bytes, "Capishot theme", &[(FORMAT, FORMAT_VERSION)], allowed, LIMITS)?;
    let css = pkg.read_text("theme.css")?.ok_or_else(|| EngineError::Other("not a valid Capishot theme: no theme.css".into()))?;
    check_css(&css).map_err(EngineError::Other)?;
    let icon = pkg.read_png("icon.png", package::ICON_SIDE)?;
    let preview = pkg.read_png("preview.png", MAX_PREVIEW_SIDE)?;
    let mut warnings = pkg.warnings.clone();
    warnings.extend(pkg.ignored.iter().map(|f| format!("{f} ignored: not part of a theme")));
    let mut meta = pkg.meta.clone();
    if icon.is_none() {
        meta.icon.clear();
    }
    let name = pkg.manifest.get("name").and_then(Value::as_str).map(|n| n.trim().chars().filter(|c| !c.is_control()).take(80).collect::<String>());
    let name = name.filter(|n| !n.is_empty()).or_else(|| (!meta.title.is_empty()).then(|| meta.title.clone())).unwrap_or_else(|| "Theme".into());
    Ok(ThemePackage { name, css, meta, icon, preview, warnings })
}

/// Packs a theme as a `.theme.capishot` (the icon and preview must be PNGs within their limits).
pub fn export(theme: &ThemePackage) -> Result<Vec<u8>> {
    check_css(&theme.css).map_err(EngineError::Other)?;
    let mut meta = theme.meta.clone();
    meta.icon = if theme.icon.is_some() { "icon.png".into() } else { String::new() };
    let mut w = package::Writer::new(FORMAT, FORMAT_VERSION, &meta, Map::from_iter([("name".to_string(), json!(theme.name))]));
    w.add("theme.css", theme.css.as_bytes())?;
    for (name, bytes, max) in [("icon.png", &theme.icon, package::ICON_SIDE), ("preview.png", &theme.preview, MAX_PREVIEW_SIDE)] {
        if let Some(b) = bytes {
            package::check_png(b, max).map_err(|e| EngineError::Other(format!("{name}: {e}")))?;
            w.add(name, b)?;
        }
    }
    w.finish()
}

// ------------------------------------------------------------------ commands

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn themes(s: &Session) -> &[CustomTheme] {
    &s.prefs().interface.custom_themes
}

fn clean_name(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control()).take(80).collect()
}

/// An id for `name` no theme has.
fn fresh_id(all: &[CustomTheme], name: &str) -> String {
    let base: String = name.chars().filter(char::is_ascii_alphanumeric).take(24).collect::<String>().to_lowercase();
    let base = if base.is_empty() { "theme".to_string() } else { base };
    (1..=all.len() + 1).map(|n| if n == 1 { base.clone() } else { format!("{base}{n}") }).find(|id| !all.iter().any(|t| &t.id == id)).unwrap_or(base)
}

/// `name`, or `name 2`, `name 3`… when another theme has it.
fn fresh_name(all: &[CustomTheme], name: &str, except: &str) -> String {
    let taken = |n: &str| all.iter().any(|t| t.id != except && t.name.eq_ignore_ascii_case(n));
    (1..=all.len() + 1).map(|i| if i == 1 { name.to_string() } else { format!("{name} {i}") }).find(|n| !taken(n)).unwrap_or_else(|| name.to_string())
}

fn icon_param(cmd: &str, p: &Value) -> Result<Option<String>> {
    match p.get("icon") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(b)) if b.is_empty() => Ok(Some(String::new())),
        Some(Value::String(b)) => {
            let bytes = photocraft_paint::tile::b64_decode(b).ok_or_else(|| bad(cmd, "`icon` is not base64"))?;
            package::check_png(&bytes, package::ICON_SIDE).map_err(|e| bad(cmd, format!("icon: {e}")))?;
            Ok(Some(b.clone()))
        }
        Some(_) => Err(bad(cmd, "`icon` is a base64 PNG")),
    }
}

fn meta_param(cmd: &str, p: &Value) -> Result<Option<Meta>> {
    match p.get("meta") {
        None | Some(Value::Null) => Ok(None),
        Some(m) => {
            let mut meta: Meta = serde_json::from_value(m.clone()).map_err(|e| bad(cmd, format!("meta: {e}")))?;
            meta.sanitize();
            Ok(Some(meta))
        }
    }
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let i = &s.prefs().interface;
    let list: Vec<Value> = i.custom_themes.iter().map(|t| json!({"id": t.id, "name": t.name, "meta": t.meta, "hasIcon": !t.icon.is_empty()})).collect();
    Ok(json!({"themes": list, "active": i.active_custom_theme().map(|t| t.id.clone())}))
}

fn get(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.custom.get";
    let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `id`"))?;
    let t = themes(s).iter().find(|t| t.id == id).ok_or_else(|| bad(CMD, format!("no theme `{id}`")))?;
    serde_json::to_value(t).map_err(|e| EngineError::Other(e.to_string()))
}

/// Adds `theme` (a fresh id, and a name no other theme has) and returns its id.
fn add(s: &mut Session, cmd: &str, mut theme: CustomTheme, activate: bool) -> Result<String> {
    if themes(s).len() >= MAX_THEMES {
        return Err(bad(cmd, format!("at most {MAX_THEMES} themes; delete some first")));
    }
    theme.id = fresh_id(themes(s), &theme.name);
    theme.name = fresh_name(themes(s), &theme.name, "");
    let id = theme.id.clone();
    s.edit_prefs(|pr| {
        pr.interface.custom_themes.push(theme);
        if activate {
            pr.interface.custom_theme = id.clone();
        }
    });
    Ok(id)
}

/// `theme.custom.save`: a new theme, or (with `id`) new contents for one.
fn save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.custom.save";
    let css = p.get("css").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `css` (the theme.css text)"))?;
    check_css(css).map_err(|e| bad(CMD, e))?;
    let name = p.get("name").and_then(Value::as_str).map(clean_name);
    let (meta, icon) = (meta_param(CMD, p)?, icon_param(CMD, p)?);
    let activate = p.get("use").and_then(Value::as_bool).unwrap_or(true);
    let id = match p.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        Some(id) => {
            let i = themes(s).iter().position(|t| t.id == id).ok_or_else(|| bad(CMD, format!("no theme `{id}`")))?;
            let name = name.filter(|n| !n.is_empty()).map(|n| fresh_name(themes(s), &n, id));
            s.edit_prefs(|pr| {
                if let Some(t) = pr.interface.custom_themes.get_mut(i) {
                    t.css = css.to_string();
                    if let Some(n) = name {
                        t.name = n;
                    }
                    if let Some(m) = meta {
                        t.meta = m;
                    }
                    if let Some(icon) = icon {
                        t.icon = icon;
                    }
                }
                if activate {
                    pr.interface.custom_theme = id.to_string();
                }
            });
            id.to_string()
        }
        None => {
            let name = name.filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "a new theme needs a `name`"))?;
            let theme = CustomTheme { id: String::new(), name, css: css.to_string(), meta: meta.unwrap_or_default(), icon: icon.unwrap_or_default() };
            add(s, CMD, theme, activate)?
        }
    };
    Ok(json!({"id": id}))
}

fn duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.custom.duplicate";
    let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `id`"))?;
    let mut copy = themes(s).iter().find(|t| t.id == id).cloned().ok_or_else(|| bad(CMD, format!("no theme `{id}`")))?;
    if let Some(n) = p.get("name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty()) {
        copy.name = n;
    }
    let activate = p.get("use").and_then(Value::as_bool).unwrap_or(false);
    Ok(json!({"id": add(s, CMD, copy, activate)?}))
}

fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.custom.rename";
    let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `id`"))?;
    let name = p.get("name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "missing `name`"))?;
    let i = themes(s).iter().position(|t| t.id == id).ok_or_else(|| bad(CMD, format!("no theme `{id}`")))?;
    let name = fresh_name(themes(s), &name, id);
    s.edit_prefs(|pr| {
        if let Some(t) = pr.interface.custom_themes.get_mut(i) {
            t.name.clone_from(&name);
        }
    });
    Ok(json!({"id": id, "name": name}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.custom.delete";
    let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `id`"))?;
    if !themes(s).iter().any(|t| t.id == id) {
        return Err(bad(CMD, format!("no theme `{id}`")));
    }
    s.edit_prefs(|pr| {
        pr.interface.custom_themes.retain(|t| t.id != id);
        if pr.interface.custom_theme == id {
            pr.interface.custom_theme.clear();
        }
    });
    Ok(json!({"deleted": id}))
}

/// `theme.custom.use`: a custom theme by id, or `""` for the built-in the appearance mode picks.
fn use_theme(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.custom.use";
    let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `id` (\"\" for the built-in theme)"))?;
    if !id.is_empty() && !themes(s).iter().any(|t| t.id == id) {
        return Err(bad(CMD, format!("no theme `{id}`")));
    }
    s.edit_prefs(|pr| pr.interface.custom_theme = id.to_string());
    Ok(json!({"active": id}))
}

fn package_export(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.package.export";
    let id = p.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `id`"))?;
    let t = themes(s).iter().find(|t| t.id == id).cloned().ok_or_else(|| bad(CMD, format!("no theme `{id}`")))?;
    let icon = if t.icon.is_empty() { None } else { photocraft_paint::tile::b64_decode(&t.icon) };
    let preview = match p.get("preview").and_then(Value::as_str) {
        Some(b) => Some(photocraft_paint::tile::b64_decode(b).ok_or_else(|| bad(CMD, "`preview` is not base64"))?),
        None => None,
    };
    let mut meta = t.meta.clone();
    if meta.title.is_empty() {
        meta.title.clone_from(&t.name);
    }
    let bytes = export(&ThemePackage { name: t.name.clone(), css: t.css, meta, icon, preview, warnings: Vec::new() })?;
    let file: String = t.name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect::<String>().trim().to_string();
    Ok(json!({"data": photocraft_paint::tile::b64_encode(&bytes), "bytes": bytes.len(), "file": format!("{}{EXTENSION}", if file.is_empty() { "theme" } else { &file })}))
}

/// `theme.package.import`: adds the theme a `.theme.capishot` holds (and uses it unless `use` is
/// false). The shell reads its stylesheet and reports the properties it didn't know.
fn package_import(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "theme.package.import";
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `data` (the file as base64)"))?;
    if data.len() > LIMITS.total / 3 * 4 + 4 {
        return Err(bad(CMD, "the file is too large for a theme"));
    }
    let bytes = photocraft_paint::tile::b64_decode(data).ok_or_else(|| bad(CMD, "`data` is not base64"))?;
    let pkg = import(&bytes)?;
    let activate = p.get("use").and_then(Value::as_bool).unwrap_or(true);
    let icon = pkg.icon.as_deref().map(photocraft_paint::tile::b64_encode).unwrap_or_default();
    let theme = CustomTheme { id: String::new(), name: pkg.name, css: pkg.css, meta: pkg.meta, icon };
    let id = add(s, CMD, theme, activate)?;
    Ok(json!({"id": id, "warnings": pkg.warnings, "preview": pkg.preview.as_deref().map(photocraft_paint::tile::b64_encode)}))
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false };
    vec![
        spec("theme.custom.list", "List Custom Themes", r##"{} → {themes:[{id,name,meta,hasIcon}], active:id|null}"##, list),
        spec("theme.custom.get", "Get Custom Theme", r##"{"id":str} → {id,name,css,meta,icon}"##, get),
        spec(
            "theme.custom.save",
            "Save Custom Theme",
            r##"{"css":theme.css text (CSS custom properties, at most 64 KB),"id":str? (update that theme; else a new one),"name":str? (needed for a new theme),"meta":{title,description,author,version,license,tags,homepage,minCapishotVersion}?,"icon":base64 PNG ≤256×256|""?,"use":bool=true} → {id}"##,
            save,
        ),
        spec("theme.custom.duplicate", "Duplicate Custom Theme", r##"{"id":str,"name":str?,"use":bool=false} → {id}"##, duplicate),
        spec("theme.custom.rename", "Rename Custom Theme", r##"{"id":str,"name":str} → {id,name}"##, rename),
        spec("theme.custom.delete", "Delete Custom Theme", r##"{"id":str} → {deleted}"##, delete),
        spec("theme.custom.use", "Use Custom Theme", r##"{"id":str ("" = the built-in theme the appearance mode picks)} → {active}"##, use_theme),
        spec("theme.package.export", "Export Theme", r##"{"id":str,"preview":base64 PNG? (≤2048 px)} → {data: the .theme.capishot as base64, bytes, file}"##, package_export),
        spec("theme.package.import", "Import Theme", r##"{"data":base64 .theme.capishot,"use":bool=true} → {id, warnings, preview?}"##, package_import),
    ]
}

#[cfg(test)]
#[path = "theme_cmds_tests.rs"]
mod tests;
