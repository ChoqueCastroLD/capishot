//! Copy layers into another open document (#589): what Photoshop does when layers are dragged from
//! the Layers panel, or with the Move tool from the canvas, onto another document's tab or window.
//! Photoshop has no menu item for it; `layer.copyToDocument` is the command the drag dispatches and
//! agents call. `layer.copyLayers` / `layer.pasteLayers` do the same through a layer clipboard
//! (UI-217-23, the Layers panel's context menu), so the destination can be opened later.
//!
//! The copies keep their names, content, masks, styles and blending; they land above the
//! destination's active layer as one history step there, and the destination becomes the active
//! document. Pixels are converted to the destination's colour profile (Color Settings' intent and
//! black point compensation) and bit depth, as Photoshop converts dragged layers.

use std::collections::{HashMap, HashSet};

use photocraft_color::ColorMode;
use photocraft_doc::{Document, Layer, LayerContent, LayerId, SmartContentsId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const CMD: &str = "layer.copyToDocument";

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.ok_or("no active layer")?;
    if s.documents().len() < 2 { Err("no other document is open".into()) } else { Ok(()) }
}

/// A document index param (`None` when absent), checked against the open documents.
fn doc_index(s: &Session, p: &Value, key: &str) -> Result<Option<usize>> {
    let Some(v) = p.get(key) else { return Ok(None) };
    let i = v.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(format!("`{key}` must be a document index")))?;
    if i >= s.documents().len() {
        return Err(bad(format!("no document {i} (there are {})", s.documents().len())));
    }
    Ok(Some(i))
}

/// An `[x, y]` param of finite numbers within ±10⁷ pixels.
fn point(p: &Value, key: &str) -> Result<Option<[f64; 2]>> {
    let Some(v) = p.get(key) else { return Ok(None) };
    let xy = v.as_array().filter(|a| a.len() == 2).and_then(|a| Some([a[0].as_f64()?, a[1].as_f64()?]));
    match xy {
        Some(xy) if xy.iter().all(|c| c.is_finite() && c.abs() <= 1e7) => Ok(Some(xy)),
        _ => Err(bad(format!("`{key}` must be [x, y] (pixels)"))),
    }
}

/// Modes whose layers convert through the colour engine.
pub(crate) fn layered(mode: ColorMode) -> bool {
    matches!(mode, ColorMode::Rgb | ColorMode::Grayscale | ColorMode::Cmyk | ColorMode::Lab)
}

fn center(r: Rect) -> [f64; 2] {
    [(f64::from(r.x0) + f64::from(r.x1)) / 2.0, (f64::from(r.y0) + f64::from(r.y1)) / 2.0]
}

/// Restore missing shape caches, or refresh every shape for a changed canvas, including groups.
fn prepare_shapes(layers: &mut [Layer], doc: &Document, new_canvas: bool) {
    for layer in layers {
        match &mut layer.content {
            LayerContent::Shape(shape) if new_canvas || shape.cache.is_none() => crate::vector_cmds::refresh_shape(doc, shape),
            LayerContent::Group(group) => prepare_shapes(&mut group.children, doc, new_canvas),
            _ => {}
        }
    }
}

