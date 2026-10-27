//! The browser-free half of the embedding API v1 (`embed_api.rs`, contract in `embed/README.md`):
//! versions, modes, tool ids, validation of the host's params, document summaries and the events
//! derived from one frame's state to the next. Tested natively.
//!
//! Compatibility rules for this file: `EMBED_API_VERSION` changes only when an existing function,
//! param or event changes shape. Adding a function, an optional param, an event or a feature string
//! keeps it, so a host built for v1 keeps working; hosts detect additions with `features`.

use photocraft_engine::DocState;
use photocraft_ui_egui::state::{Panels, Tool};
use photocraft_ui_egui::view_cmds::ViewOptions;
use serde_json::{Value, json};

/// The embedding API version a host checks before using anything else.
pub const EMBED_API_VERSION: u32 = 1;

/// What this build offers, for hosts that adapt at runtime. Strings are only ever added.
pub const FEATURES: &[&str] = &[
    "documents.open",
    "documents.new",
    "documents.save.psd",
    "documents.save.png",
    "documents.save.pcraft",
    "documents.close",
    "execute",
    "request",
    "commands.list",
    "tools.set",
    "modes.live",
    "viewport.fit",
    "viewport.zoom",
    "history",
    "events",
];

/// The build stamp: the Capishot commit and the build id, set by `scripts/build-embed.sh`
/// (`CAPISHOT_COMMIT`, `CAPISHOT_BUILD_HASH`), plus the app and `.pcraft` versions.
pub fn version() -> Value {
    json!({
        "embedApi": EMBED_API_VERSION,
        "capishotCommit": option_env!("CAPISHOT_COMMIT").unwrap_or("unknown"),
        "buildHash": option_env!("CAPISHOT_BUILD_HASH").unwrap_or("dev"),
        "app": env!("CARGO_PKG_VERSION"),
        "format": photocraft_format::FORMAT_VERSION,
        "features": FEATURES,
    })
}

/// How much of Capishot's own UI shows around the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Only the canvas (zoom, pan and the tool cursor still work): the host draws its own chrome.
    #[default]
    Embedded,
    /// The whole Capishot UI (menus, toolbar, options bar, panels) on the same documents.
    Full,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode, String> {
        match s {
            "embedded" => Ok(Mode::Embedded),
            "full" => Ok(Mode::Full),
            other => Err(format!("unknown mode `{other}` (embedded, full)")),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Mode::Embedded => "embedded",
            Mode::Full => "full",
        }
    }
}

/// The screen mode embedded mode uses: no document tabs and no scroll bars, but (unlike Full
/// Screen Mode) the normal pasteboard and no Esc to leave it.
const EMBEDDED_SCREEN_MODE: &str = "fullScreenWithMenuBar";

/// The full-mode chrome kept while embedded, to bring it back as the user left it.
#[derive(Clone, Debug, PartialEq)]
pub struct Chrome {
    pub panels: Panels,
    pub screen_mode: String,
}

impl Chrome {
    pub fn of(panels: &Panels, view: &ViewOptions) -> Chrome {
        Chrome { panels: panels.clone(), screen_mode: view.screen_mode.clone() }
    }

    /// A saved chrome that shows nothing (taken from an embedded session's preferences) would
    /// make "full" look embedded: the defaults instead.
    fn usable(&self) -> bool {
        (self.panels.toolbar || self.panels.dock || self.panels.menu_bar) && self.screen_mode != EMBEDDED_SCREEN_MODE
    }
}

/// Hides everything but the canvas. Applied every frame while embedded, so a shortcut that
/// brings chrome back (Tab, ⇧Tab, F) can't leave the host with half of Capishot's UI.
/// Floating windows the user opened on purpose (Brush Settings…) are left alone.
pub fn hide_chrome(panels: &mut Panels, view: &mut ViewOptions) {
    panels.menu_bar = false;
    panels.options_bar = false;
    panels.status_bar = false;
    panels.toolbar = false;
    panels.dock = false;
    panels.rail = false;
    panels.hidden_by_tab = None;
    if view.screen_mode != EMBEDDED_SCREEN_MODE {
        view.screen_mode = EMBEDDED_SCREEN_MODE.into();
    }
}

