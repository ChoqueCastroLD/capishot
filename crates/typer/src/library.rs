// Ported from TypeR app_src/context.jsx (normalizeFolders, style/folder reducer cases),
// app_src/folderUtils.js and app_src/librarySerialization.js (develop 71bc061), MIT, (c) 2017
// Swirt. See crates/typer/LICENSE-TypeR.txt.

//! The style library: styles (L1, L4) in nested folders (L2, L3).
//!
//! Styles keep TypeR's JSON (field names and the `textProps` ActionDescriptor JSON) so a library
//! imports and re-exports unchanged; [`crate::psjson`] converts to Capishot's type model when a
//! style is applied. Only the keys TypeR's own serializer writes are kept (plus Capishot's
//! `capishot` side object), so foreign data never rides along.

use serde_json::{Map, Value};

use crate::id::{Id, num};

/// The style keys besides `id`, `name`, `folder`, `edited` and `prefixes` that a library keeps
/// (TypeR's `serializeStyle` whitelist, plus `capishot`).
pub const STYLE_EXTRA_KEYS: &[&str] = &[
    "textType",
    "textProps",
    "prefixColor",
    "prefixesDisabled",
    "stroke",
    "sizePresets",
    "autoSizeByPageWidth",
    "sizePresetDefaultIndex",
    "sizePresetMinWidths",
    "capishot",
];

/// Prefix chip colours offered by the style editor.
pub const PREFIX_COLORS: [&str; 8] = ["#FFA6A4", "#FDD4BB", "#FFF3B0", "#CDFDD3", "#BEDADC", "#C2F2FF", "#C3C9FF", "#F6D5FF"];
/// The prefix colour of a style that doesn't set one.
pub const DEFAULT_PREFIX_COLOR: &str = "#FFF3B0";

/// Per-style layer kind override (L6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextType {
    Inherit,
    Point,
    Paragraph,
}

/// A TypeR style.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub id: Id,
    pub name: String,
    /// `None` = Unsorted.
    pub folder: Option<Id>,
    /// Last edit, milliseconds since the Unix epoch (drives the import merge).
    pub edited: Option<serde_json::Number>,
    /// Line tags (`SFX:`) that select this style.
    pub prefixes: Vec<String>,
    /// The other whitelisted keys ([`STYLE_EXTRA_KEYS`]), verbatim.
    pub props: Map<String, Value>,
}

impl Style {
    /// A style from TypeR JSON; `None` without a valid id. Unknown keys are dropped.
    pub fn from_json(v: &Value) -> Option<Style> {
        let o = v.as_object()?;
        let id = Id::from_value(o.get("id")?)?;
        Some(Self::from_map_with_id(o, id))
    }

    /// A style from TypeR JSON with the given id (folder exports carry no ids).
    pub fn from_map_with_id(o: &Map<String, Value>, id: Id) -> Style {
        let name = o.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let folder = Id::opt_from_value(o.get("folder"));
        let edited = o.get("edited").and_then(Value::as_number).cloned();
        let prefixes = match o.get("prefixes") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            Some(Value::String(s)) => split_list(s),
            _ => Vec::new(),
        };
        let props = STYLE_EXTRA_KEYS.iter().filter_map(|k| o.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        Style { id, name, folder, edited, prefixes, props }
    }

