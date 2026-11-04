//! Per-layer pixel revisions (`pixelRev` in `document.inspect`, the embed API's
//! `layerPixelsChanged` event): a counter per layer that moves whenever the layer's pixels or its
//! layer mask change, whatever changed them (a brush stroke, a fill, Remove, a clear, undo, redo,
//! a background job), so a host syncing a document live can tell which layers to send.
//!
//! Nothing is recorded as edits happen: the document is compared with the one seen last time
//! someone asked, tile by tile. Tiles are copy-on-write (`photocraft-raster`), so an untouched tile
//! is the same allocation in both and the comparison is a pointer check per tile, never a pixel
//! scan. The tracker keeps the document it saw last (one more `Arc`, so a freed tile's address
//! can't come back as a false "unchanged").

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use photocraft_doc::{Document, Layer, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::Surface;

/// The most layers whose dirty rects wait to be taken ([`PixelRevs::take_changes`]); past it the
/// oldest are dropped (their revisions still move), so a session nobody drains stays small.
const MAX_PENDING: usize = 4096;

/// The most changes [`PixelRevs::dirty_since`] remembers; older ones are forgotten and asking
/// about them answers [`Dirty::Full`].
const MAX_LOG: usize = 2048;

/// One layer's pixel change since the last [`PixelRevs::take_changes`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelChange {
    pub layer: LayerId,
    /// The changed area in document pixels (tile-aligned; the union of every change since).
    pub rect: Rect,
    /// The layer's `pixelRev` after the change.
    pub rev: u64,
}

#[derive(Clone, Debug, Default)]
struct Track {
    seen: Option<Arc<Document>>,
    revs: HashMap<LayerId, u64>,
    /// The last revision handed out (revisions are unique within the document).
    counter: u64,
    pending: Vec<PixelChange>,
    /// Every change with its revision, oldest first, at most [`MAX_LOG`].
    log: VecDeque<(u64, LayerId, Rect)>,
    /// The newest revision forgotten from `log`: changes after it are all there.
    floor: u64,
}

/// What changed in a layer since a revision ([`PixelRevs::dirty_since`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dirty {
    /// Nothing: the layer is as it was at that revision.
    Clean,
    /// The area that changed (document pixels, tile-aligned), the union of every change since.
    Rect(Rect),
    /// Unknown: the revision is older than what is remembered (or not one of this document's).
    /// Read the whole layer again.
    Full,
}

/// The tracker a [`crate::DocState`] keeps. Reads (`&self`) update it, so `document.inspect` can
/// report fresh numbers without a mutable session.
#[derive(Debug, Default)]
pub struct PixelRevs(Mutex<Track>);

impl Clone for PixelRevs {
    fn clone(&self) -> Self {
        Self(Mutex::new(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()))
    }
}

impl PixelRevs {
    /// Every layer's `pixelRev` for `doc` (0 for layers unchanged since the document was first looked at).
    pub fn revisions(&self, doc: &Arc<Document>) -> HashMap<LayerId, u64> {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        sync(&mut t, doc);
        t.revs.clone()
    }

    /// One layer's `pixelRev` (`None`: no such layer).
    pub fn revision(&self, doc: &Arc<Document>, layer: LayerId) -> Option<u64> {
        doc.layer(layer)?;
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        sync(&mut t, doc);
        Some(t.revs.get(&layer).copied().unwrap_or(0))
    }

    /// Where `layer` changed after revision `since` (a `pixelRev` read earlier), with its
    /// `pixelRev` now; `None`: no such layer. Changes are remembered for the last
    /// [`MAX_LOG`] of them, across layers; before that the answer is [`Dirty::Full`].
    pub fn dirty_since(&self, doc: &Arc<Document>, layer: LayerId, since: u64) -> Option<(Dirty, u64)> {
        doc.layer(layer)?;
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        sync(&mut t, doc);
        let rev = t.revs.get(&layer).copied().unwrap_or(0);
        if since > t.counter || since < t.floor {
            return Some((Dirty::Full, rev));
        }
        let rect = t.log.iter().filter(|(r, l, _)| *r > since && *l == layer).map(|(_, _, rect)| *rect).reduce(|a, b| a.union(&b));
        Some((rect.map_or(Dirty::Clean, Dirty::Rect), rev))
    }

    /// The layers whose pixels or mask changed since the last call, with the changed area.
    pub fn take_changes(&self, doc: &Arc<Document>) -> Vec<PixelChange> {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        sync(&mut t, doc);
        std::mem::take(&mut t.pending)
    }
}

