//! Font projects ("Proyectos de fuentes"): a series' fonts, each with a role (Dialogue, Shout,
//! Thought, Narration, SFX, Notes, or the project's own), each role's type settings (size,
//! leading, tracking, caps, colour, stroke, alignment), which role Bubble Type's styles and
//! TypeR's line tags use, and the file names that open with the project.
//!
//! Projects are kept in the preferences (`fontProjects`: no font files there; those are the
//! user's fonts). A page remembers its project in an image resource of its PSD
//! ([`RESOURCE_ID`], ignored by Photoshop), and a file name like a remembered one
//! ([`page_prefix`]) opens with its project.
//!
//! A `.capiproj` ([`export`], [`import`]) is a project to share: a zip with `manifest.json` and
//! the font files the project uses under `fonts/` (with their SHA-256), and optionally TypeR's
//! style library and Bubble Type's styles. Never pages or images. Fonts whose licence forbids
//! redistribution (OS/2 `fsType` bit 1, [`restricted`]) are left out unless asked, and only
//! named; the team installs them themselves.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The PSD image resource holding a page's project (in Photoshop's plug-in range, 4000-4999).
pub const RESOURCE_ID: u16 = 4242;
/// Its name.
pub const RESOURCE_NAME: &str = "Capishot font project";
/// The `.capiproj` format version.
pub const FORMAT_VERSION: u32 = 1;
/// A `.capiproj` larger than this gets a warning on export (bytes).
pub const LARGE_EXPORT: u64 = 50 << 20;
/// The largest `.capiproj` read (bytes), and the largest font file in one.
const MAX_IMPORT: usize = 512 << 20;
const MAX_FONT: usize = 64 << 20;

/// The built-in roles: id and English name (the UI translates it).
pub const BUILTIN_ROLES: [(&str, &str); 6] =
    [("dialogue", "Dialogue"), ("shout", "Shout"), ("thought", "Thought"), ("narration", "Narration"), ("sfx", "SFX"), ("notes", "Notes")];

/// A role's stroke: width (px), colour and position, as the Stroke layer effect.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoleStroke {
    pub width: f32,
    pub color: String,
    /// "outside", "center" or "inside".
    pub position: String,
}

/// What a role sets on its text; `None` leaves that alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoleSettings {
    pub size_pt: Option<f32>,
    /// The leading in points; `Some(0)` or less: Auto.
    pub leading_pt: Option<f32>,
    /// 1/1000 em.
    pub tracking: Option<f32>,
    /// "normal", "small" or "all" (all caps).
    pub caps: Option<String>,
    /// `#rrggbb`.
    pub color: Option<String>,
    pub stroke: Option<RoleStroke>,
    /// "left", "center", "right" or "justify".
    pub align: Option<String>,
}

impl RoleSettings {
    /// The `type.setStyle` keys these settings set.
    pub fn style_params(&self) -> serde_json::Map<String, Value> {
        let mut p = serde_json::Map::new();
        if let Some(v) = self.size_pt.filter(|v| v.is_finite() && *v > 0.0) {
            p.insert("size".into(), json!(v.clamp(0.1, 1296.0)));
        }
        if let Some(v) = self.leading_pt.filter(|v| v.is_finite()) {
            p.insert("leading".into(), if v <= 0.0 { json!("auto") } else { json!(v.min(5000.0)) });
        }
        if let Some(v) = self.tracking.filter(|v| v.is_finite()) {
            p.insert("tracking".into(), json!(v.clamp(-1000.0, 10000.0)));
        }
        if let Some(c) = self.caps.as_deref().filter(|c| matches!(*c, "normal" | "small" | "all")) {
            p.insert("caps".into(), json!(c));
        }
        if let Some(c) = self.color.as_deref().filter(|c| is_hex(c)) {
            p.insert("color".into(), json!(c));
        }
        if let Some(a) = self.align.as_deref().filter(|a| matches!(*a, "left" | "center" | "right" | "justify")) {
            p.insert("align".into(), json!(a));
        }
        p
    }