/// Readies `copies` (a scratch document of the source's colour holding duplicated layers) for
/// landing in `dest`: smart objects get fresh contents ids, keeping only the sharing within the
/// batch (contents ids belong to a document, so the copies never join objects already in `dest`,
/// including another opening of the same file), and the pixels convert to `dest`'s profile and
/// bit depth.
pub(crate) fn adapt_copies(s: &Session, copies: &mut Document, dest: &Document) -> Result<()> {
    // Link-group IDs are scoped to a document. Two independently linked pairs can both
    // use group 1; copying one must not link it to the other's existing layers (#3045).
    // Remap each imported group once so links within the copied batch stay intact.
    let mut used: HashSet<u64> = dest.walk().into_iter().filter_map(|(_, _, l)| l.link_group).collect();
    let mut next_group = dest.unused_link_group().ok_or_else(|| EngineError::Other("no free linked-layer group id".into()))?;
    let groups: Vec<_> = copies.walk().into_iter().filter_map(|(_, _, l)| l.link_group.map(|group| (l.id, group))).collect();
    let mut remapped = HashMap::new();
    for (_, group) in &groups {
        if remapped.contains_key(group) {
            continue;
        }
        while used.contains(&next_group) {
            next_group = next_group.checked_add(1).unwrap_or(1);
        }
        remapped.insert(*group, next_group);
        used.insert(next_group);
        next_group = next_group.checked_add(1).unwrap_or(1);
    }
    for (id, group) in groups {
        if let Some(layer) = copies.layer_mut(id) {
            layer.link_group = remapped.get(&group).copied();
        }
    }
    let mut contents = HashMap::new();
    let smart_ids: Vec<_> = copies.walk().into_iter().filter_map(|(_, _, l)| matches!(l.content, LayerContent::Smart(_)).then_some(l.id)).collect();
    for id in smart_ids {
        if let Some(l) = copies.layer_mut(id)
            && let LayerContent::Smart(sm) = &mut l.content
        {
            sm.contents_id = *contents.entry(sm.contents_id).or_insert_with(SmartContentsId::fresh);
        }
    }
    if (copies.mode, &copies.icc_profile) != (dest.mode, &dest.icc_profile) {
        let profile = crate::color_cmds::document_profile(dest);
        crate::color_cmds::convert_document(copies, &profile, s.color.settings.intent(), s.color.settings.bpc)?;
    }
    if copies.depth != dest.depth {
        crate::image_cmds::for_each_surface(&mut copies.layers, true, &mut |surf, _| {
            let f = surf.format().with_sample(dest.depth);
            *surf = surf.convert(f);
        });
    }
    let new_canvas = copies.bounds() != dest.bounds();
    prepare_shapes(&mut copies.layers, dest, new_canvas);
    Ok(())
}

/// Moves every layer of `copies` by whole pixels (content, masks, vectors and smart placement).
pub(crate) fn translate_copies(copies: &mut Document, destination: &Document, dx: i32, dy: i32) {
    if dx != 0 || dy != 0 {
        let mut snapshot = copies.clone();
        // Vector caches are clipped and quantized for the destination, not the clipboard's source.
        snapshot.size = destination.size;
        snapshot.mode = destination.mode;
        snapshot.depth = destination.depth;
        snapshot.icc_profile = destination.icc_profile.clone();
        for l in &mut copies.layers {
            crate::commands::translate_layer(&snapshot, l, dx, dy);
            crate::vector_cmds::translate_vectors(&snapshot, l, f64::from(dx), f64::from(dy));
        }
    }
}

/// Layers copied with `layer.copyLayers`, waiting for `layer.pasteLayers` in any document.
#[derive(Clone, Debug)]
pub struct LayerClip {
    /// The copies, bottom to top, in a scratch document of the source's size, colour and depth.
    copies: Document,
    /// The source's patterns the copies may reference.
    patterns: Vec<photocraft_doc::Pattern>,
    /// Copies of the source's Background, renamed like a new layer where they land.
    backgrounds: Vec<LayerId>,
}

impl LayerClip {
    /// How many layers (top level) are on the layer clipboard.
    pub fn len(&self) -> usize {
        self.copies.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.copies.layers.is_empty()
    }
}

/// The layer ids of a `layers` param (`None`: absent), all present in `doc`, as top-level ids.
fn layer_ids(doc: &Document, p: &Value, selected: Vec<LayerId>, cmd: &str) -> Result<Vec<LayerId>> {
    let bad = |msg: &str| EngineError::BadParams { cmd: cmd.into(), msg: msg.into() };
    let ids: Vec<LayerId> = match p.get("layers") {
        None | Some(Value::Null) => selected,
        Some(v) => v
            .as_array()
            .map(|a| a.iter().map(|id| id.as_u64().map(LayerId)).collect::<Option<Vec<_>>>())
            .and_then(|ids| ids)
            .ok_or_else(|| bad("`layers` must be an array of layer ids"))?,
    };
    if let Some(missing) = ids.iter().find(|id| doc.layer(**id).is_none()) {
        return Err(EngineError::NoLayer(*missing));
    }
    let ids = crate::layer_multi_cmds::top_level(doc, &ids);
    if ids.is_empty() {
        return Err(bad("no layers to copy"));
    }
    if !layered(doc.mode) {
        return Err(EngineError::Other(format!("layers can't be copied from a {:?} document", doc.mode)));
    }
    Ok(ids)
}