    /// TypeR JSON for this style (`serializeStyle`).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), self.id.to_value());
        o.insert("name".into(), Value::String(self.name.clone()));
        o.insert("folder".into(), self.folder.as_ref().map_or(Value::Null, Id::to_value));
        if let Some(e) = &self.edited {
            o.insert("edited".into(), Value::Number(e.clone()));
        }
        o.insert("prefixes".into(), Value::Array(self.prefixes.iter().cloned().map(Value::String).collect()));
        for (k, v) in &self.props {
            o.insert(k.clone(), v.clone());
        }
        Value::Object(o)
    }

    /// A new style with TypeR's defaults (Tahoma Regular 14 px, centred, hyphenated, black).
    pub fn new(id: Id, name: &str) -> Style {
        let mut props = Map::new();
        props.insert("textProps".into(), default_text_props());
        props.insert("stroke".into(), default_stroke());
        props.insert("textType".into(), Value::String("inherit".into()));
        Style { id, name: name.to_string(), folder: None, edited: None, prefixes: Vec::new(), props }
    }

    pub fn text_props(&self) -> Option<&Map<String, Value>> {
        self.props.get("textProps")?.as_object()
    }

    /// `textProps.layerText.textStyleRange[0].textStyle`.
    pub fn text_style(&self) -> Option<&Map<String, Value>> {
        self.text_props()?.get("layerText")?.get("textStyleRange")?.get(0)?.get("textStyle")?.as_object()
    }

    pub(crate) fn text_style_mut(&mut self) -> Option<&mut Map<String, Value>> {
        self.props.get_mut("textProps")?.get_mut("layerText")?.get_mut("textStyleRange")?.get_mut(0)?.get_mut("textStyle")?.as_object_mut()
    }

    /// `textProps.layerText.paragraphStyleRange[0].paragraphStyle`.
    pub fn paragraph_style(&self) -> Option<&Map<String, Value>> {
        self.text_props()?.get("layerText")?.get("paragraphStyleRange")?.get(0)?.get("paragraphStyle")?.as_object()
    }

    /// The style's text size in its `typeUnit`; `None` = keep the layer's size.
    pub fn size(&self) -> Option<f64> {
        crate::presets::parse_size(self.text_style()?.get("size"))
    }

    /// `textProps.typeUnit` (`pixelsUnit`, `pointsUnit`, `millimetersUnit`).
    pub fn type_unit(&self) -> Option<&str> {
        self.text_props()?.get("typeUnit")?.as_str()
    }

    pub fn prefixes_disabled(&self) -> bool {
        self.props.get("prefixesDisabled").and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn prefix_color(&self) -> &str {
        self.props.get("prefixColor").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_PREFIX_COLOR)
    }

    pub fn text_type(&self) -> TextType {
        match self.props.get("textType").and_then(Value::as_str) {
            Some("point") => TextType::Point,
            Some("paragraph") => TextType::Paragraph,
            _ => TextType::Inherit,
        }
    }

    pub fn stroke(&self) -> Option<&Map<String, Value>> {
        self.props.get("stroke")?.as_object()
    }

    /// The style's folder as a group key (`None` = Unsorted).
    pub fn folder_key(&self) -> Option<&Id> {
        self.folder.as_ref()
    }
}

/// A folder of styles.
#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub id: Id,
    pub name: String,
    /// `None` = top level.
    pub parent_id: Option<Id>,
    /// Position among its siblings (0..n after [`Library::normalize`]).
    pub order: f64,
}

impl Folder {
    pub fn from_json(v: &Value) -> Option<Folder> {
        let o = v.as_object()?;
        let id = Id::from_value(o.get("id")?)?;
        Some(Folder {
            id,
            name: o.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
            parent_id: Id::opt_from_value(o.get("parentId")),
            order: o.get("order").and_then(Value::as_f64).filter(|f| f.is_finite()).unwrap_or(0.0),
        })
    }

    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), self.id.to_value());
        o.insert("name".into(), Value::String(self.name.clone()));
        o.insert("parentId".into(), self.parent_id.as_ref().map_or(Value::Null, Id::to_value));
        o.insert("order".into(), num(self.order));
        Value::Object(o)
    }
}

/// One row of the flattened folder tree (for pickers and the styles tree).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub id: Id,
    pub name: String,
    pub parent_id: Option<Id>,
    pub depth: usize,
    /// Breadcrumb, `Series / Chapter`.
    pub label: String,
}

/// Folders and styles. Styles are kept in TypeR's display order: Unsorted first, then each
/// folder's styles in tree order (this is also the order prefixes are matched in).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub folders: Vec<Folder>,
    pub styles: Vec<Style>,
}

/// Folder trees deeper than this are flattened at this depth (bounded recursion).
const MAX_FOLDER_DEPTH: usize = 64;

impl Library {
    pub fn style(&self, id: &Id) -> Option<&Style> {
        self.styles.iter().find(|s| &s.id == id)
    }

    pub fn style_mut(&mut self, id: &Id) -> Option<&mut Style> {
        self.styles.iter_mut().find(|s| &s.id == id)
    }

    pub fn folder(&self, id: &Id) -> Option<&Folder> {
        self.folders.iter().find(|f| &f.id == id)
    }