    /// The `layer.layerStyle.stroke` params of its stroke (`None`: none set; a width of 0 means
    /// no stroke).
    pub fn stroke_params(&self) -> Option<Value> {
        let s = self.stroke.as_ref()?;
        let position = match s.position.as_str() {
            "center" | "inside" => s.position.as_str(),
            _ => "outside",
        };
        Some(json!({"size": s.width.clamp(0.0, 250.0), "color": if is_hex(&s.color) { s.color.as_str() } else { "#ffffff" }, "position": position}))
    }
}

fn is_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Role {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub settings: RoleSettings,
}

/// A font of the project and its role.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectFont {
    pub family: String,
    pub role: String,
    /// The font file's SHA-256 when it came with an imported project (or was exported).
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Grouping in the project list (a scan, say).
    pub folder: Option<String>,
    pub roles: Vec<Role>,
    pub fonts: Vec<ProjectFont>,
    /// Bubble Type's styles ("bubble", "narration", "floating") → role id.
    pub bubble: BTreeMap<String, String>,
    /// TypeR line tags (`GRITO` for `GRITO: …`) → role id.
    pub tags: BTreeMap<String, String>,
    /// File-name prefixes (before the page number) and folders whose pages open with it.
    pub patterns: Vec<String>,
}

impl Project {
    /// A new project with the built-in roles and the default mappings.
    pub fn new(id: &str, name: &str) -> Self {
        let roles = BUILTIN_ROLES.iter().map(|(id, name)| Role { id: (*id).into(), name: (*name).into(), builtin: true, settings: RoleSettings::default() }).collect();
        let bubble = [("bubble", "dialogue"), ("narration", "narration"), ("floating", "sfx")].iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        let tags = [
            ("GRITO", "shout"),
            ("SHOUT", "shout"),
            ("PENSAMIENTO", "thought"),
            ("THOUGHT", "thought"),
            ("NARRACION", "narration"),
            ("NARRACIÓN", "narration"),
            ("NARRATION", "narration"),
            ("SFX", "sfx"),
            ("NOTA", "notes"),
            ("NOTE", "notes"),
        ]
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
        Project { id: id.into(), name: name.into(), folder: None, roles, fonts: Vec::new(), bubble, tags, patterns: Vec::new() }
    }

    pub fn role(&self, id: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.id == id)
    }

    /// The first font of `role`.
    pub fn font_for(&self, role: &str) -> Option<&str> {
        self.fonts.iter().find(|f| f.role == role).map(|f| f.family.as_str())
    }

    /// The role of `family` in the project.
    pub fn role_of(&self, family: &str) -> Option<&str> {
        self.fonts.iter().find(|f| f.family == family).map(|f| f.role.as_str())
    }

    /// The `type.setStyle` keys of `role`: its font (the first one with that role) and settings.
    pub fn role_style(&self, role: &str) -> serde_json::Map<String, Value> {
        let mut p = self.role(role).map(|r| r.settings.style_params()).unwrap_or_default();
        if let Some(f) = self.font_for(role) {
            p.insert("font".into(), json!(f));
        }
        p
    }

    /// The role of a TypeR line's tag (`GRITO: ¡No!` → shout), case and accents as written.
    pub fn role_for_line(&self, line: &str) -> Option<&str> {
        let (tag, _) = line.trim_start().split_once(':')?;
        let tag = tag.trim();
        self.tags.iter().find(|(k, _)| k.eq_ignore_ascii_case(tag)).map(|(_, v)| v.as_str())
    }

    /// Whether a page named `file` (in folder `folder`) opens with this project.
    pub fn matches(&self, file: &str, folder: Option<&str>) -> bool {
        self.patterns.iter().any(|p| match p.strip_prefix("folder:") {
            Some(f) => folder.is_some_and(|d| d.eq_ignore_ascii_case(f)),
            None => page_prefix(file).is_some_and(|pre| pre.eq_ignore_ascii_case(p)),
        })
    }
}

