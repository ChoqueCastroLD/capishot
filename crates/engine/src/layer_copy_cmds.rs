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

use std::collections::HashMap;

use photocraft_color::ColorMode;
use photocraft_doc::{Document, LayerContent, LayerId, SmartContentsId};
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
fn layered(mode: ColorMode) -> bool {
    matches!(mode, ColorMode::Rgb | ColorMode::Grayscale | ColorMode::Cmyk | ColorMode::Lab)
}

fn center(r: Rect) -> [f64; 2] {
    [(f64::from(r.x0) + f64::from(r.x1)) / 2.0, (f64::from(r.y0) + f64::from(r.y1)) / 2.0]
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
        if crate::extra_cmds::is_background(src_layer) && sdoc.layers.first().is_some_and(|b| b.id == *id) {
            backgrounds.push(l.id);
        }
        copies.layers.push(l);
    }
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
    // Contents IDs belong to a document. Preserve sharing within this batch, without joining
    // pre-existing objects in the destination (including another opening of the same file).
    let mut contents = HashMap::new();
    let smart_ids: Vec<_> = copies.walk().into_iter().filter_map(|(_, _, l)| matches!(l.content, LayerContent::Smart(_)).then_some(l.id)).collect();
    for id in smart_ids {
        if let Some(l) = copies.layer_mut(id)
            && let LayerContent::Smart(sm) = &mut l.content
        {
            sm.contents_id = *contents.entry(sm.contents_id).or_insert_with(SmartContentsId::fresh);
        }
    }
    if (copies.mode, &copies.icc_profile) != (ddoc.mode, &ddoc.icc_profile) {
        let profile = crate::color_cmds::document_profile(&ddoc);
        crate::color_cmds::convert_document(&mut copies, &profile, s.color.settings.intent(), s.color.settings.bpc)?;
    }
    if copies.depth != ddoc.depth {
        crate::image_cmds::for_each_surface(&mut copies.layers, true, &mut |surf, _| {
            let f = surf.format().with_sample(ddoc.depth);
            *surf = surf.convert(f);
        });
    }
    let bounds = copies.layers.iter().filter_map(crate::layer_multi_cmds::layer_bounds).fold(Rect::EMPTY, |a, b| a.union(&b));
    let content = if bounds.is_empty() { copies.bounds() } else { bounds };
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
    if dx != 0 || dy != 0 {
        let snapshot = copies.clone();
        for l in &mut copies.layers {
            crate::commands::translate_layer(&snapshot, l, dx, dy);
            crate::vector_cmds::translate_vectors(&snapshot, l, f64::from(dx), f64::from(dy));
        }
    }
    let patterns: Vec<_> = patterns.into_iter().filter(|pat| !ddoc.patterns.iter().any(|d| d.id == pat.id)).collect();
    s.set_active(dest);
    let edited = s.edit(label, |doc, active| {
        doc.patterns.extend(patterns);
        let mut above = *active;
        let mut new = Vec::with_capacity(copies.layers.len());
        for l in copies.layers {
            let id = doc.insert_above(above, l);
            above = Some(id);
            new.push(id);
        }
        *active = above;
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
    let clip = snapshot(&from.doc, &ids)?;
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

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CMD,
            label: "Copy Layers to Document",
            menu: &[],
            shortcut: None,
            params: r##"{"document":index (destination),"layers":[id,…]?=the source's selected layers,"source":index?=active,"center":bool=false (centre on the destination's canvas),"at":[x,y]? (centre the copies on this point),"offset":[dx,dy]?=[0,0] (from their place in the source)}"##,
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
