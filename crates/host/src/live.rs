//! Live sync (**experimental**): several people editing one document, each in their own process,
//! kept in step through the host's server.
//!
//! - [`Host::live_start`] makes a document live, naming this peer (`origin`, unique per person
//!   and session).
//! - The person's edits (and the host's own calls) become [`Envelope`]s: take them with
//!   [`Host::live_take_local`] after [`Host::poll`] and send them to the server.
//! - Envelopes from the server go to [`Host::live_apply`]: applied with no undo step (the
//!   person's Undo and Redo keep them, `history: "none"`), per layer last writer wins.
//!
//! Layers are addressed by [`LayerUid`] (the PSD layer id: the same for everyone who opened the
//! same file). Each envelope carries a Lamport `stamp` and its `origin`; for each key (a layer's
//! pixels, its mask, its settings and content, a group's order) the envelope with the highest
//! `(stamp, origin)` wins, and an older one arriving late is ignored ([`Applied::Stale`]). Local
//! edits take part: a remote op older than this peer's last change to the same key is dropped.
//!
//! What an [`Op`] carries:
//! - [`Op::Pixels`]: a rect of a pixel layer's pixels or its layer mask, as PNG (replaces that
//!   rect, transparency included).
//! - [`Op::Layer`]: everything about one layer but its pixels (a pixel layer's) or its children
//!   (a group's): settings, effects, mask switches, type, shape, adjustment, smart object. A
//!   one-layer `.pcraft`. Also adds the layer when the receiver lacks it, at `parent` / `above`.
//! - [`Op::Removed`], [`Op::Reordered`]: deletes and stacking order.
//! - [`Op::Command`]: any engine command, applied as is (no last-writer check).
//!
//! The serialised form (serde JSON, `op` tag, camelCase, byte fields as base64 strings) is
//! what the host sends; it may still change while this module is experimental.

use std::collections::{HashMap, HashSet};

use photocraft_engine::HistoryMode;
use photocraft_engine::doc::{Document, Layer, LayerContent, LayerId as DocLayerId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result, bad};
use crate::events::{Event, Origin};
use crate::{DocumentId, Host, LayerId, LayerUid};

/// One change to a live document. See the [module docs](self).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Op {
    /// A rect of a pixel layer's pixels (or its layer mask) as PNG, top-left at `rect`'s
    /// origin; replaces that rect.
    Pixels {
        /// The layer.
        layer: LayerUid,
        /// `[x, y, width, height]`, document pixels.
        rect: [i32; 4],
        /// The layer mask (gray) instead of the pixels.
        mask: bool,
        /// The PNG.
        #[serde(with = "b64")]
        png: Vec<u8>,
    },
    /// One layer's settings and content (not a pixel layer's pixels, not a group's children),
    /// as a one-layer `.pcraft`. Adds the layer when missing: in group `parent` (`None`: the
    /// top level), right above `above` (`None`: at the bottom).
    Layer {
        /// The layer.
        layer: LayerUid,
        /// Its group.
        parent: Option<LayerUid>,
        /// The layer right below it.
        above: Option<LayerUid>,
        /// The one-layer `.pcraft`.
        #[serde(with = "b64")]
        pcraft: Vec<u8>,
    },
    /// The layer was deleted.
    Removed {
        /// The layer.
        layer: LayerUid,
    },
    /// A group's (`None`: the top level) children, top to bottom; layers the receiver lacks are
    /// skipped, ones it has that aren't listed stay where they are relative to the others.
    Reordered {
        /// The group.
        group: Option<LayerUid>,
        /// Its children, top to bottom.
        layers: Vec<LayerUid>,
    },
    /// An engine command (control-protocol id and params), applied as is with no undo step.
    Command {
        /// The command id.
        command: String,
        /// Its params.
        params: Value,
    },
}

/// An [`Op`] with its ordering: what peers exchange.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    /// The change.
    pub op: Op,
    /// Lamport clock: higher wins.
    pub stamp: u64,
    /// The peer that made it (ties on `stamp` go to the greater `origin`).
    pub origin: String,
}

/// What [`Host::live_apply`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// Applied.
    Applied,
    /// A newer change to the same key was seen already; nothing changed.
    Stale,
    /// Nothing to apply to (the layer isn't in this document); nothing changed.
    Skipped(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    Pixels(u32),
    Mask(u32),
    Layer(u32),
    Order(Option<u32>),
}