/// Every project and the active one (`prefs.fontProjects`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Projects {
    pub projects: Vec<Project>,
    pub active: Option<String>,
    /// The font families picked lately, newest first (the font menu's Recent).
    pub recent: Vec<String>,
}

impl Projects {
    pub fn of(s: &Session) -> Self {
        serde_json::from_value(s.prefs().font_projects.clone()).unwrap_or_default()
    }

    fn save(self, s: &mut Session) {
        let v = serde_json::to_value(&self).unwrap_or(Value::Null);
        s.prefs.edit(|p| p.font_projects = v);
    }

    pub fn get(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.id == id)
    }

    pub fn active(&self) -> Option<&Project> {
        self.active.as_deref().and_then(|id| self.get(id))
    }

    /// The project a page named `file` (in `folder`) opens with.
    pub fn for_file(&self, file: &str, folder: Option<&str>) -> Option<&Project> {
        self.projects.iter().find(|p| p.matches(file, folder))
    }

    fn fresh_id(&self, name: &str) -> String {
        let base: String = name.chars().filter(|c| c.is_alphanumeric()).take(16).collect::<String>().to_lowercase();
        let base = if base.is_empty() { "project".to_string() } else { base };
        (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}{n}") }).find(|id| self.get(id).is_none()).unwrap_or(base)
    }
}

/// How many fonts the font menu's Recent keeps.
pub const RECENT: usize = 8;

/// `family` was picked in a font menu: it leads the Recent list.
pub fn note_recent(s: &mut Session, family: &str) {
    let mut all = Projects::of(s);
    if all.recent.first().is_some_and(|f| f == family) {
        return;
    }
    all.recent.retain(|f| f != family);
    all.recent.insert(0, family.to_string());
    all.recent.truncate(RECENT);
    all.save(s);
}

/// The active project, if any.
pub fn active(s: &Session) -> Option<Project> {
    Projects::of(s).active().cloned()
}

/// The part of a page's file name before its page number ("OnePiece_c1050_p03.psd" →
/// "OnePiece_c1050_p"): pages of one chapter share it. `None` without a number, or with
/// nothing before it.
pub fn page_prefix(file: &str) -> Option<String> {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let chars: Vec<char> = stem.chars().collect();
    let end = chars.iter().rposition(char::is_ascii_digit)? + 1;
    let start = chars[..end].iter().rposition(|c| !c.is_ascii_digit()).map_or(0, |i| i + 1);
    let prefix: String = chars[..start].iter().collect();
    (!prefix.trim().is_empty()).then_some(prefix)
}

// ------------------------------------------------------------------ licensing

/// The OS/2 table's `fsType` (embedding permissions) of a TrueType/OpenType font (the first
/// face of a collection); `None` when there is no OS/2 table.
pub fn fs_type(font: &[u8]) -> Option<u16> {
    let be16 = |o: usize| font.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let be32 = |o: usize| font.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let base = if font.get(0..4) == Some(b"ttcf") { be32(12)? as usize } else { 0 };
    let tables = be16(base + 4)? as usize;
    for i in 0..tables.min(512) {
        let rec = base + 12 + i * 16;
        if font.get(rec..rec + 4) == Some(b"OS/2") {
            let off = be32(rec + 8)? as usize;
            return be16(off + 8);
        }
    }
    None
}

/// The font's licence forbids embedding it elsewhere (OS/2 `fsType` bit 1, "Restricted
/// License"): it is not put in a shared project.
pub fn restricted(font: &[u8]) -> bool {
    fs_type(font).is_some_and(|f| f & 0x0002 != 0)
}

// ------------------------------------------------------------------ .capiproj

/// A font file for [`export`].
#[derive(Clone, Debug)]
pub struct FontFile {
    /// The file's name.
    pub name: String,
    /// The families it provides.
    pub families: Vec<String>,
    pub bytes: Arc<Vec<u8>>,
}

/// What [`export`] made.
#[derive(Debug)]
pub struct Exported {
    pub bytes: Vec<u8>,
    /// Families left out because their licence forbids sharing them.
    pub restricted: Vec<String>,
    /// Families the project uses with no file here (system or bundled fonts, or missing): only named.
    pub referenced: Vec<String>,
    /// Over [`LARGE_EXPORT`].
    pub large: bool,
}