/// Brings back the full UI: the chrome saved when embedded mode started, or the defaults.
pub fn show_chrome(saved: Option<&Chrome>, panels: &mut Panels, view: &mut ViewOptions) {
    match saved.filter(|c| c.usable()) {
        Some(c) => {
            *panels = c.panels.clone();
            view.screen_mode = c.screen_mode.clone();
        }
        None => {
            let d = Panels::default();
            panels.menu_bar = d.menu_bar;
            panels.options_bar = d.options_bar;
            panels.status_bar = d.status_bar;
            panels.toolbar = d.toolbar;
            panels.dock = d.dock;
            panels.rail = d.rail;
            panels.hidden_by_tab = None;
            view.screen_mode = "standard".into();
        }
    }
}

/// A tool's id for hosts: its name in lowerCamelCase (`brush`, `eraser`, `cloneStamp`).
pub fn tool_id(t: Tool) -> String {
    let name = format!("{t:?}");
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Every tool id, in toolbar order.
pub fn tools() -> Vec<String> {
    Tool::ALL.iter().map(|t| tool_id(*t)).collect()
}

/// A tool from its id (or any name `ui.set` accepts).
pub fn parse_tool(id: &str) -> Result<Tool, String> {
    Tool::from_name(id).ok_or_else(|| format!("unknown tool `{id}`"))
}

/// The control requests `setTool(tool, options)` makes, in order. Options (all optional):
/// `size` (px), `color` (`#rrggbb`, the foreground), `opacity`, `flow`, `hardness` (0..1) and
/// `brush` (any `tools.setBrush` fields, passed through). Everything is checked first.
pub fn set_tool_requests(tool: &str, options: &Value) -> Result<Vec<(&'static str, Value)>, String> {
    let tool = parse_tool(tool)?;
    let empty = serde_json::Map::new();
    let opts = match options {
        Value::Null => &empty,
        Value::Object(o) => o,
        _ => return Err("setTool options must be an object".into()),
    };
    const KEYS: [&str; 6] = ["size", "color", "opacity", "flow", "hardness", "brush"];
    if let Some(k) = opts.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown setTool option `{k}` ({})", KEYS.join(", ")));
    }
    let mut ui = json!({ "tool": tool_id(tool) });
    if let Some(size) = opts.get("size") {
        let size = size.as_f64().filter(|s| s.is_finite() && (1.0..=5000.0).contains(s)).ok_or("size must be a number of pixels, 1..5000")?;
        ui["brushSize"] = json!(size);
    }
    let mut out = vec![("ui.set", ui)];
    if let Some(color) = opts.get("color") {
        let c = color.as_str().filter(|c| is_hex_color(c)).ok_or("color must be \"#rrggbb\"")?;
        out.push(("engine.execute", json!({ "command": "tools.setColors", "params": { "foreground": c } })));
    }
    let mut brush = match opts.get("brush") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(b)) => b.clone(),
        Some(_) => return Err("brush must be an object of tools.setBrush fields".into()),
    };
    for k in ["opacity", "flow", "hardness"] {
        if let Some(v) = opts.get(k) {
            let v = v.as_f64().filter(|v| (0.0..=1.0).contains(v)).ok_or_else(|| format!("{k} must be 0..1"))?;
            brush.insert(k.into(), json!(v));
        }
    }
    if !brush.is_empty() {
        out.push(("engine.execute", json!({ "command": "tools.setBrush", "params": brush })));
    }
    Ok(out)
}

fn is_hex_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c.chars().skip(1).all(|ch| ch.is_ascii_hexdigit())
}

/// `zoom(level)`: a factor (1 = 100%) becomes `ui.set {zoom}`; `"in"`, `"out"`, `"fit"` and
/// `"actual"` the View menu's commands.
pub fn zoom_request(level: &Value) -> Result<(&'static str, Value), String> {
    if let Some(z) = level.as_f64() {
        if !(z.is_finite() && (0.002..=320.0).contains(&z)) {
            return Err("zoom must be a factor between 0.002 and 320 (1 = 100%)".into());
        }
        return Ok(("ui.set", json!({ "zoom": z })));
    }
    let command = match level.as_str() {
        Some("in") => "view.zoomIn",
        Some("out") => "view.zoomOut",
        Some("actual") => "view.actualPixels",
        Some("fit") => return Ok(("ui.set", json!({ "fit": true }))),
        _ => return Err("zoom takes a factor or \"in\", \"out\", \"fit\", \"actual\"".into()),
    };
    Ok(("engine.execute", json!({ "command": command, "params": {} })))
}

