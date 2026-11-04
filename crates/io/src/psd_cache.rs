//! Per-document PSD/PSB export cache: an incremental save re-encodes only what changed since the
//! last one, and writes the same bytes a full save does.
//!
//! * Layer channels: each layer's encoded pixel channels and mask channel are kept with the
//!   surface they were encoded from. Tiles are copy-on-write (`photocraft-raster`), so an
//!   untouched layer still holds the same tile allocations and a pointer check per tile proves its
//!   pixels are the ones encoded; the cache holds those tiles, so a freed tile's address can't
//!   come back as a false match. Encoding is a pure function of the pixels, the document's pixel
//!   format and the PSD version, which are part of the match.
//! * The merged image of documents photocraft-compose renders exactly as psd-compiler would
//!   (`psdc::compose_agrees`: plain pixel and type layers, per-pixel blending, no effects, no
//!   feathered masks): the last composite is kept with a snapshot of the layers, and when only
//!   layer pixels changed, only the changed tiles' area is rendered again.

use std::collections::HashMap;
use std::sync::Arc;

// Background saves encode on workers while the page's thread may save (spins there).
use wasm_sync::Mutex;

use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_psd::layer::ChannelData;
use photocraft_psd::{Rect as PsdRect, Version};
use photocraft_raster::Surface;

/// Shared PSD export cache (see the module docs). Clones share it; dropping the last clone
/// releases it.
#[derive(Clone, Default)]
pub struct PsdExportCache(Arc<Mutex<Inner>>);

impl std::fmt::Debug for PsdExportCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PsdExportCache")
    }
}

impl PartialEq for PsdExportCache {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What the last save reused and encoded, for tests and benchmarks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PsdCacheStats {
    /// Layer pixel and mask channel sets taken from the cache.
    pub reused: usize,
    /// Channel sets encoded (and stored).
    pub encoded: usize,
    /// The merged image's area rendered again, in pixels (`None`: not through the cache).
    pub merged_rendered: Option<u64>,
}

#[derive(Default)]
struct Inner {
    /// The last save's channel sets; a save replaces them with its own (layers gone are dropped).
    channels: HashMap<Key, Encoded>,
    merged: Option<MergedCache>,
    stats: PsdCacheStats,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Part {
    Pixels,
    Mask,
}

type Key = (LayerId, Part);

/// One layer's encoded pixels (or mask), and what they were encoded from.
#[derive(Clone)]
pub(crate) struct Encoded {
    source: Surface,
    fmt: PixelFormat,
    version: Version,
    pub rect: PsdRect,
    pub channels: Vec<ChannelData>,
    pub warnings: Vec<String>,
    /// A mask's content bounds and default value.
    pub mask: Option<(Rect, f32)>,
}

impl Encoded {
    pub(crate) fn new(source: &Surface, fmt: PixelFormat, version: Version, rect: PsdRect, channels: Vec<ChannelData>, warnings: Vec<String>) -> Self {
        Self { source: source.clone(), fmt, version, rect, channels, warnings, mask: None }
    }

    pub(crate) fn with_mask(mut self, bounds: Rect, default: f32) -> Self {
        self.mask = Some((bounds, default));
        self
    }
}

/// One save's use of the cache: lookups in the last save's entries, and this save's entries,
/// which replace them when the save finishes ([`Session::finish`]).
pub(crate) struct Session {
    cache: PsdExportCache,
    fresh: Mutex<(HashMap<Key, Encoded>, PsdCacheStats)>,
}

impl Session {
    pub(crate) fn new(cache: &PsdExportCache) -> Self {
        Self { cache: cache.clone(), fresh: Mutex::new(Default::default()) }
    }

    /// The channels `source` encodes to as `part` of `layer`, when the last save encoded these
    /// same pixels in this format and version.
    pub(crate) fn get(&self, layer: LayerId, part: Part, source: &Surface, fmt: PixelFormat, version: Version) -> Option<Encoded> {
        let hit = {
            let inner = self.cache.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.channels.get(&(layer, part)).filter(|e| e.fmt == fmt && e.version == version && same_pixels(&e.source, source)).cloned()
        }?;
        let mut fresh = self.fresh.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh.1.reused += 1;
        fresh.0.insert((layer, part), hit.clone());
        Some(hit)
    }