/// Packs `project` as a `.capiproj`: its manifest, the font `files` it uses (each once, by
/// SHA-256; restricted ones only with `include_restricted`), and `typer` (TypeR's style library)
/// and `bubble` (Bubble Type's styles) when given. `skip` lists families not to pack (the
/// export dialog's unticked ones).
pub fn export(project: &Project, files: &[FontFile], skip: &[String], include_restricted: bool, typer: Option<&Value>, bubble: Option<&Value>) -> Result<Exported> {
    let mut zip = photocraft_format::zip::ZipWriter::new();
    let mut fonts_json = Vec::new();
    let mut packed: BTreeMap<String, String> = BTreeMap::new();
    let (mut restricted_out, mut referenced) = (Vec::new(), Vec::new());
    let mut total = 0u64;
    for f in &project.fonts {
        let file = files.iter().find(|x| x.families.iter().any(|fam| fam == &f.family));
        let mut entry = json!({"family": f.family, "role": f.role});
        match file {
            Some(file) if skip.contains(&f.family) => {
                let _ = file;
                entry["referenced"] = json!("skipped");
                referenced.push(f.family.clone());
            }
            Some(file) if restricted(&file.bytes) && !include_restricted => {
                entry["referenced"] = json!("restricted");
                restricted_out.push(f.family.clone());
            }
            Some(file) => {
                let sha = photocraft_text::sha256::sha256_hex(&file.bytes);
                let path = match packed.get(&sha) {
                    Some(p) => p.clone(),
                    None => {
                        let p = format!("fonts/{}-{}", &sha[..12], safe_name(&file.name));
                        zip.add(&p, &file.bytes).map_err(|e| EngineError::Other(e.to_string()))?;
                        total += file.bytes.len() as u64;
                        packed.insert(sha.clone(), p.clone());
                        p
                    }
                };
                entry["file"] = json!(path);
                entry["sha256"] = json!(sha);
            }
            None => {
                entry["referenced"] = json!("notHere");
                referenced.push(f.family.clone());
            }
        }
        fonts_json.push(entry);
    }
    let mut p = project.clone();
    for f in &mut p.fonts {
        f.sha256 = fonts_json.iter().find(|e| e["family"] == f.family.as_str()).and_then(|e| e["sha256"].as_str()).map(str::to_string);
    }
    let manifest = json!({
        "format": "capiproj",
        "version": FORMAT_VERSION,
        "name": project.name,
        "project": p,
        "fonts": fonts_json,
        "typer": typer.is_some(),
        "bubbleStyles": bubble.is_some(),
    });
    let text = serde_json::to_vec_pretty(&manifest).map_err(|e| EngineError::Other(e.to_string()))?;
    zip.add("manifest.json", &text).map_err(|e| EngineError::Other(e.to_string()))?;
    if let Some(t) = typer {
        zip.add("typer.json", &serde_json::to_vec(t).map_err(|e| EngineError::Other(e.to_string()))?).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    if let Some(b) = bubble {
        zip.add("bubbleStyles.json", &serde_json::to_vec(b).map_err(|e| EngineError::Other(e.to_string()))?).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    let bytes = zip.finish().map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(Exported { large: total > LARGE_EXPORT, bytes, restricted: restricted_out, referenced })
}

fn safe_name(name: &str) -> String {
    let n: String = name.rsplit(['/', '\\']).next().unwrap_or(name).chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).take(80).collect();
    if n.is_empty() { "font".into() } else { n }
}

/// A `.capiproj` read.
#[derive(Debug)]
pub struct Imported {
    pub project: Project,
    /// Its font files (name, bytes), each checked against its SHA-256.
    pub fonts: Vec<(String, Vec<u8>)>,
    pub typer: Option<Value>,
    pub bubble: Option<Value>,
    /// Families the project uses but doesn't carry (to install separately).
    pub referenced: Vec<String>,
}

/// Reads a `.capiproj`. Only the manifest, font files under `fonts/` and the two style files are
/// read; anything else in the archive is ignored.
pub fn import(bytes: &[u8]) -> Result<Imported> {
    let bad = |m: &str| EngineError::Other(format!("not a Capishot font project: {m}"));
    if bytes.len() > MAX_IMPORT {
        return Err(bad("too large"));
    }
    let zip = photocraft_format::zip::ZipReader::new(bytes).map_err(|e| bad(&e.to_string()))?;
    let manifest: Value = serde_json::from_slice(&zip.read_by_name("manifest.json", 4 << 20).map_err(|_| bad("no manifest.json"))?).map_err(|e| bad(&e.to_string()))?;
    if manifest["format"] != "capiproj" {
        return Err(bad("unknown format"));
    }
    if manifest["version"].as_u64().unwrap_or(0) > u64::from(FORMAT_VERSION) {
        return Err(EngineError::Other("this font project was made by a newer Capishot; update to open it".into()));
    }
    let mut project: Project = serde_json::from_value(manifest["project"].clone()).map_err(|e| bad(&e.to_string()))?;
    if project.name.trim().is_empty() {
        project.name = manifest["name"].as_str().unwrap_or("Project").to_string();
    }
    let mut fonts = Vec::new();
    let mut referenced = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in manifest["fonts"].as_array().into_iter().flatten() {
        let family = f["family"].as_str().unwrap_or_default().to_string();
        let Some(path) = f["file"].as_str().filter(|p| p.starts_with("fonts/") && !p.contains("..")) else {
            referenced.push(family);
            continue;
        };
        if !seen.insert(path.to_string()) {
            continue;
        }
        let data = zip.read_by_name(path, MAX_FONT).map_err(|e| bad(&format!("{path}: {e}")))?;
        if let Some(sha) = f["sha256"].as_str()
            && photocraft_text::sha256::sha256_hex(&data) != sha
        {
            return Err(bad(&format!("{path} is damaged (its checksum doesn't match)")));
        }
        let name = path.trim_start_matches("fonts/").split_once('-').map_or(path, |(_, n)| n).to_string();
        fonts.push((name, data));
    }
    let read_json = |name: &str| zip.read_by_name(name, 16 << 20).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    Ok(Imported { project, fonts, typer: read_json("typer.json"), bubble: read_json("bubbleStyles.json"), referenced })
}

/// Adds (or replaces, by id) an imported project.
pub fn add_imported(s: &mut Session, project: Project, activate: bool) -> String {
    let mut all = Projects::of(s);
    let id = project.id.clone();
    match all.get_mut(&id) {
        Some(p) => *p = project,
        None => all.projects.push(project),
    }
    if activate {
        all.active = Some(id.clone());
    }
    all.save(s);
    id
}

// ------------------------------------------------------------------ the page's project

/// The project id a document carries (its PSD resource).
pub fn doc_project(doc: &photocraft_doc::Document) -> Option<String> {
    let (_, _, data) = doc.metadata.psd_resources.iter().find(|(id, _, _)| *id == RESOURCE_ID)?;
    let v: Value = serde_json::from_slice(data).ok()?;
    v["fontProject"].as_str().map(str::to_string)
}

/// Writes (or with `None` removes) the project id a document carries.
pub fn set_doc_project(doc: &mut photocraft_doc::Document, project: Option<&Project>) {
    doc.metadata.psd_resources.retain(|(id, _, _)| *id != RESOURCE_ID);
    if let Some(p) = project {
        let data = serde_json::to_vec(&json!({"fontProject": p.id, "name": p.name})).unwrap_or_default();
        doc.metadata.psd_resources.push((RESOURCE_ID, RESOURCE_NAME.into(), Arc::new(data)));
    }
}

/// A document just opened: the project it carries, else one whose patterns match its file name,
/// becomes the active one. Returns its id.
pub fn activate_for_open(s: &mut Session) -> Option<String> {
    let st = s.active()?;
    let path = st.path.clone().unwrap_or_else(|| st.doc.name.clone());
    let carried = doc_project(&st.doc);
    let mut all = Projects::of(s);
    let folder = std::path::Path::new(&path).parent().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string());
    let id = carried.filter(|id| all.get(id).is_some()).or_else(|| all.for_file(&path, folder.as_deref()).map(|p| p.id.clone()))?;
    if all.active.as_deref() != Some(&id) {
        all.active = Some(id.clone());
        all.save(s);
    }
    Some(id)
}