fn sync(t: &mut Track, doc: &Arc<Document>) {
    let Some(seen) = t.seen.clone() else {
        t.seen = Some(doc.clone());
        return;
    };
    if Arc::ptr_eq(&seen, doc) {
        return;
    }
    let before: HashMap<LayerId, &Layer> = seen.walk().into_iter().map(|(_, _, l)| (l.id, l)).collect();
    for (_, _, l) in doc.walk() {
        let rect = match before.get(&l.id) {
            Some(old) => changed_rect(old, l),
            // A new layer: whatever it holds is new (an empty one changes nothing yet).
            None => Some(content_rect(l)).filter(|r| !r.is_empty()),
        };
        let Some(rect) = rect else { continue };
        t.counter = t.counter.saturating_add(1);
        let rev = t.counter;
        t.revs.insert(l.id, rev);
        t.log.push_back((rev, l.id, rect));
        if t.log.len() > MAX_LOG
            && let Some((old, ..)) = t.log.pop_front()
        {
            t.floor = old;
        }
        match t.pending.iter_mut().find(|c| c.layer == l.id) {
            Some(c) => {
                c.rect = c.rect.union(&rect);
                c.rev = rev;
            }
            None => t.pending.push(PixelChange { layer: l.id, rect, rev }),
        }
    }
    if t.pending.len() > MAX_PENDING {
        let extra = t.pending.len() - MAX_PENDING;
        t.pending.drain(..extra);
    }
    // Layers that are gone: forget them (an undo that brings one back counts it as new).
    if t.revs.len() > doc.layer_count() {
        let live: std::collections::HashSet<LayerId> = doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
        t.revs.retain(|id, _| live.contains(id));
    }
    t.seen = Some(doc.clone());
}

/// Where `new`'s pixels or mask differ from `old`'s, `None` when they don't.
fn changed_rect(old: &Layer, new: &Layer) -> Option<Rect> {
    let pixels = surface_diff(old.surface(), new.surface());
    let mask = surface_diff(old.mask.as_ref().map(|m| &m.surface), new.mask.as_ref().map(|m| &m.surface));
    // A mask switched on or off changes what the layer shows everywhere it covers.
    let mask_state = (old.mask.as_ref().map(|m| (m.enabled, m.density.to_bits(), m.feather.to_bits()))
        != new.mask.as_ref().map(|m| (m.enabled, m.density.to_bits(), m.feather.to_bits())))
    .then(|| content_rect(old).union(&content_rect(new)));
    [pixels, mask, mask_state].into_iter().flatten().reduce(|a, b| a.union(&b))
}

fn content_rect(l: &Layer) -> Rect {
    let r = l.surface().map(Surface::tile_bounds).unwrap_or_default();
    match &l.mask {
        Some(m) => r.union(&m.surface.tile_bounds()),
        None => r,
    }
}