    /// Repair and order the library (TypeR's `repairFolderHierarchy`, `normalizeFolders` and the
    /// style sort): duplicate folders dropped, cycles and dangling parents moved to the top
    /// level, sibling order compacted to 0..n, styles of missing folders moved to Unsorted, and
    /// styles sorted Unsorted first, then folder by folder in tree order (stable).
    pub fn normalize(&mut self) {
        // Drop duplicate folder ids (first wins).
        let mut seen: Vec<Id> = Vec::new();
        self.folders.retain(|f| {
            if seen.contains(&f.id) {
                false
            } else {
                seen.push(f.id.clone());
                true
            }
        });
        // Break cycles and dangling parents, in order, as TypeR's `repairFolderHierarchy` does:
        // walking up from each folder, the first link to a missing or already visited folder is cut.
        for i in 0..self.folders.len() {
            let Some(start) = self.folders.get(i).map(|f| f.id.clone()) else { continue };
            let mut visited = vec![start];
            let mut cur = i;
            while let Some(pid) = self.folders.get(cur).and_then(|f| f.parent_id.clone()) {
                match self.folders.iter().position(|f| f.id == pid) {
                    Some(p) if !visited.contains(&pid) => {
                        visited.push(pid);
                        cur = p;
                    }
                    _ => {
                        if let Some(f) = self.folders.get_mut(cur) {
                            f.parent_id = None;
                        }
                        break;
                    }
                }
            }
        }
        // Compact sibling order (stable by the old order).
        let mut keys: Vec<Option<Id>> = Vec::new();
        for f in &self.folders {
            if !keys.contains(&f.parent_id) {
                keys.push(f.parent_id.clone());
            }
        }
        for key in keys {
            let mut idx: Vec<usize> = (0..self.folders.len()).filter(|i| self.folders.get(*i).is_some_and(|f| f.parent_id == key)).collect();
            idx.sort_by(|a, b| {
                let (x, y) = (self.folders.get(*a).map_or(0.0, |f| f.order), self.folders.get(*b).map_or(0.0, |f| f.order));
                x.total_cmp(&y)
            });
            for (n, i) in idx.into_iter().enumerate() {
                if let Some(f) = self.folders.get_mut(i) {
                    f.order = n as f64;
                }
            }
        }
        // Styles of missing folders go to Unsorted, then sort by tree order.
        let folder_ids: Vec<Id> = self.folders.iter().map(|f| f.id.clone()).collect();
        for s in &mut self.styles {
            if s.folder.as_ref().is_some_and(|f| !folder_ids.contains(f)) {
                s.folder = None;
            }
        }
        let order = self.folder_order();
        let rank = |s: &Style| match &s.folder {
            None => 0,
            Some(f) => order.iter().position(|o| o == f).map_or(usize::MAX, |p| p + 1),
        };
        let mut styles = std::mem::take(&mut self.styles);
        styles.sort_by_key(|s| rank(s));
        self.styles = styles;
    }

    /// Folder ids in tree order (depth first, siblings by `order`).
    pub fn folder_order(&self) -> Vec<Id> {
        self.flatten_tree().into_iter().map(|e| e.id).collect()
    }

    /// The folder tree flattened depth first, with depth and breadcrumb (`flattenFolderTree`).
    pub fn flatten_tree(&self) -> Vec<FolderEntry> {
        let mut out = Vec::new();
        self.flatten_children(None, &[], 0, &mut out);
        out
    }

    fn children_of(&self, parent: Option<&Id>) -> Vec<&Folder> {
        let mut c: Vec<&Folder> = self.folders.iter().filter(|f| f.parent_id.as_ref() == parent).collect();
        c.sort_by(|a, b| a.order.total_cmp(&b.order));
        c
    }

    fn flatten_children(&self, parent: Option<&Id>, crumbs: &[String], depth: usize, out: &mut Vec<FolderEntry>) {
        if depth > MAX_FOLDER_DEPTH {
            return;
        }
        for f in self.children_of(parent) {
            if out.iter().any(|e| e.id == f.id) {
                continue; // cycle guard (normalize breaks cycles; this keeps an unnormalized tree safe)
            }
            let mut path = crumbs.to_vec();
            path.push(f.name.clone());
            out.push(FolderEntry { id: f.id.clone(), name: f.name.clone(), parent_id: f.parent_id.clone(), depth, label: path.join(" / ") });
            self.flatten_children(Some(&f.id), &path, depth + 1, out);
        }
    }