// ------------------------------------------------------------------ commands

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn str_param<'a>(p: &'a Value, key: &str, cmd: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or_else(|| bad(cmd, format!("need \"{key}\"")))
}

/// The project a command names (`"id"`), else the active one.
fn target<'a>(all: &'a mut Projects, p: &Value, cmd: &str) -> Result<&'a mut Project> {
    let id = p.get("id").and_then(Value::as_str).map(str::to_string).or_else(|| all.active.clone()).ok_or_else(|| bad(cmd, "no project (none is active)"))?;
    all.get_mut(&id).ok_or_else(|| bad(cmd, format!("no project {id:?}")))
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let all = Projects::of(s);
    Ok(json!({"projects": all.projects, "active": all.active}))
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.create";
    let name = str_param(p, "name", CMD)?.trim().chars().take(80).collect::<String>();
    let mut all = Projects::of(s);
    let id = all.fresh_id(&name);
    let mut project = Project::new(&id, &name);
    project.folder = p.get("folder").and_then(Value::as_str).map(str::to_string);
    all.projects.push(project);
    if p.get("activate").and_then(Value::as_bool).unwrap_or(true) {
        all.active = Some(id.clone());
    }
    all.save(s);
    Ok(json!({"id": id}))
}

fn edit(s: &mut Session, p: &Value, cmd: &str, f: impl FnOnce(&mut Project) -> Result<()>) -> Result<Value> {
    let mut all = Projects::of(s);
    let project = target(&mut all, p, cmd)?;
    f(project)?;
    let id = project.id.clone();
    all.save(s);
    Ok(json!({"id": id}))
}