    pub(crate) fn put(&self, layer: LayerId, part: Part, e: Encoded) {
        let mut fresh = self.fresh.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh.1.encoded += 1;
        fresh.0.insert((layer, part), e);
    }

    /// This save's entries become the cache's.
    pub(crate) fn finish(self) {
        let (channels, stats) = self.fresh.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut inner = self.cache.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.channels = channels;
        inner.stats = PsdCacheStats { merged_rendered: inner.stats.merged_rendered, ..stats };
    }
}

impl PsdExportCache {
    /// What the last save through this cache reused and encoded.
    pub fn stats(&self) -> PsdCacheStats {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stats
    }

    /// Forgets everything (the next save encodes every layer).
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Inner::default();
    }

    /// `doc`'s composite as straight 8-bit RGBA (`psdc::compose_rgba8`), rendering only what
    /// changed since the last call when only layer pixels did; `render(doc, rect, out)` renders
    /// `rect` of the canvas into the full-canvas buffer `out`.
    pub(crate) fn merged_rgba8(&self, doc: &Document, render: impl Fn(&Document, Rect, &mut [u8]) -> Option<()>) -> Option<Arc<Vec<u8>>> {
        let canvas = doc.bounds();
        let n = (canvas.width() as usize).checked_mul(canvas.height() as usize)?.checked_mul(4)?;
        let prev = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).merged.take();
        let (mut rgba, dirty) = match prev {
            Some(p) if p.rgba.len() == n => {
                let dirty = render_dirty(&p.doc, doc).unwrap_or(canvas);
                // The last save's caller has let go of it by now: no copy.
                (Arc::try_unwrap(p.rgba).unwrap_or_else(|a| (*a).clone()), dirty)
            }
            _ => (vec![0u8; n], canvas),
        };
        let dirty = dirty.intersect(&canvas);
        if !dirty.is_empty() {
            // A failed render leaves nothing cached: the next save renders everything.
            render(doc, dirty, &mut rgba)?;
        }
        let mut inner = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.stats.merged_rendered = Some(u64::from(dirty.width()) * u64::from(dirty.height()));
        let rgba = Arc::new(rgba);
        inner.merged = Some(MergedCache { doc: snapshot(doc), rgba: rgba.clone() });
        Some(rgba)
    }
}

/// The last composite and the layers it was rendered from.
struct MergedCache {
    doc: Document,
    rgba: Arc<Vec<u8>>,
}

/// What the composite depends on, without the parts that don't change it (selection, history
/// state, metadata): the layers, size, mode and profile.
fn snapshot(doc: &Document) -> Document {
    let mut d = Document::new(String::new(), doc.size, doc.mode, doc.depth);
    d.icc_profile.clone_from(&doc.icc_profile);
    d.layers.clone_from(&doc.layers);
    d
}

/// The area whose composite may differ between `old` and `new`, when only layer pixels and mask
/// pixels changed (an empty rect: nothing did); `None` when anything else did (a layer's
/// properties, the layer tree, the canvas): then everything renders again.
fn render_dirty(old: &Document, new: &Document) -> Option<Rect> {
    if old.size != new.size || old.mode != new.mode || old.depth != new.depth || old.icc_profile != new.icc_profile {
        return None;
    }
    layers_dirty(&old.layers, &new.layers, 0)
}

const MAX_DEPTH: u32 = 64;

fn layers_dirty(old: &[Layer], new: &[Layer], depth: u32) -> Option<Rect> {
    if old.len() != new.len() || depth > MAX_DEPTH {
        return None;
    }
    let mut dirty = Rect::default();
    for (a, b) in old.iter().zip(new) {
        if shell(a) != shell(b) {
            return None;
        }
        let pixels = match (&a.content, &b.content) {
            (LayerContent::Group(ga), LayerContent::Group(gb)) => layers_dirty(&ga.children, &gb.children, depth + 1)?,
            _ => surface_dirty(content_surface(a), content_surface(b))?,
        };
        let mask = surface_dirty(a.mask.as_ref().map(|m| &m.surface), b.mask.as_ref().map(|m| &m.surface))?;
        dirty = union(union(dirty, pixels), mask);
    }
    Some(dirty)
}

fn union(a: Rect, b: Rect) -> Rect {
    if a.is_empty() {
        b
    } else if b.is_empty() {
        a
    } else {
        a.union(&b)
    }
}