    /// Every descendant folder of `id`, breadth first (`collectDescendantIds`); cycle-safe.
    pub fn descendants(&self, id: &Id) -> Vec<Id> {
        collect_descendants(&self.folders, id)
    }

    /// The styles whose prefixes are matched (`getAutomaticTagStyles`). With `isolate` (the
    /// "current folder tag priority" setting) only styles in exactly the current style's folder
    /// (Unsorted is a group of its own; subfolders and parents are not included); without it, or
    /// when the current style is unknown, all styles.
    pub fn tag_styles(&self, current: Option<&Id>, isolate: bool) -> Vec<&Style> {
        let all = || self.styles.iter().collect();
        if !isolate {
            return all();
        }
        let Some(cur) = current.and_then(|id| self.style(id)) else { return all() };
        let folder = cur.folder.clone();
        self.styles.iter().filter(|s| s.folder == folder).collect()
    }

    /// The styles of the current style's folder, for numpad 1–9 (`getNumpadStyleId`): the
    /// `index`-th (0-based) style of that folder (the first style's folder if `current` is
    /// unknown).
    pub fn style_in_current_folder(&self, current: Option<&Id>, index: usize) -> Option<&Style> {
        let cur = current.and_then(|id| self.style(id)).or_else(|| self.styles.first())?;
        self.styles.iter().filter(|s| s.folder == cur.folder).nth(index)
    }