/// `newDocument(options)` → `file.new` params: `width` and `height` required (pixels), `name`,
/// `background` (`white`, `transparent`, `black`, `#rrggbb`), `resolution` (ppi), `mode`, `depth`.
pub fn new_document_params(opts: &Value) -> Result<Value, String> {
    let o = opts.as_object().ok_or("newDocument takes {width, height, …}")?;
    const KEYS: [&str; 7] = ["width", "height", "name", "background", "resolution", "mode", "depth"];
    if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown newDocument option `{k}` ({})", KEYS.join(", ")));
    }
    for k in ["width", "height"] {
        let ok = o.get(k).and_then(Value::as_u64).is_some_and(|v| (1..=300_000).contains(&v));
        if !ok {
            return Err(format!("{k} must be a whole number of pixels, 1..300000"));
        }
    }
    Ok(Value::Object(o.clone()))
}

/// The formats `saveDocument` writes: layered (`psd`, `pcraft`) or flat.
pub fn save_format(format: &str) -> Result<String, String> {
    let f = format.trim().trim_start_matches('.').to_ascii_lowercase();
    const FORMATS: [&str; 8] = ["psd", "pcraft", "png", "jpg", "jpeg", "webp", "tif", "tiff"];
    if FORMATS.contains(&f.as_str()) { Ok(f) } else { Err(format!("can't save as `{format}` ({})", FORMATS.join(", "))) }
}

/// A document as hosts see it.
pub fn doc_info(st: &DocState, active: bool) -> Value {
    json!({
        "id": st.doc.id.0,
        "name": st.doc.name,
        "width": st.doc.size.width,
        "height": st.doc.size.height,
        "revision": st.revision,
        "dirty": st.revision != st.saved_revision,
        "active": active,
    })
}

/// What the events compare from one frame to the next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub active: Option<u64>,
    pub docs: Vec<DocSnap>,
    pub tool: String,
    /// The status bar's error, if it shows one.
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocSnap {
    pub id: u64,
    pub revision: u64,
    pub dirty: bool,
    /// The selection's bounds `[x, y, width, height]`, `None` without a selection.
    pub selection: Option<[i32; 4]>,
}