/// The pixels a layer draws from (a type layer's raster).
fn content_surface(l: &Layer) -> Option<&Surface> {
    match &l.content {
        LayerContent::Text(t) => t.cache.as_ref(),
        _ => l.surface(),
    }
}

/// `l` without its pixels (content, mask, type raster, group children): equal shells draw alike
/// wherever their pixels are equal.
fn shell(l: &Layer) -> Layer {
    let mut s = l.clone();
    match &mut s.content {
        LayerContent::Raster(r) => *r = Surface::new(r.format()),
        LayerContent::Text(t) => t.cache = t.cache.as_ref().map(|c| Surface::new(c.format())),
        LayerContent::Group(g) => g.children.clear(),
        _ => {}
    }
    if let Some(m) = &mut s.mask {
        m.surface = Surface::new(m.surface.format());
    }
    s
}

/// Where two surfaces' pixels may differ: the union of the tiles that aren't the same allocation;
/// `None` when they can't be compared tile by tile (another format or default pixel).
fn surface_dirty(a: Option<&Surface>, b: Option<&Surface>) -> Option<Rect> {
    match (a, b) {
        (None, None) => Some(Rect::default()),
        (Some(a), Some(b)) => {
            if a.format() != b.format() || a.default_pixel() != b.default_pixel() {
                return None;
            }
            let mut dirty = Rect::default();
            let (mut ia, mut ib) = (a.tiles().peekable(), b.tiles().peekable());
            loop {
                let r = match (ia.peek(), ib.peek()) {
                    (None, None) => break,
                    (Some((ca, ta)), Some((cb, tb))) if ca == cb => {
                        let r = (!Arc::ptr_eq(ta, tb)).then(|| ca.rect());
                        ia.next();
                        ib.next();
                        r
                    }
                    (Some((ca, _)), Some((cb, _))) if ca < cb => ia.next().map(|(c, _)| c.rect()),
                    (Some(_), Some(_)) | (None, Some(_)) => ib.next().map(|(c, _)| c.rect()),
                    (Some(_), None) => ia.next().map(|(c, _)| c.rect()),
                };
                if let Some(r) = r {
                    dirty = union(dirty, r);
                }
            }
            Some(dirty)
        }
        _ => None,
    }
}

/// Whether `a` and `b` hold the same pixels by construction: the same format, default pixel and
/// tile allocations.
fn same_pixels(a: &Surface, b: &Surface) -> bool {
    a.format() == b.format()
        && a.default_pixel() == b.default_pixel()
        && a.tiles().count() == b.tiles().count()
        && a.tiles().zip(b.tiles()).all(|((ca, ta), (cb, tb))| ca == cb && Arc::ptr_eq(ta, tb))
}

#[cfg(test)]
mod tests {
    use super::*;

    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::{LayerMask, Size};

    fn doc(mode: ColorMode, depth: SampleType) -> Document {
        let mut d = Document::new("t", Size::new(700, 520), mode, depth);
        let fmt = d.pixel_format();
        let mut bg = Layer::raster("Background", fmt);
        if let Some(s) = bg.surface_mut() {
            let n = s.format().channels();
            let (mut paper, mut ink) = (vec![0.9; n], vec![0.1; n]);
            paper[n - 1] = 1.0;
            ink[n - 1] = 1.0;
            s.fill_rect(Rect::from_xywh(0, 0, 700, 520), &paper);
            for x in (0..700).step_by(9) {
                s.fill_rect(Rect::from_xywh(x, 0, 2, 520), &ink);
            }
        }
        d.layers.push(bg);
        for i in 0..4 {
            let mut l = Layer::raster(format!("L{i}"), fmt);
            if let Some(s) = l.surface_mut() {
                let mut px = vec![0.2 * i as f32; s.format().channels()];
                if let Some(a) = px.last_mut() {
                    *a = 0.8;
                }
                s.fill_rect(Rect::from_xywh(40 + i * 150, 30 + i * 90, 260, 180), &px);
            }
            if i == 2 {
                let mf = PixelFormat::new(ColorMode::Grayscale, depth, false);
                let mut m = Surface::new(mf);
                m.fill_rect(Rect::from_xywh(0, 0, 700, 520), &[1.0]);
                m.fill_rect(Rect::from_xywh(350, 220, 60, 60), &[0.0]);
                l.mask = Some(LayerMask { surface: m, ..LayerMask::reveal_all() });
            }
            d.layers.push(l);
        }
        d
    }