/// Copies of `ids` (top level, bottom to top) out of `sdoc`, with everything they need.
fn snapshot(sdoc: &Document, ids: &[LayerId]) -> Result<LayerClip> {
    let mut copies = Document::new("", sdoc.size, sdoc.mode, sdoc.depth);
    copies.icc_profile = sdoc.icc_profile.clone();
    // Type re-lays out at the source's resolution, so it keeps its size in pixels.
    copies.resolution_dpi = sdoc.resolution_dpi;
    let mut backgrounds = Vec::new();
    for id in ids {
        let src_layer = sdoc.layer(*id).ok_or(EngineError::NoLayer(*id))?;
        let l = src_layer.duplicate();
        // A copied Background arrives as an ordinary layer (the destination keeps its own).
        if sdoc.is_background(*id) {
            backgrounds.push(l.id);
        }
        copies.layers.push(l);
    }
    // Missing shape caches are restored in source space, including shapes nested inside groups.
    prepare_shapes(&mut copies.layers, sdoc, false);
    Ok(LayerClip { copies, patterns: sdoc.patterns.clone(), backgrounds })
}

/// Where the copies go: centred on the destination's canvas (`center`), centred on a point
/// (`at`), or offset from their place in the source (`offset`, default: the same place).
struct Placement {
    center: bool,
    at: Option<[f64; 2]>,
    offset: Option<[f64; 2]>,
}

impl Placement {
    fn parse(p: &Value) -> Result<Self> {
        Ok(Self { center: p.get("center").and_then(Value::as_bool).unwrap_or(false), at: point(p, "at")?, offset: point(p, "offset")? })
    }
}

/// Puts `clip`'s copies above the active layer of document `dest` (one history step there) and
/// makes `dest` active; on failure document `restore` stays active.
fn land(s: &mut Session, clip: LayerClip, dest: usize, restore: usize, place: &Placement, label: &str) -> Result<Value> {
    let ddoc = s.documents().get(dest).ok_or(EngineError::NoDocument)?.doc.clone();
    if !layered(ddoc.mode) {
        return Err(EngineError::Other(format!("layers can't be copied into a {:?} document", ddoc.mode)));
    }
    let LayerClip { mut copies, patterns, backgrounds } = clip;
    for (n, id) in backgrounds.iter().enumerate() {
        let name = if n == 0 { ddoc.next_layer_name("Layer") } else { format!("{} {}", ddoc.next_layer_name("Layer"), n + 1) };
        if let Some(l) = copies.layer_mut(*id) {
            l.name = name;
            l.locks = Default::default();
        }
    }
    // Measure in source space (the snapshot readied its shapes there): destination refresh may
    // clip a shape before it is moved.
    let bounds = copies.layers.iter().filter_map(crate::layer_multi_cmds::layer_bounds).fold(Rect::EMPTY, |a, b| a.union(&b));
    let content = if bounds.is_empty() { copies.bounds() } else { bounds };
    adapt_copies(s, &mut copies, &ddoc)?;
    let (dx, dy) = if place.center {
        let (c, d) = (center(content), center(ddoc.bounds()));
        (d[0] - c[0], d[1] - c[1])
    } else if let Some(at) = place.at {
        let c = center(content);
        (at[0] - c[0], at[1] - c[1])
    } else {
        place.offset.map_or((0.0, 0.0), |o| (o[0], o[1]))
    };
    let (dx, dy) = (dx.round() as i32, dy.round() as i32);
    translate_copies(&mut copies, &ddoc, dx, dy);
    let patterns: Vec<_> = patterns.into_iter().filter(|pat| !ddoc.patterns.iter().any(|d| d.id == pat.id)).collect();
    s.set_active(dest);
    let edited = s.edit(label, |doc, active| {
        doc.patterns.extend(patterns);
        let mut above = *active;
        let mut new = Vec::with_capacity(copies.layers.len());
        // The first copy goes where a new layer would; the rest stack above it.
        let mut place = crate::layer_place::Place::AutoEditable;
        for l in copies.layers {
            let id = crate::layer_place::insert_new(doc, above, l, place);
            place = crate::layer_place::Place::Above;
            above = Some(id);
            new.push(id);
        }
        *active = above;
        // Copied groups keep their own depth: repeated copies would otherwise nest without end.
        crate::layer_multi_cmds::check_group_depth(doc, "Copy Layers")?;
        Ok(new)
    });
    // Nothing changed: the source stays the active document.
    if edited.is_err() {
        s.set_active(restore);
    }
    let new = edited?;
    let last = new.last().copied();
    crate::layer_multi_cmds::reselect(s, new.clone(), last);
    Ok(json!({"document": dest, "layers": new.iter().map(|l| l.0).collect::<Vec<_>>(), "offset": [dx, dy]}))
}

