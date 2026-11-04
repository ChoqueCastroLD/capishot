//! What a host compares from one look at a document to the next to tell its own code what changed:
//! every layer's settings ([`layer_props`]), the stacking order ([`layer_order`]) and the
//! selection's bounds ([`selection_bounds`]), plus the diffs between two looks
//! ([`layer_changes`], [`reordered_groups`]). Shared by the web embed's events (`embed_api_core.rs`)
//! and the in-process host API (`capishot-host`), so both report the same changes. Pixel changes
//! come from [`crate::pixel_revs`].

use photocraft_doc::{BlendMode, Document, Effects, LabelColor, Layer, LayerContent, Locks, TextLayer, VectorMask};

/// A layer's settings and its type, shape or adjustment (not its pixels: `pixel_revs` says
/// those), as `layerChanged` compares them.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerProps {
    pub id: u64,
    pub parent: Option<u64>,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub fill_opacity: f32,
    pub blend: BlendMode,
    pub clipped: bool,
    pub locks: Locks,
    pub label: LabelColor,
    pub effects: Effects,
    /// The layer mask's switches (its pixels count as pixels).
    pub mask: Option<(bool, f32, f32)>,
    pub vector_mask: Option<VectorMask>,
    /// A type or shape layer's model without its rendered pixels, an adjustment's or fill's
    /// settings; `None` for pixel layers, groups and smart objects.
    pub content: Option<LayerContent>,
}

/// Every layer's [`LayerProps`], in the document's walk order (parents before children).
pub fn layer_props(doc: &Document) -> Vec<LayerProps> {
    fn rec(layers: &[Layer], parent: Option<u64>, out: &mut Vec<LayerProps>) {
        for l in layers {
            let content = match &l.content {
                LayerContent::Text(t) => Some(LayerContent::Text(TextLayer { cache: None, psd_raw: None, ..t.clone() })),
                LayerContent::Shape(sh) => {
                    let mut sh = sh.clone();
                    sh.cache = None;
                    sh.psd_raw = None;
                    Some(LayerContent::Shape(sh))
                }
                c @ (LayerContent::Adjustment(_) | LayerContent::Fill(_)) => Some(c.clone()),
                _ => None,
            };
            out.push(LayerProps {
                id: l.id.0,
                parent,
                name: l.name.clone(),
                visible: l.visible,
                opacity: l.opacity,
                fill_opacity: l.fill_opacity,
                blend: l.blend,
                clipped: l.clipped,
                locks: l.locks,
                label: l.label,
                effects: l.effects.clone(),
                mask: l.mask.as_ref().map(|m| (m.enabled, m.density, m.feather)),
                vector_mask: l.vector_mask.clone(),
                content,
            });
            if let Some(ch) = l.children() {
                rec(ch, Some(l.id.0), out);
            }
        }
    }
    let mut out = Vec::new();
    rec(&doc.layers, None, &mut out);
    out
}

/// One layer's change between two [`layer_props`] looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerChange {
    /// The layer's session id.
    pub layer: u64,
    /// What changed: `added`, `removed`, or any of `name`, `visible`, `opacity`, `fillOpacity`,
    /// `blend`, `clipped`, `locks`, `label`, `effects`, `mask`, `vectorMask`, `parent`, `content`
    /// (the text and its style, a shape, an adjustment's or fill's settings).
    pub what: Vec<&'static str>,
}