    fn save(d: &Document, cache: Option<&PsdExportCache>) -> Vec<u8> {
        let opts = crate::ExportOptions { psd_cache: cache.cloned(), ..Default::default() };
        crate::export(d, "t.psd", &opts).unwrap().bytes
    }

    /// #16: after each kind of edit, an incremental save writes a full save's bytes, and encodes
    /// (and renders) only what changed.
    #[test]
    fn incremental_saves_are_full_saves() {
        for (mode, depth) in [(ColorMode::Rgb, SampleType::U8), (ColorMode::Grayscale, SampleType::U8), (ColorMode::Rgb, SampleType::U16)] {
            let cache = PsdExportCache::default();
            let mut d = doc(mode, depth);
            assert_eq!(save(&d, Some(&cache)), save(&d, None), "{mode:?} {depth:?}: first save");
            assert_eq!(cache.stats().reused, 0);
            let px = vec![0.5; d.layers[3].surface().unwrap().format().channels()];
            type Edit = Box<dyn Fn(&mut Document)>;
            let edits: Vec<(&str, Edit)> = vec![
                ("nothing", Box::new(|_| {})),
                ("a dab", Box::new(move |d| d.layers[3].surface_mut().unwrap().fill_rect(Rect::from_xywh(330, 200, 8, 8), &px))),
                ("a mask dab", Box::new(|d| d.layers[3].mask.as_mut().unwrap().surface.fill_rect(Rect::from_xywh(380, 250, 4, 4), &[1.0]))),
                ("opacity", Box::new(|d| d.layers[1].opacity = 0.5)),
                ("hidden", Box::new(|d| d.layers[2].visible = false)),
                ("removed", Box::new(|d| drop(d.layers.remove(4)))),
                ("moved", Box::new(|d| d.layers.swap(1, 2))),
            ];
            for (what, edit) in edits {
                edit(&mut d);
                let inc = save(&d, Some(&cache));
                assert!(inc == save(&d, None), "{mode:?} {depth:?}: after {what}, the incremental save differs");
                let st = cache.stats();
                assert!(st.reused >= 3, "{what}: {st:?}");
                if what == "a dab" && (mode, depth) == (ColorMode::Rgb, SampleType::U8) {
                    assert_eq!(st.encoded, 1, "{st:?}");
                    let rendered = st.merged_rendered.unwrap();
                    assert!(rendered > 0 && rendered < 700 * 520 / 2, "only the changed tiles render: {st:?}");
                }
                if what == "nothing" {
                    assert_eq!(st.encoded, 0, "{st:?}");
                }
            }
        }
    }

    #[test]
    fn same_pixels_is_a_pointer_check() {
        let fmt = PixelFormat::new(photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8, true);
        let mut a = Surface::new(fmt);
        a.fill_rect(Rect::from_xywh(0, 0, 600, 300), &[1.0, 0.0, 0.0, 1.0]);
        let mut b = a.clone();
        assert!(same_pixels(&a, &b));
        assert_eq!(surface_dirty(Some(&a), Some(&b)), Some(Rect::default()));
        b.fill_rect(Rect::from_xywh(5, 5, 2, 2), &[0.0, 1.0, 0.0, 1.0]);
        assert!(!same_pixels(&a, &b));
        let d = surface_dirty(Some(&a), Some(&b)).unwrap();
        assert!(d.contains_rect(&Rect::from_xywh(5, 5, 2, 2)) && d.width() < 600, "{d:?}");
        // Equal values in other tiles are still "changed": only identity counts.
        let mut c = Surface::new(fmt);
        c.fill_rect(Rect::from_xywh(0, 0, 600, 300), &[1.0, 0.0, 0.0, 1.0]);
        assert!(!same_pixels(&a, &c));
        b.fill_rect(Rect::from_xywh(900, 900, 2, 2), &[0.0, 1.0, 0.0, 1.0]);
        assert!(surface_dirty(Some(&a), Some(&b)).unwrap().contains_rect(&Rect::from_xywh(900, 900, 2, 2)));
        assert_eq!(surface_dirty(Some(&a), None), None);
    }
}