fn duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.duplicate";
    let mut all = Projects::of(s);
    let src = target(&mut all, p, CMD)?.clone();
    let name = p.get("name").and_then(Value::as_str).map_or_else(|| format!("{} (2)", src.name), str::to_string);
    let id = all.fresh_id(&name);
    all.projects.push(Project { id: id.clone(), name, patterns: Vec::new(), ..src });
    all.save(s);
    Ok(json!({"id": id}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.delete";
    let id = str_param(p, "id", CMD)?.to_string();
    let mut all = Projects::of(s);
    let before = all.projects.len();
    all.projects.retain(|x| x.id != id);
    if all.projects.len() == before {
        return Err(bad(CMD, format!("no project {id:?}")));
    }
    if all.active.as_deref() == Some(&id) {
        all.active = None;
    }
    all.save(s);
    Ok(json!({"deleted": id}))
}

fn activate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.activate";
    let mut all = Projects::of(s);
    let id = match p.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().filter(|id| all.get(id).is_some()).ok_or_else(|| bad(CMD, format!("no project {v}")))?.to_string()),
    };
    all.active = id.clone();
    let project = id.as_deref().and_then(|i| all.get(i)).cloned();
    all.save(s);
    // The open page remembers it (in its PSD), without an undo step.
    if p.get("remember").and_then(Value::as_bool).unwrap_or(true)
        && let Some(st) = s.active_mut()
    {
        set_doc_project(std::sync::Arc::make_mut(&mut st.doc), project.as_ref());
    }
    Ok(json!({"active": id}))
}

fn add_font(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.addFont";
    let family = str_param(p, "family", CMD)?.to_string();
    let role = str_param(p, "role", CMD)?.to_string();
    edit(s, p, CMD, |pr| {
        if pr.role(&role).is_none() {
            return Err(bad(CMD, format!("no role {role:?}")));
        }
        match pr.fonts.iter_mut().find(|f| f.family == family) {
            Some(f) => f.role = role,
            None => pr.fonts.push(ProjectFont { family, role, sha256: None }),
        }
        Ok(())
    })
}