/// `layer.copyToDocument`.
fn copy_to_document(s: &mut Session, p: &Value) -> Result<Value> {
    let dest = doc_index(s, p, "document")?.ok_or_else(|| bad("missing `document` (the destination's index)"))?;
    let src = match doc_index(s, p, "source")? {
        Some(i) => i,
        None => s.active_index().ok_or(EngineError::NoDocument)?,
    };
    if src == dest {
        return Err(bad("the destination is the source document (use layer.duplicate)"));
    }
    let from = &s.documents()[src];
    let ids = layer_ids(&from.doc, p, from.selected_layers(), CMD)?;
    let mut clip = snapshot(&from.doc, &ids)?;
    // One layer: `name` names its copy, in the same step (Duplicate Layer… › As).
    if let ([one], Some(name)) = (clip.copies.layers.as_mut_slice(), p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty())) {
        one.name = name.to_string();
        clip.backgrounds.clear();
    }
    let label = if ids.len() == 1 { "Duplicate Layer" } else { "Duplicate Layers" };
    land(s, clip, dest, src, &Placement::parse(p)?, label)
}

const COPY: &str = "layer.copyLayers";
const PASTE: &str = "layer.pasteLayers";

fn object(p: &Value, cmd: &str) -> Result<()> {
    match p {
        Value::Object(_) | Value::Null => Ok(()),
        _ => Err(EngineError::BadParams { cmd: cmd.into(), msg: "expected an object".into() }),
    }
}

/// `layer.copyLayers`: the selected layers, whole (masks, styles, type, smart objects), onto the
/// layer clipboard. The document is unchanged.
fn copy_layers(s: &mut Session, p: &Value) -> Result<Value> {
    object(p, COPY)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let ids = layer_ids(&st.doc, p, st.selected_layers(), COPY)?;
    let clip = snapshot(&st.doc, &ids)?;
    let n = clip.len();
    s.layer_clipboard = Some(clip);
    Ok(json!({"layers": n}))
}

/// `layer.pasteLayers`: the layer clipboard into the active document, above its active layer.
fn paste_layers(s: &mut Session, p: &Value) -> Result<Value> {
    object(p, PASTE)?;
    let place = Placement::parse(p)?;
    let dest = s.active_index().ok_or(EngineError::NoDocument)?;
    let clip = s.layer_clipboard.clone().ok_or_else(|| EngineError::Other("no layers have been copied".into()))?;
    let label = if clip.len() == 1 { "Paste Layer" } else { "Paste Layers" };
    land(s, clip, dest, dest, &place, label)
}

fn copy_enabled(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.ok_or("no active layer")?;
    Ok(())
}

fn paste_enabled(s: &Session) -> std::result::Result<(), String> {
    s.active().ok_or("no document open")?;
    s.layer_clipboard.as_ref().filter(|c| !c.is_empty()).map(|_| ()).ok_or_else(|| "no layers have been copied".into())
}