/// A live document's bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct LiveState {
    origin: String,
    clock: u64,
    last: HashMap<Key, (u64, String)>,
    pixels: HashMap<u32, [i32; 4]>,
    layers: Vec<u32>,
    removed: Vec<u32>,
    orders: HashSet<Option<LayerId>>,
}

impl LiveState {
    fn wins(&self, key: Key, stamp: u64, origin: &str) -> bool {
        self.last.get(&key).is_none_or(|(s, o)| (stamp, origin) > (*s, o.as_str()))
    }

    fn note(&mut self, key: Key, stamp: u64, origin: &str) {
        self.last.insert(key, (stamp, origin.to_string()));
    }
}

fn union(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
    let (x1, y1) = (a[0].saturating_add(a[2]).max(b[0].saturating_add(b[2])), a[1].saturating_add(a[3]).max(b[1].saturating_add(b[3])));
    [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)]
}

/// Remember a local change of a live document, for [`Host::live_take_local`].
pub(crate) fn note_local(live: &mut HashMap<DocumentId, LiveState>, e: &Event) {
    match e {
        Event::LayerPixelsChanged { document, uid: Some(uid), rect, origin: Origin::Local, .. } => {
            if let Some(st) = live.get_mut(document) {
                let r = st.pixels.get(&uid.0).map_or(*rect, |r| union(*r, *rect));
                st.pixels.insert(uid.0, r);
            }
        }
        Event::LayersChanged { document, uid: Some(uid), what, origin: Origin::Local, .. } => {
            if let Some(st) = live.get_mut(document) {
                if what.iter().any(|w| w == "removed") {
                    st.removed.push(uid.0);
                    st.layers.retain(|l| *l != uid.0);
                    st.pixels.remove(&uid.0);
                } else if !st.layers.contains(&uid.0) {
                    st.layers.push(uid.0);
                }
            }
        }
        Event::LayersReordered { document, group, origin: Origin::Local, .. } => {
            if let Some(st) = live.get_mut(document) {
                st.orders.insert(*group);
            }
        }
        _ => {}
    }
}

fn find_uid(doc: &Document, uid: LayerUid) -> Option<&Layer> {
    doc.walk().into_iter().map(|(_, _, l)| l).find(|l| l.psd_id == Some(uid.0))
}

/// The children of group `parent` (`None`: the top level).
fn siblings_mut(doc: &mut Document, parent: Option<DocLayerId>) -> Option<&mut Vec<Layer>> {
    match parent {
        None => Some(&mut doc.layers),
        Some(g) => doc.layer_mut(g)?.children_mut(),
    }
}

/// Where layer `id` sits: its group (`None`: the top level) and its index there.
fn place_of(doc: &Document, id: DocLayerId) -> Option<(Option<DocLayerId>, usize)> {
    let path = doc.path_of(id)?;
    let (index, parent) = path.split_last()?;
    let group = if parent.is_empty() { None } else { Some(doc.layer_at(parent)?.id) };
    Some((group, *index))
}

/// One layer's [`Op::Layer`] payload: a `.pcraft` holding that layer alone, without a pixel
/// layer's pixels or a group's children.
fn encode_layer(doc: &Document, layer: &Layer) -> Result<Vec<u8>> {
    let mut l = layer.clone();
    match &mut l.content {
        LayerContent::Raster(s) => *s = photocraft_raster::Surface::new(s.format()),
        LayerContent::Group(g) => g.children.clear(),
        _ => {}
    }
    let mut one = Document::new("layer", doc.size, doc.mode, doc.depth);
    one.layers = vec![l];
    photocraft_format::save_to_bytes(&one, &photocraft_format::SaveOptions::default()).map_err(|e| Error::File(e.to_string()))
}

fn decode_layer(doc: &Document, bytes: &[u8]) -> Result<Layer> {
    let one = photocraft_format::load_from_bytes(bytes).map_err(|e| Error::File(format!("live layer: {e}")))?;
    if one.mode != doc.mode || one.depth != doc.depth {
        return Err(Error::File(format!("live layer: {:?} {:?} doesn't match the document's {:?} {:?}", one.mode, one.depth, doc.mode, doc.depth)));
    }
    one.layers.into_iter().next().ok_or_else(|| Error::File("live layer: the payload holds no layer".into()))
}