fn remove_font(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.removeFont";
    let family = str_param(p, "family", CMD)?.to_string();
    edit(s, p, CMD, |pr| {
        pr.fonts.retain(|f| f.family != family);
        Ok(())
    })
}

fn set_role(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.setRole";
    let role = str_param(p, "role", CMD)?.to_string();
    let settings: Option<RoleSettings> = match p.get("settings") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| bad(CMD, e.to_string()))?),
    };
    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    edit(s, p, CMD, |pr| {
        match pr.roles.iter_mut().find(|r| r.id == role) {
            Some(r) => {
                if let Some(n) = name {
                    r.name = n;
                }
                if let Some(st) = settings {
                    r.settings = st;
                }
            }
            None => pr.roles.push(Role { id: role.clone(), name: name.unwrap_or_else(|| role.clone()), builtin: false, settings: settings.unwrap_or_default() }),
        }
        Ok(())
    })
}

fn remove_role(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.removeRole";
    let role = str_param(p, "role", CMD)?.to_string();
    edit(s, p, CMD, |pr| {
        if pr.role(&role).is_some_and(|r| r.builtin) {
            return Err(bad(CMD, "a built-in role stays"));
        }
        pr.roles.retain(|r| r.id != role);
        pr.fonts.retain(|f| f.role != role);
        Ok(())
    })
}

fn set_mapping(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.setMapping";
    edit(s, p, CMD, |pr| {
        for (key, map) in [("bubble", &mut pr.bubble), ("tags", &mut pr.tags)] {
            if let Some(o) = p.get(key).and_then(Value::as_object) {
                for (k, v) in o {
                    match v.as_str() {
                        Some(r) => {
                            map.insert(k.clone(), r.to_string());
                        }
                        None => {
                            map.remove(k);
                        }
                    }
                }
            }
        }
        Ok(())
    })
}

fn set_props(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.set";
    edit(s, p, CMD, |pr| {
        if let Some(n) = p.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) {
            pr.name = n.trim().chars().take(80).collect();
        }
        if let Some(f) = p.get("folder") {
            pr.folder = f.as_str().filter(|f| !f.trim().is_empty()).map(str::to_string);
        }
        Ok(())
    })
}

fn remember_pattern(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.rememberPattern";
    let file = match p.get("file").and_then(Value::as_str) {
        Some(f) => f.to_string(),
        None => s.active().map(|st| st.path.clone().unwrap_or_else(|| st.doc.name.clone())).ok_or_else(|| bad(CMD, "need \"file\" (no document open)"))?,
    };
    let pattern = match p.get("folder").and_then(Value::as_str) {
        Some(f) => format!("folder:{f}"),
        None => page_prefix(&file).ok_or_else(|| bad(CMD, format!("{file:?} has no page number to tell its pages by")))?,
    };
    let pat = pattern.clone();
    edit(s, p, CMD, move |pr| {
        if !pr.patterns.contains(&pat) {
            pr.patterns.push(pat);
        }
        Ok(())
    })?;
    Ok(json!({"pattern": pattern}))
}

fn for_file(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.forFile";
    let file = str_param(p, "file", CMD)?;
    let all = Projects::of(s);
    Ok(json!({"id": all.for_file(file, p.get("folder").and_then(Value::as_str)).map(|p| p.id.clone())}))
}