/// Every layer whose settings changed, was added or was removed from `prev` to `next`: changed
/// and added ones in `next`'s order, then the removed ones.
pub fn layer_changes(prev: &[LayerProps], next: &[LayerProps]) -> Vec<LayerChange> {
    let mut out = Vec::new();
    for n in next {
        let Some(p) = prev.iter().find(|p| p.id == n.id) else {
            out.push(LayerChange { layer: n.id, what: vec!["added"] });
            continue;
        };
        if p == n {
            continue;
        }
        let checks: [(&'static str, bool); 13] = [
            ("name", p.name != n.name),
            ("visible", p.visible != n.visible),
            ("opacity", p.opacity != n.opacity),
            ("fillOpacity", p.fill_opacity != n.fill_opacity),
            ("blend", p.blend != n.blend),
            ("clipped", p.clipped != n.clipped),
            ("locks", p.locks != n.locks),
            ("label", p.label != n.label),
            ("effects", p.effects != n.effects),
            ("mask", p.mask != n.mask),
            ("vectorMask", p.vector_mask != n.vector_mask),
            ("parent", p.parent != n.parent),
            ("content", p.content != n.content),
        ];
        let what: Vec<&'static str> = checks.iter().filter(|(_, changed)| *changed).map(|(w, _)| *w).collect();
        if !what.is_empty() {
            out.push(LayerChange { layer: n.id, what });
        }
    }
    for p in prev.iter().filter(|p| !next.iter().any(|n| n.id == p.id)) {
        out.push(LayerChange { layer: p.id, what: vec!["removed"] });
    }
    out
}

/// Each group's (`None`: the document's top level) child layer ids, top to bottom.
pub type LayerOrder = Vec<(Option<u64>, Vec<u64>)>;

/// The stacking order of `doc`'s layers, group by group.
pub fn layer_order(doc: &Document) -> LayerOrder {
    fn rec(layers: &[Layer], parent: Option<u64>, out: &mut LayerOrder) {
        out.push((parent, layers.iter().rev().map(|l| l.id.0).collect()));
        for l in layers {
            if let Some(ch) = l.children() {
                rec(ch, Some(l.id.0), out);
            }
        }
    }
    let mut out = Vec::new();
    rec(&doc.layers, None, &mut out);
    out
}

/// Every group (`None`: the top level) whose layers kept in it changed order from `prev` to
/// `next`, with its children now, top to bottom. Layers added, deleted or moved to another group
/// alone don't count.
pub fn reordered_groups(prev: &LayerOrder, next: &LayerOrder) -> Vec<(Option<u64>, Vec<u64>)> {
    let mut out = Vec::new();
    for (group, now) in next {
        let Some((_, was)) = prev.iter().find(|(g, _)| g == group) else { continue };
        let kept_now: Vec<u64> = now.iter().copied().filter(|id| was.contains(id)).collect();
        let kept_was: Vec<u64> = was.iter().copied().filter(|id| now.contains(id)).collect();
        if kept_now != kept_was {
            out.push((*group, now.clone()));
        }
    }
    out
}

/// The selection's bounds `[x, y, width, height]` (document pixels), `None` without a selection
/// (or an empty one). Walks the selection's tiles: callers cache it per revision.
pub fn selection_bounds(doc: &Document) -> Option<[i32; 4]> {
    doc.selection.as_ref().map(|s| s.content_bounds()).filter(|r| !r.is_empty()).map(|r| [r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;

    fn doc_with_two_layers() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
        s.execute("layer.new.layer", json!({"name": "A"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "B"})).unwrap();
        s
    }

    #[test]
    fn a_rename_and_a_new_layer_are_changes() {
        let mut s = doc_with_two_layers();
        let before = layer_props(&s.active().unwrap().doc);
        s.execute("layer.setProps", json!({"name": "B2"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "C"})).unwrap();
        let after = layer_props(&s.active().unwrap().doc);
        let changes = layer_changes(&before, &after);
        assert!(changes.iter().any(|c| c.what == ["name"]), "{changes:?}");
        assert!(changes.iter().any(|c| c.what == ["added"]), "{changes:?}");
        assert!(layer_changes(&after, &after).is_empty());
    }

    #[test]
    fn moving_a_layer_reorders_its_group() {
        let mut s = doc_with_two_layers();
        let before = layer_order(&s.active().unwrap().doc);
        s.execute("layer.arrange.sendBackward", json!({})).unwrap();
        let after = layer_order(&s.active().unwrap().doc);
        let groups = reordered_groups(&before, &after);
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].0, None);
        assert!(reordered_groups(&after, &after).is_empty());
    }

    #[test]
    fn no_selection_has_no_bounds() {
        let mut s = doc_with_two_layers();
        assert_eq!(selection_bounds(&s.active().unwrap().doc), None);
        s.execute("select.all", json!({})).unwrap();
        assert_eq!(selection_bounds(&s.active().unwrap().doc), Some([0, 0, 32, 32]));
    }
}