/// Put `incoming` (uid `uid`) into `doc`: over the layer with that uid (keeping its session id,
/// a pixel layer's pixels, a group's children, a mask's pixels), or added at `parent`/`above`.
fn put_layer(doc: &mut Document, uid: LayerUid, mut incoming: Layer, parent: Option<LayerUid>, above: Option<LayerUid>) -> std::result::Result<(), String> {
    incoming.psd_id = Some(uid.0);
    if let Some(local) = find_uid(doc, uid) {
        let id = local.id;
        incoming.id = id;
        match (&mut incoming.content, &local.content) {
            (LayerContent::Raster(s), LayerContent::Raster(mine)) => *s = mine.clone(),
            (LayerContent::Group(g), LayerContent::Group(mine)) => g.children = mine.children.clone(),
            _ => {}
        }
        if let (Some(m), Some(mine)) = (incoming.mask.as_mut(), local.mask.as_ref()) {
            m.surface = mine.surface.clone();
        }
        let slot = doc.layer_mut(id).ok_or("the layer went away")?;
        *slot = incoming;
        return Ok(());
    }
    let group = match parent {
        None => None,
        Some(p) => Some(find_uid(doc, p).filter(|l| l.children().is_some()).ok_or("its group isn't in this document")?.id),
    };
    let below = above.and_then(|a| find_uid(doc, a)).map(|l| l.id);
    incoming.id = DocLayerId::fresh();
    let list = siblings_mut(doc, group).ok_or("its group isn't in this document")?;
    // Layers are stored bottom to top: right above `below`, else at the bottom.
    let at = below.and_then(|b| list.iter().position(|l| l.id == b)).map_or(0, |i| i + 1);
    list.insert(at.min(list.len()), incoming);
    Ok(())
}

fn remove_layer(doc: &mut Document, uid: LayerUid) -> bool {
    let Some(id) = find_uid(doc, uid).map(|l| l.id) else { return false };
    let Some((group, index)) = place_of(doc, id) else { return false };
    let Some(list) = siblings_mut(doc, group) else { return false };
    if index < list.len() {
        list.remove(index);
        return true;
    }
    false
}

/// Order group `group`'s children as `layers` (top to bottom); others keep their slots.
fn reorder(doc: &mut Document, group: Option<LayerUid>, layers: &[LayerUid]) -> bool {
    let gid = match group {
        None => None,
        Some(g) => match find_uid(doc, g) {
            Some(l) => Some(l.id),
            None => return false,
        },
    };
    let Some(list) = siblings_mut(doc, gid) else { return false };
    // Bottom to top, as stored.
    let wanted: Vec<u32> = layers.iter().rev().map(|u| u.0).collect();
    let slots: Vec<usize> = list.iter().enumerate().filter(|(_, l)| l.psd_id.is_some_and(|u| wanted.contains(&u))).map(|(i, _)| i).collect();
    let mut moving: Vec<Layer> = Vec::with_capacity(slots.len());
    for i in slots.iter().rev() {
        moving.push(list.remove(*i));
    }
    moving.sort_by_key(|l| l.psd_id.and_then(|u| wanted.iter().position(|w| *w == u)).unwrap_or(usize::MAX));
    for (slot, l) in slots.iter().zip(moving) {
        list.insert((*slot).min(list.len()), l);
    }
    true
}

impl Host {
    /// Make `document` live as peer `origin` (unique per person and session): its local changes
    /// are recorded for [`Self::live_take_local`] from the next [`Self::poll`] on.
    pub fn live_start(&mut self, document: DocumentId, origin: &str) -> Result<()> {
        if !self.session().documents().iter().any(|st| st.doc.id.0 == document.0) {
            return Err(Error::NoDocument(document));
        }
        if origin.trim().is_empty() {
            return Err(bad("liveStart", "origin must name this peer"));
        }
        // Changes made before going live are not ops.
        self.collect(Origin::Local);
        self.live.insert(document, LiveState { origin: origin.to_string(), ..LiveState::default() });
        Ok(())
    }

    /// Stop live sync on `document` (its pending local ops are dropped).
    pub fn live_stop(&mut self, document: DocumentId) {
        self.live.remove(&document);
    }