    /// An id not used by any style or folder: `<prefix><n>` for the first free n.
    pub fn fresh_id(&self, prefix: &str) -> Id {
        let used = |c: &Id| self.styles.iter().any(|s| &s.id == c) || self.folders.iter().any(|f| &f.id == c);
        let mut n = self.styles.len() + self.folders.len();
        loop {
            let candidate = Id::Str(format!("{prefix}{}", to_base36(n as u64)));
            if !used(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Add or update a style (`saveStyle`). Prefixes given as one string are split on newlines
    /// and `;`; size presets are normalized. Returns the stored id.
    pub fn save_style(&mut self, mut style: Style) -> Id {
        style.prefixes = style.prefixes.iter().map(|p| crate::pages::js_trim(p).to_string()).filter(|p| !p.is_empty()).collect();
        crate::presets::normalize_style_presets(&mut style);
        let id = style.id.clone();
        match self.style_mut(&id) {
            Some(existing) => *existing = style,
            None => self.styles.push(style),
        }
        self.normalize();
        id
    }

    /// Remove a style. Returns whether it existed.
    pub fn delete_style(&mut self, id: &Id) -> bool {
        let before = self.styles.len();
        self.styles.retain(|s| &s.id != id);
        before != self.styles.len()
    }

    /// Copy a style as `<name> copy` with a new id into `folder` (its own folder when `None`).
    /// Returns the new id.
    pub fn duplicate_style(&mut self, id: &Id, folder: Option<Option<Id>>) -> Option<Id> {
        let mut copy = self.style(id)?.clone();
        copy.id = self.fresh_id("s");
        copy.name = format!("{} copy", copy.name);
        if let Some(f) = folder {
            copy.folder = f.filter(|f| self.folder(f).is_some());
        }
        let new_id = copy.id.clone();
        self.styles.push(copy);
        self.normalize();
        Some(new_id)
    }

    /// Flip a style's `prefixesDisabled` (the eye toggle). Returns the new "disabled" state.
    pub fn toggle_prefixes(&mut self, id: &Id) -> Option<bool> {
        let s = self.style_mut(id)?;
        let disabled = !s.prefixes_disabled();
        s.props.insert("prefixesDisabled".into(), Value::Bool(disabled));
        Some(disabled)
    }

    /// Move a style into `folder` (`None` = Unsorted) and optionally reorder that folder's styles
    /// by `order` (ids; unknown ones go last) (`moveStyleToFolder`).
    pub fn move_style(&mut self, id: &Id, folder: Option<Id>, order: &[Id]) -> Result<(), String> {
        if let Some(f) = &folder
            && self.folder(f).is_none()
        {
            return Err(format!("no folder {f}"));
        }
        let pos = self.styles.iter().position(|s| &s.id == id).ok_or_else(|| format!("no style {id}"))?;
        let mut moved = self.styles.remove(pos);
        moved.folder = folder.clone();
        self.styles.push(moved);
        if !order.is_empty() {
            let rank = |s: &Style| order.iter().position(|o| o == &s.id).unwrap_or(usize::MAX);
            let (mut target, rest): (Vec<Style>, Vec<Style>) = std::mem::take(&mut self.styles).into_iter().partition(|s| s.folder == folder);
            target.sort_by_key(|s| rank(s));
            self.styles = rest;
            self.styles.extend(target);
        }
        self.normalize();
        Ok(())
    }

    /// Add or update a folder (`saveFolder`). `style_ids`, when given, become exactly the
    /// folder's members (styles dropped from it go to Unsorted). A missing parent puts the folder
    /// at the top level; without `order_given` a new or re-parented folder goes last among its
    /// siblings and an edited one keeps its place (TypeR moves it last).
    pub fn save_folder(&mut self, mut folder: Folder, style_ids: Option<&[Id]>, order_given: bool) {
        if let Some(ids) = style_ids {
            for s in &mut self.styles {
                if s.folder.as_ref() == Some(&folder.id) && !ids.contains(&s.id) {
                    s.folder = None;
                } else if ids.contains(&s.id) {
                    s.folder = Some(folder.id.clone());
                }
            }
        }
        if folder.parent_id.as_ref().is_some_and(|p| self.folder(p).is_none() || p == &folder.id) {
            folder.parent_id = None;
        }
        let siblings = self.folders.iter().filter(|f| f.parent_id == folder.parent_id && f.id != folder.id).count();
        if !order_given {
            // A new folder (or one moved to another parent) goes last; a renamed one keeps its place.
            folder.order = match self.folder(&folder.id) {
                Some(old) if old.parent_id == folder.parent_id => old.order,
                _ => siblings as f64,
            };
        }
        match self.folders.iter_mut().find(|f| f.id == folder.id) {
            Some(existing) => *existing = folder,
            None => self.folders.push(folder),
        }
        self.normalize();
    }

    /// Delete a folder and its subfolders (`deleteFolder`). Their styles move to Unsorted, or are
    /// deleted with `permanent` (Shift + Delete). Returns whether the folder existed.
    pub fn delete_folder(&mut self, id: &Id, permanent: bool) -> bool {
        if self.folder(id).is_none() {
            return false;
        }
        let mut remove = vec![id.clone()];
        remove.extend(self.descendants(id));
        self.folders.retain(|f| !remove.contains(&f.id));
        if permanent {
            self.styles.retain(|s| s.folder.as_ref().is_none_or(|f| !remove.contains(f)));
        } else {
            for s in &mut self.styles {
                if s.folder.as_ref().is_some_and(|f| remove.contains(f)) {
                    s.folder = None;
                }
            }
        }
        self.normalize();
        true
    }

    /// Deep-copy a folder with its subfolders and styles, each named `<name> copy` with new ids
    /// (`duplicateFolder`). Returns the new folder's id.
    pub fn duplicate_folder(&mut self, id: &Id) -> Option<Id> {
        let original = self.folder(id)?.clone();
        let source = self.clone();
        let root = self.duplicate_folder_rec(&source, &original, original.parent_id.clone(), 0);
        self.normalize();
        Some(root)
    }

    fn duplicate_folder_rec(&mut self, source: &Library, folder: &Folder, parent: Option<Id>, depth: usize) -> Id {
        let siblings = self.folders.iter().filter(|f| f.parent_id == parent).count();
        let new_id = self.fresh_id("f");
        self.folders.push(Folder { id: new_id.clone(), name: format!("{} copy", folder.name), parent_id: parent, order: siblings as f64 });
        for s in source.styles.iter().filter(|s| s.folder.as_ref() == Some(&folder.id)) {
            let mut copy = s.clone();
            copy.id = self.fresh_id("s");
            copy.name = format!("{} copy", s.name);
            copy.folder = Some(new_id.clone());
            self.styles.push(copy);
        }
        if depth < MAX_FOLDER_DEPTH {
            for child in source.children_of(Some(&folder.id)) {
                self.duplicate_folder_rec(source, child, Some(new_id.clone()), depth + 1);
            }
        }
        new_id
    }

    /// Reorder the children of `parent` by `order` (ids; others keep their place)
    /// (`reorderFolders`).
    pub fn reorder_folders(&mut self, parent: Option<&Id>, order: &[Id]) {
        for f in &mut self.folders {
            if f.parent_id.as_ref() == parent
                && let Some(p) = order.iter().position(|o| o == &f.id)
            {
                f.order = p as f64;
            }
        }
        self.normalize();
    }
}

/// Breadth-first descendants of `id` in `folders`; cycle-safe (`collectDescendantIds`).
pub fn collect_descendants(folders: &[Folder], id: &Id) -> Vec<Id> {
    let mut out = Vec::new();
    let mut visited = vec![id.clone()];
    let mut queue = std::collections::VecDeque::from([id.clone()]);
    while let Some(cur) = queue.pop_front() {
        for child in folders.iter().filter(|f| f.parent_id.as_ref() == Some(&cur)) {
            if visited.contains(&child.id) {
                continue;
            }
            visited.push(child.id.clone());
            out.push(child.id.clone());
            queue.push_back(child.id.clone());
        }
    }
    out
}

/// A list typed as one string: split on newlines and `;`, trimmed, empties dropped (prefixes,
/// ignore prefixes, ignore tags).
pub fn split_list(s: &str) -> Vec<String> {
    s.split(['\n', ';']).map(|p| crate::pages::js_trim(p.trim_end_matches('\r')).to_string()).filter(|p| !p.is_empty()).collect()
}

fn to_base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS.get((n % 36) as usize).copied().unwrap_or(b'0'));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// TypeR's default `textProps` (`getDefaultStyle`): Tahoma Regular 14 px, auto leading, metrics
/// kerning, centred, hyphenation and hanging punctuation on, smooth anti-aliasing, black.
pub fn default_text_props() -> Value {
    serde_json::json!({
        "layerText": {
            "textGridding": "none",
            "orientation": "horizontal",
            "antiAlias": "antiAliasSmooth",
            "textStyleRange": [{
                "from": 0,
                "to": 100,
                "textStyle": {
                    "fontPostScriptName": "Tahoma",
                    "fontName": "Tahoma",
                    "fontStyleName": "Regular",
                    "fontScript": 0,
                    "fontTechnology": 1,
                    "fontAvailable": true,
                    "size": 14,
                    "impliedFontSize": 14,
                    "horizontalScale": 100,
                    "verticalScale": 100,
                    "autoLeading": true,
                    "tracking": 0,
                    "baselineShift": 0,
                    "impliedBaselineShift": 0,
                    "autoKern": "metricsKern",
                    "fontCaps": "normal",
                    "digitSet": "defaultDigits",
                    "diacXOffset": 0,
                    "markYDistFromBaseline": 100,
                    "otbaseline": "normal",
                    "ligature": false,
                    "altligature": false,
                    "connectionForms": false,
                    "contextualLigatures": false,
                    "baselineDirection": "withStream",
                    "color": {"red": 0, "green": 0, "blue": 0}
                }
            }],
            "paragraphStyleRange": [{
                "from": 0,
                "to": 100,
                "paragraphStyle": {
                    "burasagari": "burasagariNone",
                    "singleWordJustification": "justifyAll",
                    "justificationMethodType": "justifMethodAutomatic",
                    "textEveryLineComposer": false,
                    "alignment": "center",
                    "hangingRoman": true,
                    "hyphenate": true
                }
            }]
        },
        "typeUnit": "pixelsUnit"
    })
}

/// TypeR's default stroke (`getDefaultStroke`): off, white, outer.
pub fn default_stroke() -> Value {
    serde_json::json!({"enabled": false, "size": 0, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn style(id: &str, folder: Option<&str>) -> Style {
        Style { id: Id::str(id), name: id.into(), folder: folder.map(Id::str), edited: None, prefixes: vec![], props: Map::new() }
    }

    fn folder(id: &str, parent: Option<&str>, order: f64) -> Folder {
        Folder { id: Id::str(id), name: id.to_uppercase(), parent_id: parent.map(Id::str), order }
    }

    fn ids(v: &[&Style]) -> Vec<String> {
        v.iter().map(|s| s.id.to_string()).collect()
    }

    // scripts/testFolderTagIsolation.js
    #[test]
    fn current_folder_tag_isolation() {
        let mut unsorted_b = style("unsorted-b", None);
        unsorted_b.folder = None;
        let lib = Library {
            folders: vec![],
            styles: vec![
                style("unsorted-a", None),
                unsorted_b,
                style("folder-a-1", Some("folder-a")),
                style("folder-a-2", Some("folder-a")),
                style("folder-b-1", Some("folder-b")),
                style("nested", Some("folder-a-child")),
            ],
        };
        let all: Vec<String> = lib.styles.iter().map(|s| s.id.to_string()).collect();
        assert_eq!(ids(&lib.tag_styles(Some(&Id::str("folder-a-1")), true)), ["folder-a-1", "folder-a-2"]);
        assert_eq!(ids(&lib.tag_styles(Some(&Id::str("nested")), true)), ["nested"]);
        assert_eq!(ids(&lib.tag_styles(Some(&Id::str("unsorted-a")), true)), ["unsorted-a", "unsorted-b"]);
        assert_eq!(ids(&lib.tag_styles(Some(&Id::str("folder-a-1")), false)), all);
        assert_eq!(ids(&lib.tag_styles(Some(&Id::str("missing")), true)), all);
        assert_eq!(ids(&lib.tag_styles(None, true)), all);
    }

    // scripts/testNumpadStyleShortcuts.js (style lookup half)
    #[test]
    fn numpad_picks_the_nth_style_of_the_current_folder() {
        let lib = Library { folders: vec![], styles: vec![style("unsorted", None), style("a1", Some("a")), style("a2", Some("a")), style("b1", Some("b"))] };
        let pick = |cur: &str, n: usize| lib.style_in_current_folder(Some(&Id::str(cur)), n).map(|s| s.id.to_string());
        assert_eq!(pick("a1", 1).as_deref(), Some("a2"));
        assert_eq!(pick("b1", 1), None);
        assert_eq!(pick("unsorted", 0).as_deref(), Some("unsorted"));
    }

    // scripts/testLibraryRoundTrip.js (collectDescendantIds, repairFolderHierarchy)
    #[test]
    fn cycles_are_repaired_and_descendants_are_cycle_safe() {
        let cycle = vec![folder("a", Some("b"), 0.0), folder("b", Some("a"), 0.0)];
        assert_eq!(collect_descendants(&cycle, &Id::str("a")), vec![Id::str("b")]);
        let mut lib = Library { folders: cycle, styles: vec![] };
        lib.normalize();
        assert!(lib.folders.iter().any(|f| f.parent_id.is_none()));
        assert_eq!(lib.flatten_tree().len(), 2, "both folders stay reachable");
    }

    #[test]
    fn normalize_orders_styles_by_tree_and_fixes_dangling_refs() {
        let mut lib = Library {
            folders: vec![folder("b", None, 5.0), folder("a", None, 1.0), folder("a1", Some("a"), 0.0), folder("x", Some("ghost"), 9.0)],
            styles: vec![style("in-b", Some("b")), style("in-a1", Some("a1")), style("lost", Some("gone")), style("in-a", Some("a")), style("u", None)],
        };
        lib.normalize();
        let order: Vec<String> = lib.styles.iter().map(|s| s.id.to_string()).collect();
        assert_eq!(order, ["lost", "u", "in-a", "in-a1", "in-b"]);
        assert_eq!(lib.style(&Id::str("lost")).unwrap().folder, None);
        assert_eq!(lib.folder(&Id::str("x")).unwrap().parent_id, None, "dangling parent goes to the top level");
        let tree: Vec<(String, usize, String)> = lib.flatten_tree().into_iter().map(|e| (e.id.to_string(), e.depth, e.label)).collect();
        assert_eq!(tree, [("a".into(), 0, "A".into()), ("a1".into(), 1, "A / A1".into()), ("b".into(), 0, "B".into()), ("x".into(), 0, "X".into())]);
        assert_eq!(lib.folder(&Id::str("b")).unwrap().order, 1.0, "sibling order compacted");
    }

    #[test]
    fn folder_crud() {
        let mut lib = Library {
            folders: vec![folder("a", None, 0.0), folder("a1", Some("a"), 0.0)],
            styles: vec![style("s1", Some("a")), style("s2", Some("a1")), style("s3", None)],
        };
        lib.normalize();
        let dup = lib.duplicate_folder(&Id::str("a")).unwrap();
        assert_eq!(lib.folder(&dup).unwrap().name, "A copy");
        assert_eq!(lib.folders.len(), 4);
        assert_eq!(lib.styles.len(), 5);
        assert!(lib.styles.iter().any(|s| s.name == "s2 copy" && s.folder.as_ref() != Some(&Id::str("a1"))));

        assert!(lib.delete_folder(&Id::str("a"), false));
        assert_eq!(lib.style(&Id::str("s2")).unwrap().folder, None, "subfolder styles go to Unsorted");
        assert!(lib.delete_folder(&dup, true));
        assert_eq!(lib.styles.len(), 3, "permanent delete removes the copies");
        assert!(!lib.delete_folder(&Id::str("nope"), false));

        lib.save_folder(Folder { id: Id::str("n"), name: "New".into(), parent_id: Some(Id::str("ghost")), order: 0.0 }, Some(&[Id::str("s1")]), false);
        assert_eq!(lib.folder(&Id::str("n")).unwrap().parent_id, None);
        assert_eq!(lib.style(&Id::str("s1")).unwrap().folder, Some(Id::str("n")));
    }

    #[test]
    fn style_crud_and_order() {
        let mut lib = Library { folders: vec![folder("a", None, 0.0)], styles: vec![style("s1", Some("a")), style("s2", Some("a"))] };
        let copy = lib.duplicate_style(&Id::str("s1"), None).unwrap();
        assert_eq!(lib.style(&copy).unwrap().name, "s1 copy");
        assert_eq!(lib.style(&copy).unwrap().folder, Some(Id::str("a")));
        lib.move_style(&copy, Some(Id::str("a")), &[copy.clone(), Id::str("s2"), Id::str("s1")]).unwrap();
        let order: Vec<String> = lib.styles.iter().map(|s| s.id.to_string()).collect();
        assert_eq!(order, [copy.to_string(), "s2".into(), "s1".into()]);
        assert!(lib.move_style(&copy, Some(Id::str("ghost")), &[]).is_err());
        assert_eq!(lib.toggle_prefixes(&Id::str("s1")), Some(true));
        assert!(lib.style(&Id::str("s1")).unwrap().prefixes_disabled());
        assert!(lib.delete_style(&Id::str("s1")));
        assert!(!lib.delete_style(&Id::str("s1")));
        let mut s = style("s9", None);
        s.prefixes = vec![" SFX: ".into(), "".into()];
        lib.save_style(s);
        assert_eq!(lib.style(&Id::str("s9")).unwrap().prefixes, ["SFX:"]);
    }

    #[test]
    fn style_json_round_trip_keeps_whitelisted_keys_only() {
        let v = json!({"id": "s", "name": "Dialogue", "folder": null, "textProps": {}, "prefixes": ["REG:"], "prefixesDisabled": true,
            "sizePresets": [12, 16, 20], "autoSizeByPageWidth": true, "sizePresetDefaultIndex": 1, "sizePresetMinWidths": [100, null, 2000], "junk": 1});
        let s = Style::from_json(&v).unwrap();
        let mut expected = v.clone();
        expected.as_object_mut().unwrap().remove("junk");
        assert_eq!(s.to_json(), expected);
        assert_eq!(Style::from_json(&json!({"name": "no id"})), None);
        let numeric = Style::from_json(&json!({"id": 7, "name": "n", "edited": 1700000000000u64})).unwrap();
        assert_eq!(numeric.to_json()["id"], json!(7));
        assert_eq!(numeric.to_json()["edited"].to_string(), "1700000000000");
    }

    #[test]
    fn default_style_matches_typer() {
        let s = Style::new(Id::str("d"), "Default");
        assert_eq!(s.size(), Some(14.0));
        assert_eq!(s.type_unit(), Some("pixelsUnit"));
        assert_eq!(s.text_style().unwrap()["fontPostScriptName"], "Tahoma");
        assert_eq!(s.paragraph_style().unwrap()["hyphenate"], true);
        assert_eq!(s.prefix_color(), DEFAULT_PREFIX_COLOR);
        assert_eq!(s.text_type(), TextType::Inherit);
    }

    #[test]
    fn fresh_ids_are_unique_and_lists_split() {
        let lib = Library { folders: vec![], styles: vec![style("s2", None)] };
        assert_eq!(lib.fresh_id("s"), Id::str("s1"));
        assert_ne!(lib.fresh_id("s"), Id::str("s2"));
        assert_eq!(split_list("## ;\n//note\r\n;;"), ["##", "//note"]);
        assert_eq!(to_base36(36 * 36), "100");
    }
}