/// `layer.duplicate` with `"document": "new"`: Photoshop's Duplicate Layer… ›
/// Document: New. A new document the size of the source's canvas holds just the duplicated
/// layers at their canvas positions (a transparent background elsewhere); the source is
/// untouched and the new document becomes active. Mode, depth, resolution, profile,
/// patterns and global light come along so the copies render the same.
pub(crate) fn duplicate_to_new_document(s: &mut Session, p: &Value) -> Result<Value> {
    let src = s.active_index().ok_or(EngineError::NoDocument)?;
    let sdoc = s.documents().get(src).ok_or(EngineError::NoDocument)?.doc.clone();
    let ids = match crate::commands::explicit_layer(s, p)? {
        Some(id) => vec![id],
        None => s.active().ok_or(EngineError::NoDocument)?.selected_layers(),
    };
    if let Some(missing) = ids.iter().find(|id| sdoc.layer(**id).is_none()) {
        return Err(EngineError::NoLayer(*missing));
    }
    let ids = crate::layer_multi_cmds::top_level(&sdoc, &ids);
    if ids.is_empty() {
        return Err(bad("no layers to copy"));
    }
    if !layered(sdoc.mode) {
        return Err(EngineError::Other(format!("layers can't be duplicated with a {:?} document", sdoc.mode)));
    }
    let doc_name = p.get("documentName").and_then(Value::as_str).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{} copy", sdoc.name));
    let mut nd = Document::new(doc_name, sdoc.size, sdoc.mode, sdoc.depth);
    nd.resolution_dpi = sdoc.resolution_dpi;
    nd.icc_profile.clone_from(&sdoc.icc_profile);
    nd.patterns.clone_from(&sdoc.patterns);
    nd.global_light = sdoc.global_light;
    for id in &ids {
        let mut l = sdoc.layer(*id).ok_or(EngineError::NoLayer(*id))?.duplicate();
        if ids.len() == 1
            && let Some(name) = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty())
        {
            l.name = name.to_string();
        }
        nd.layers.push(l);
    }
    // Contents ids belong to a document: fresh ones keep the copies independent of the
    // source while preserving sharing within the batch (as `adapt_copies` does).
    let mut contents = HashMap::new();
    let smart_ids: Vec<_> = nd.walk().into_iter().filter_map(|(_, _, l)| matches!(l.content, LayerContent::Smart(_)).then_some(l.id)).collect();
    for id in smart_ids {
        if let Some(l) = nd.layer_mut(id)
            && let LayerContent::Smart(sm) = &mut l.content
        {
            sm.contents_id = *contents.entry(sm.contents_id).or_insert_with(SmartContentsId::fresh);
        }
    }
    let new_ids: Vec<LayerId> = nd.layers.iter().map(|l| l.id).collect();
    let index = s.add_document(nd, None);
    crate::layer_multi_cmds::reselect(s, new_ids.clone(), new_ids.last().copied());
    let mut out = json!({"document": index, "layers": new_ids.iter().map(|l| l.0).collect::<Vec<_>>()});
    if let [one] = new_ids.as_slice()
        && let Some(map) = out.as_object_mut()
    {
        map.insert("layer".into(), json!(one.0));
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CMD,
            label: "Copy Layers to Document",
            menu: &[],
            shortcut: None,
            params: r##"{"document":index (destination),"layers":[id,…]?=the source's selected layers,"source":index?=active,"center":bool=false (centre on the destination's canvas),"at":[x,y]? (centre the copies on this point),"offset":[dx,dy]?=[0,0] (from their place in the source),"name":str? (one layer: the copy's name)}"##,
            enabled,
            run: copy_to_document,
            journal: true,
        },
        CommandSpec {
            id: COPY,
            label: "Copy Layers",
            menu: &[],
            shortcut: None,
            params: r##"{"layers":[id,…]?=the selected layers} → {layers: n} (whole layers with masks, styles, type and smart objects onto the layer clipboard; the document is unchanged)"##,
            enabled: copy_enabled,
            run: copy_layers,
            journal: true,
        },
        CommandSpec {
            id: PASTE,
            label: "Paste Layers",
            menu: &[],
            shortcut: None,
            params: r##"{"center":bool=false (centre on the canvas),"at":[x,y]? (centre on this point),"offset":[dx,dy]?=[0,0] (from where they were copied)} → {document, layers, offset} (the layer clipboard above the active layer, converted to this document's colour and depth)"##,
            enabled: paste_enabled,
            run: paste_layers,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests;