    /// Whether `document` is live.
    pub fn is_live(&self, document: DocumentId) -> bool {
        self.live.contains_key(&document)
    }

    /// Apply a collaborator's envelope to live `document`: no undo step, per-key last writer
    /// wins (see the [module docs](self)). The change is reported at the next [`Self::poll`] with
    /// [`Origin::Remote`] and is not sent back out.
    pub fn live_apply(&mut self, document: DocumentId, env: &Envelope) -> Result<Applied> {
        let key = match &env.op {
            Op::Pixels { layer, mask: false, .. } => Some(Key::Pixels(layer.0)),
            Op::Pixels { layer, mask: true, .. } => Some(Key::Mask(layer.0)),
            Op::Layer { layer, .. } | Op::Removed { layer } => Some(Key::Layer(layer.0)),
            Op::Reordered { group, .. } => Some(Key::Order(group.map(|g| g.0))),
            Op::Command { .. } => None,
        };
        {
            let st = self.live.get_mut(&document).ok_or_else(|| bad("liveApply", format!("document {} is not live (live_start)", document.0)))?;
            st.clock = st.clock.max(env.stamp);
            if let Some(k) = key
                && !st.wins(k, env.stamp, &env.origin)
            {
                return Ok(Applied::Stale);
            }
        }
        // Local changes so far are the person's; what follows is the collaborator's.
        self.collect(Origin::Local);
        let r = self.apply_op(document, &env.op);
        self.collect(Origin::Remote);
        let applied = r?;
        if let (Some(k), Some(st), Applied::Applied) = (key, self.live.get_mut(&document), &applied) {
            st.note(k, env.stamp, &env.origin);
            if let Op::Removed { layer } = &env.op {
                for k in [Key::Pixels(layer.0), Key::Mask(layer.0)] {
                    st.note(k, env.stamp, &env.origin);
                }
            }
        }
        Ok(applied)
    }

    fn apply_op(&mut self, document: DocumentId, op: &Op) -> Result<Applied> {
        let silent = crate::ExecOptions::silent(document);
        let doc = self.session().documents().iter().find(|st| st.doc.id.0 == document.0).map(|st| st.doc.clone()).ok_or(Error::NoDocument(document))?;
        match op {
            Op::Command { command, params } => {
                self.execute_with(command, params.clone(), silent)?;
                Ok(Applied::Applied)
            }
            Op::Pixels { layer, rect, mask, png } => {
                let Some(id) = find_uid(&doc, *layer).map(|l| l.id) else { return Ok(Applied::Skipped(format!("no layer with uid {}", layer.0))) };
                let png = photocraft_paint::tile::b64_encode(png);
                let p = json!({"layer": id.0, "png": png, "x": rect[0], "y": rect[1], "mode": "replace"});
                self.execute_with(if *mask { "layer.layerMask.set" } else { "layer.putPixels" }, p, silent)?;
                Ok(Applied::Applied)
            }
            Op::Layer { layer, parent, above, pcraft } => {
                let incoming = decode_layer(&doc, pcraft)?;
                let (uid, parent, above) = (*layer, *parent, *above);
                self.edit_silently(document, "Live Change", move |d| put_layer(d, uid, incoming, parent, above))?;
                Ok(Applied::Applied)
            }
            Op::Removed { layer } => {
                if find_uid(&doc, *layer).is_none() {
                    return Ok(Applied::Skipped(format!("no layer with uid {}", layer.0)));
                }
                let uid = *layer;
                self.edit_silently(document, "Live Delete", move |d| if remove_layer(d, uid) { Ok(()) } else { Err("the layer went away".into()) })?;
                Ok(Applied::Applied)
            }
            Op::Reordered { group, layers } => {
                let (group, layers) = (*group, layers.clone());
                self.edit_silently(document, "Live Order", move |d| if reorder(d, group, &layers) { Ok(()) } else { Err("its group isn't in this document".into()) })?;
                Ok(Applied::Applied)
            }
        }
    }

    /// Run `f` on `document` with no undo step (the scoped `history: "none"` machinery).
    fn edit_silently(&mut self, document: DocumentId, label: &str, f: impl FnOnce(&mut Document) -> std::result::Result<(), String>) -> Result<()> {
        let s = self.session_mut();
        let scope = s.begin_scope(Some(document.into()), HistoryMode::None)?;
        let r = s.edit(label, |d, _| f(d).map_err(photocraft_engine::EngineError::Other));
        s.end_scope(scope);
        Ok(r?)
    }