/// The events between two frames (none for the first one: hosts read the state then).
pub fn events(prev: Option<&Snapshot>, next: &Snapshot) -> Vec<Value> {
    let Some(prev) = prev else { return Vec::new() };
    let mut out = Vec::new();
    let ids = |s: &Snapshot| s.docs.iter().map(|d| d.id).collect::<Vec<_>>();
    if ids(prev) != ids(next) || prev.active != next.active {
        out.push(json!({ "type": "documents", "documents": ids(next), "active": next.active }));
    }
    for d in &next.docs {
        let Some(before) = prev.docs.iter().find(|p| p.id == d.id) else { continue };
        if before.revision != d.revision {
            out.push(json!({ "type": "documentChanged", "id": d.id, "revision": d.revision, "dirty": d.dirty }));
        }
        if before.dirty != d.dirty {
            out.push(json!({ "type": "dirty", "id": d.id, "dirty": d.dirty }));
        }
        if before.selection != d.selection {
            out.push(json!({ "type": "selectionChanged", "id": d.id, "bounds": d.selection }));
        }
    }
    if prev.tool != next.tool {
        out.push(json!({ "type": "toolChanged", "tool": next.tool }));
    }
    if let Some(e) = next.error.as_ref().filter(|e| prev.error.as_ref() != Some(*e)) {
        out.push(json!({ "type": "error", "message": e, "source": "status", "fatal": false }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_names_the_api_and_the_build() {
        let v = version();
        assert_eq!(v["embedApi"], json!(EMBED_API_VERSION));
        assert!(v["capishotCommit"].is_string() && v["buildHash"].is_string());
        assert!(v["features"].as_array().is_some_and(|f| f.contains(&json!("modes.live"))));
    }

    #[test]
    fn modes_parse_and_round_trip() {
        assert_eq!(Mode::parse("embedded"), Ok(Mode::Embedded));
        assert_eq!(Mode::parse(Mode::Full.id()), Ok(Mode::Full));
        assert!(Mode::parse("Full").is_err());
    }

    #[test]
    fn embedded_hides_the_chrome_and_full_brings_back_what_was_there() {
        let (mut panels, mut view) = (Panels::default(), ViewOptions::default());
        panels.history = true;
        let saved = Chrome::of(&panels, &view);
        hide_chrome(&mut panels, &mut view);
        assert!(!panels.menu_bar && !panels.toolbar && !panels.dock && !panels.options_bar && !panels.status_bar && !panels.rail);
        assert!(view.hides_tabs() && !view.hides_chrome() && !view.shows_scrollbars(), "no tabs or bars, normal pasteboard");
        show_chrome(Some(&saved), &mut panels, &mut view);
        assert_eq!(panels, saved.panels);
        assert_eq!(view.screen_mode, "standard");
    }

    #[test]
    fn full_mode_from_an_embedded_preference_falls_back_to_the_defaults() {
        let (mut panels, mut view) = (Panels::default(), ViewOptions::default());
        hide_chrome(&mut panels, &mut view);
        let saved = Chrome::of(&panels, &view);
        show_chrome(Some(&saved), &mut panels, &mut view);
        assert!(panels.menu_bar && panels.toolbar && panels.dock && panels.options_bar);
        assert_eq!(view.screen_mode, "standard");
    }

    #[test]
    fn tool_ids_are_camel_case_and_parse_back() {
        assert_eq!(tool_id(Tool::CloneStamp), "cloneStamp");
        assert_eq!(tool_id(Tool::Brush), "brush");
        for id in tools() {
            assert_eq!(tool_id(parse_tool(&id).expect("a tool")), id);
        }
        assert!(parse_tool("flamethrower").is_err());
    }

    #[test]
    fn set_tool_validates_everything_before_any_request() {
        let r = set_tool_requests("eraser", &json!({"size": 40, "color": "#112233", "opacity": 0.5, "brush": {"spacing": 0.1}})).expect("ok");
        assert_eq!(r[0], ("ui.set", json!({"tool": "eraser", "brushSize": 40.0})));
        assert_eq!(r[1].1["params"], json!({"foreground": "#112233"}));
        assert_eq!(r[2].1, json!({"command": "tools.setBrush", "params": {"spacing": 0.1, "opacity": 0.5}}));
        assert_eq!(set_tool_requests("brush", &Value::Null).map(|r| r.len()), Ok(1));
        for bad in [json!({"size": -1}), json!({"color": "red"}), json!({"opacity": 2}), json!({"nope": 1}), json!([1]), json!({"brush": 3})] {
            assert!(set_tool_requests("brush", &bad).is_err(), "{bad}");
        }
        assert!(set_tool_requests("laser", &Value::Null).is_err());
    }

    #[test]
    fn zoom_levels() {
        assert_eq!(zoom_request(&json!(2)), Ok(("ui.set", json!({"zoom": 2.0}))));
        assert_eq!(zoom_request(&json!("fit")), Ok(("ui.set", json!({"fit": true}))));
        assert_eq!(zoom_request(&json!("in")).map(|r| r.1["command"].clone()), Ok(json!("view.zoomIn")));
        for bad in [json!(0), json!(-1), json!(1e9), json!("huge"), Value::Null] {
            assert!(zoom_request(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn new_documents_need_a_size() {
        assert!(new_document_params(&json!({"width": 800, "height": 1200, "background": "transparent"})).is_ok());
        for bad in [json!({}), json!({"width": 0, "height": 10}), json!({"width": 10.5, "height": 10}), json!({"width": 10, "height": 10, "x": 1}), json!(3)] {
            assert!(new_document_params(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn documents_are_summarised_for_hosts() {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 30, "height": 20, "name": "page"})).expect("a new document");
        let st = s.active().expect("a document");
        let v = doc_info(st, true);
        assert_eq!(v["id"], json!(st.doc.id.0));
        assert_eq!((&v["name"], &v["width"], &v["height"], &v["active"]), (&json!("page"), &json!(30), &json!(20), &json!(true)));
        assert_eq!(v["dirty"], json!(st.revision != st.saved_revision));
    }

    #[test]
    fn save_formats() {
        assert_eq!(save_format(".PSD"), Ok("psd".into()));
        assert!(save_format("exe").is_err());
    }

    #[test]
    fn events_follow_what_changed() {
        let a = Snapshot { active: Some(1), docs: vec![DocSnap { id: 1, revision: 3, dirty: false, selection: None }], tool: "brush".into(), error: None };
        assert!(events(None, &a).is_empty(), "the first frame is the baseline");
        assert!(events(Some(&a), &a).is_empty());
        let mut b = a.clone();
        b.docs[0] = DocSnap { id: 1, revision: 4, dirty: true, selection: Some([0, 0, 5, 5]) };
        b.tool = "eraser".into();
        b.error = Some("boom".into());
        let types: Vec<_> = events(Some(&a), &b).iter().map(|e| e["type"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(types, ["documentChanged", "dirty", "selectionChanged", "toolChanged", "error"]);
        let mut c = b.clone();
        c.docs.push(DocSnap { id: 2, ..Default::default() });
        c.active = Some(2);
        let e = events(Some(&b), &c);
        assert_eq!(e, [json!({"type": "documents", "documents": [1, 2], "active": 2})], "the same error isn't repeated");
    }
}