/// The tiles that differ between two surfaces (by allocation: copy-on-write keeps untouched tiles
/// shared), as one rect. A changed default pixel covers both surfaces' tiles.
fn surface_diff(a: Option<&Surface>, b: Option<&Surface>) -> Option<Rect> {
    match (a, b) {
        (None, None) => None,
        (Some(s), None) | (None, Some(s)) => Some(s.tile_bounds()),
        (Some(a), Some(b)) => {
            if a.format() != b.format() || a.default_pixel() != b.default_pixel() {
                return Some(a.tile_bounds().union(&b.tile_bounds()));
            }
            let mut r: Option<Rect> = None;
            let mut add = |t: Rect| r = Some(r.map_or(t, |r| r.union(&t)));
            for (c, tile) in a.tiles() {
                if b.tile(*c).is_none_or(|t| !Arc::ptr_eq(t, tile)) {
                    add(c.rect());
                }
            }
            for (c, _) in b.tiles() {
                if a.tile(*c).is_none() {
                    add(c.rect());
                }
            }
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::PixelFormat;
    use photocraft_doc::LayerContent;

    fn doc_with(layers: Vec<Layer>) -> Document {
        let mut d = Document::new("t", photocraft_geom::Size::new(256, 256), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8);
        d.layers = layers;
        d
    }

    #[test]
    fn a_painted_tile_moves_the_revision_and_reports_its_rect() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 10, 10), &[1.0, 0.0, 0.0, 1.0]);
        let layer = Layer::new("a", LayerContent::Raster(s));
        let id = layer.id;
        let other = Layer::new("b", LayerContent::Raster(Surface::new(PixelFormat::RGBA8)));
        let other_id = other.id;
        let d0 = Arc::new(doc_with(vec![layer, other]));
        let revs = PixelRevs::default();
        assert_eq!(revs.revision(&d0, id), Some(0));
        assert!(revs.take_changes(&d0).is_empty(), "the first look is the baseline");
        let mut d1 = (*d0).clone();
        if let Some(s) = d1.layer_mut(id).and_then(Layer::surface_mut) {
            s.fill_rect(Rect::new(300, 0, 310, 10), &[0.0, 1.0, 0.0, 1.0]);
        }
        let d1 = Arc::new(d1);
        let changes = revs.take_changes(&d1);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].layer, id);
        assert!(changes[0].rect.contains_rect(&Rect::new(300, 0, 310, 10)) && !changes[0].rect.contains_rect(&Rect::new(0, 0, 10, 10)), "{:?}", changes[0].rect);
        assert_eq!(revs.revision(&d1, id), Some(1));
        assert_eq!(revs.revision(&d1, other_id), Some(0), "an untouched layer keeps its revision");
        // Undo (the old document back) is a change too.
        assert_eq!(revs.take_changes(&d0).len(), 1);
        assert_eq!(revs.revision(&d0, id), Some(2));
        assert!(revs.take_changes(&d0).is_empty());
        // A new empty layer is no pixel change.
        let mut d2 = (*d0).clone();
        d2.layers.push(Layer::new("empty", LayerContent::Raster(Surface::new(PixelFormat::RGBA8))));
        assert!(revs.take_changes(&Arc::new(d2)).is_empty());
    }

    #[test]
    fn dirty_since_unions_the_changes_after_a_revision() {
        let layer = Layer::new("a", LayerContent::Raster(Surface::new(PixelFormat::RGBA8)));
        let id = layer.id;
        let mut d = Arc::new(doc_with(vec![layer]));
        let revs = PixelRevs::default();
        assert_eq!(revs.dirty_since(&d, id, 0), Some((Dirty::Clean, 0)));
        let paint = |d: &Arc<Document>, r: Rect| {
            let mut n = (**d).clone();
            if let Some(s) = n.layer_mut(id).and_then(Layer::surface_mut) {
                s.fill_rect(r, &[1.0, 0.0, 0.0, 1.0]);
            }
            Arc::new(n)
        };
        d = paint(&d, Rect::new(0, 0, 4, 4));
        let (_, r1) = revs.dirty_since(&d, id, 0).unwrap();
        d = paint(&d, Rect::new(300, 300, 304, 304));
        let (dirty, r2) = revs.dirty_since(&d, id, r1).unwrap();
        let Dirty::Rect(rect) = dirty else { panic!("{dirty:?}") };
        assert!(rect.contains_rect(&Rect::new(300, 300, 304, 304)) && !rect.contains_rect(&Rect::new(0, 0, 4, 4)), "{rect:?}");
        let Dirty::Rect(all) = revs.dirty_since(&d, id, 0).unwrap().0 else { panic!() };
        assert!(all.contains_rect(&Rect::new(0, 0, 304, 304)));
        assert_eq!(revs.dirty_since(&d, id, r2), Some((Dirty::Clean, r2)));
        assert_eq!(revs.dirty_since(&d, id, r2 + 7).map(|d| d.0), Some(Dirty::Full), "a revision from elsewhere");
        assert_eq!(revs.dirty_since(&d, LayerId(u64::MAX), 0), None);
        // Past the remembered changes: the whole layer.
        for i in 0..MAX_LOG as i32 + 1 {
            d = paint(&d, Rect::new(i % 4, 0, i % 4 + 1, 1));
            revs.revisions(&d);
        }
        assert_eq!(revs.dirty_since(&d, id, r2).map(|d| d.0), Some(Dirty::Full));
    }

    #[test]
    fn mask_edits_count_and_renames_do_not() {
        let layer = Layer::new("a", LayerContent::Raster(Surface::new(PixelFormat::RGBA8)));
        let id = layer.id;
        let d0 = Arc::new(doc_with(vec![layer]));
        let revs = PixelRevs::default();
        revs.revisions(&d0);
        let mut d1 = (*d0).clone();
        if let Some(l) = d1.layer_mut(id) {
            l.name = "renamed".into();
        }
        let d1 = Arc::new(d1);
        assert!(revs.take_changes(&d1).is_empty());
        let mut d2 = (*d1).clone();
        if let Some(l) = d2.layer_mut(id) {
            let mut m = photocraft_doc::LayerMask::reveal_all();
            m.surface.fill_rect(Rect::new(0, 0, 4, 4), &[0.0]);
            l.mask = Some(m);
        }
        let d2 = Arc::new(d2);
        let c = revs.take_changes(&d2);
        assert_eq!(c.len(), 1);
        assert_eq!(revs.revision(&d2, id), Some(c[0].rev));
    }
}