    /// The ops the person's edits on live `document` made since the last call (call after
    /// [`Self::poll`]), stamped and ready to send. Remote changes are never included.
    pub fn live_take_local(&mut self, document: DocumentId) -> Result<Vec<Envelope>> {
        let st = self.live.get_mut(&document).ok_or_else(|| bad("liveTakeLocal", format!("document {} is not live (live_start)", document.0)))?;
        let (pixels, layers, removed, orders) = (std::mem::take(&mut st.pixels), std::mem::take(&mut st.layers), std::mem::take(&mut st.removed), std::mem::take(&mut st.orders));
        let doc = self.session().documents().iter().find(|s| s.doc.id.0 == document.0).map(|s| s.doc.clone()).ok_or(Error::NoDocument(document))?;
        let mut ops: Vec<(Key, Op)> = Vec::new();
        for uid in removed {
            ops.push((Key::Layer(uid), Op::Removed { layer: LayerUid(uid) }));
        }
        for uid in layers {
            let Some(l) = find_uid(&doc, LayerUid(uid)) else { continue };
            let (group, index) = place_of(&doc, l.id).unwrap_or((None, 0));
            let parent = group.and_then(|g| doc.layer(g)).and_then(|g| g.psd_id).map(LayerUid);
            let above = index.checked_sub(1).and_then(|i| {
                let list = match group {
                    None => Some(doc.layers.as_slice()),
                    Some(g) => doc.layer(g).and_then(|g| g.children()),
                };
                list?.get(i)?.psd_id.map(LayerUid)
            });
            ops.push((Key::Layer(uid), Op::Layer { layer: LayerUid(uid), parent, above, pcraft: encode_layer(&doc, l)? }));
        }
        for (uid, rect) in pixels {
            let Some(l) = find_uid(&doc, LayerUid(uid)) else { continue };
            // Type, shapes and smart objects draw their own pixels: their op is `Layer`.
            if !matches!(l.content, LayerContent::Raster(_)) {
                continue;
            }
            let id = LayerId::from(l.id);
            let has_mask = l.mask.is_some();
            for mask in [false, true] {
                if mask && !has_mask {
                    continue;
                }
                let px = self.get_pixels(document, id, crate::GetPixels { rect: Some(rect), mask, raw: false })?;
                ops.push((if mask { Key::Mask(uid) } else { Key::Pixels(uid) }, Op::Pixels { layer: LayerUid(uid), rect: px.rect, mask, png: px.data }));
            }
        }
        for group in orders {
            let gid = group.map(Into::into);
            let list = match gid {
                None => Some(doc.layers.as_slice()),
                Some(g) => doc.layer(g).and_then(|g| g.children()),
            };
            let Some(list) = list else { continue };
            let guid = match gid {
                None => None,
                Some(g) => match doc.layer(g).and_then(|l| l.psd_id) {
                    Some(u) => Some(u),
                    None => continue,
                },
            };
            let layers = list.iter().rev().filter_map(|l| l.psd_id.map(LayerUid)).collect();
            ops.push((Key::Order(guid), Op::Reordered { group: guid.map(LayerUid), layers }));
        }
        let st = self.live.get_mut(&document).ok_or_else(|| bad("liveTakeLocal", "the document stopped being live"))?;
        let origin = st.origin.clone();
        Ok(ops
            .into_iter()
            .map(|(key, op)| {
                st.clock = st.clock.saturating_add(1);
                let stamp = st.clock;
                st.note(key, stamp, &origin);
                Envelope { op, stamp, origin: origin.clone() }
            })
            .collect())
    }

    /// The uid of `layer` in `document` as last polled (also for a layer deleted since).
    pub fn layer_uid(&self, document: DocumentId, layer: LayerId) -> Option<LayerUid> {
        self.uid_of(document, layer)
    }
}

/// Byte fields as base64 strings.
mod b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&photocraft_paint::tile::b64_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        photocraft_paint::tile::b64_decode(&s).ok_or_else(|| serde::de::Error::custom("not base64"))
    }
}