/// Applies the active project's `role` to type layer `layer` (default: the active layer): its
/// font (with `"font": true`, the default) and settings, and its stroke, as one undo step.
fn apply_role(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "fontProjects.applyRole";
    let role = str_param(p, "role", CMD)?.to_string();
    let project = active(s).ok_or_else(|| bad(CMD, "no active project"))?;
    let r = project.role(&role).ok_or_else(|| bad(CMD, format!("no role {role:?}")))?;
    let mut style = r.settings.style_params();
    if p.get("font").and_then(Value::as_bool).unwrap_or(true)
        && let Some(f) = project.font_for(&role)
    {
        style.insert("font".into(), json!(f));
    }
    let layer = crate::commands::layer_param(s, p)?;
    let key = p.get("coalesce").and_then(Value::as_str).map_or_else(|| format!("font-role-{}-{}", layer.0, role), str::to_string);
    let mut out = json!({"layer": layer.0, "role": role});
    if !style.is_empty() {
        style.insert("layer".into(), json!(layer.0));
        style.insert("coalesce".into(), json!(key));
        s.execute("type.setStyle", Value::Object(style))?;
    }
    if let Some(mut st) = r.settings.stroke_params() {
        st["layer"] = json!(layer.0);
        st["coalesce"] = json!(key);
        if st["size"].as_f64().unwrap_or(0.0) > 0.0 {
            s.execute("layer.layerStyle.stroke", st)?;
        }
    }
    out["applied"] = json!(true);
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    fn always(_: &Session) -> std::result::Result<(), String> {
        Ok(())
    }
    fn has_doc(s: &Session) -> std::result::Result<(), String> {
        s.active().map(|_| ()).ok_or_else(|| "no document open".into())
    }
    macro_rules! spec {
        ($id:literal, $label:literal, $params:literal, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: $enabled, run: $run, journal: false }
        };
    }
    vec![
        spec!("fontProjects.list", "Font Projects", r#"{} -> {"projects":[…],"active":id|null}"#, always, list),
        spec!("fontProjects.create", "New Font Project", r#"{"name":str,"folder":str?,"activate":bool=true} -> {"id"}"#, always, create),
        spec!("fontProjects.set", "Rename Font Project", r#"{"id":id?,"name":str?,"folder":str|null?}"#, always, set_props),
        spec!("fontProjects.duplicate", "Duplicate Font Project", r#"{"id":id?,"name":str?} -> {"id"}"#, always, duplicate),
        spec!("fontProjects.delete", "Delete Font Project", r#"{"id":id}"#, always, delete),
        spec!(
            "fontProjects.activate",
            "Activate Font Project",
            r#"{"id":id|null,"remember":bool=true (the open page carries it in its PSD)}"#,
            always,
            activate
        ),
        spec!("fontProjects.addFont", "Add Font to Project", r#"{"id":id?,"family":str,"role":role id}"#, always, add_font),
        spec!("fontProjects.removeFont", "Remove Font from Project", r#"{"id":id?,"family":str}"#, always, remove_font),
        spec!(
            "fontProjects.setRole",
            "Set Font Project Role",
            r##"{"id":id?,"role":id (a new id adds a custom role),"name":str?,"settings":{"sizePt","leadingPt" (≤0: Auto),"tracking","caps":"normal|small|all","color":"#rrggbb","stroke":{"width","color","position"},"align"}?}"##,
            always,
            set_role
        ),
        spec!("fontProjects.removeRole", "Remove Font Project Role", r#"{"id":id?,"role":id}"#, always, remove_role),
        spec!(
            "fontProjects.setMapping",
            "Set Font Project Mappings",
            r#"{"id":id?,"bubble":{"bubble|narration|floating":role|null}?,"tags":{"TAG":role|null}?}"#,
            always,
            set_mapping
        ),
        spec!(
            "fontProjects.rememberPattern",
            "Remember Pages for Font Project",
            r#"{"id":id?,"file":str? (default the open page),"folder":str?} -> {"pattern"}"#,
            always,
            remember_pattern
        ),
        spec!("fontProjects.forFile", "Font Project for File", r#"{"file":str,"folder":str?} -> {"id"|null}"#, always, for_file),
        CommandSpec {
            id: "fontProjects.applyRole",
            label: "Apply Font Role",
            menu: &[],
            shortcut: None,
            params: r#"{"role":id,"layer":id?,"font":bool=true}"#,
            enabled: has_doc,
            run: apply_role,
            journal: true,
        },
    ]
}

#[cfg(test)]
#[path = "font_projects_tests.rs"]
mod tests;
