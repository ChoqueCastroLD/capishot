//! Box-filtered mip levels of a document's pixel surfaces, cached by tile identity.
//!
//! Level `l` of a surface holds its pixels averaged over `2^l × 2^l` blocks (alpha-weighted where
//! the format has alpha, so a reduced layer composites like the area average of the full one).
//! A tile of level `l + 1` is the 2×2 reduction of four tiles of level `l`; it is cached under
//! their copy-on-write pointers, so after an edit only the reduced tiles above the changed tiles
//! are recomputed, and a [`MipCache::document`] of the next document state costs a pointer walk.
//!
//! [`MipCache::document`] is the document every surface of which is reduced to level `l` (the
//! structure of [`crate::proxy::proxy_document`]): its composite is the canvas at `1/2^l`
//! resolution for about `1/4^l` of the compositing work. Where [`crate::proxy::proxy_faithful`]
//! is false (layer effects, vector masks) a reduced document is not a faithful reduction.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};

use photocraft_color::{PixelFormat, SampleType, read_sample, write_sample};
use photocraft_doc::Document;
use photocraft_geom::{TILE_SIZE, TileCoord};
use photocraft_raster::{Surface, Tile};

/// Deepest level kept (1/65536): a 300k-pixel side reduces to under 5 pixels.
pub const MAX_LEVEL: u32 = 16;

/// The four child tile pointers (0 where the child is missing) and the surface's format and
/// default pixel (what a missing child reads as).
type Key = ([usize; 4], u64);

struct Entry {
    children: [Option<Weak<Tile>>; 4],
    tile: Arc<Tile>,
    /// [`MipCache::epoch`] of the last build that used this entry.
    used: u64,
}

/// A whole surface's reduction, kept so an unchanged layer of the next document state (a stroke
/// changes one layer) costs a pointer comparison instead of a walk over its tiles.
struct Memo {
    sources: Vec<(TileCoord, Weak<Tile>)>,
    out: Surface,
    used: u64,
}

/// Reduced tiles by the tiles they were reduced from (see the module docs).
#[derive(Default)]
pub struct MipCache {
    entries: HashMap<Key, Entry>,
    /// By (fingerprint of the source's tiles, format and default; level).
    memos: HashMap<(u64, u32), Memo>,
    epoch: u64,
    bytes: usize,
}

/// Most whole-surface reductions kept.
const MAX_MEMOS: usize = 4096;

impl MipCache {
    /// Bytes of reduced tiles held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Reduced tiles held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Start a new use: tiles not used since are the first [`Self::trim`] lets go.
    pub fn begin(&mut self) {
        self.epoch += 1;
    }

    /// Drop entries until at most `max_bytes` are held: those whose source tiles are gone first,
    /// then the least recently used. Entries used by the current epoch stay.
    pub fn trim(&mut self, max_bytes: usize) {
        let epoch = self.epoch;
        // Memos hold their reduced tiles: they go with the sources they were made from.
        self.memos.retain(|_, m| m.used == epoch || m.sources.iter().all(|(_, w)| w.strong_count() > 0));
        if self.memos.len() > MAX_MEMOS || self.bytes > max_bytes {
            self.memos.retain(|_, m| m.used == epoch);
        }
        let before = self.entries.len();
        self.entries.retain(|_, e| e.used == epoch || e.children.iter().flatten().all(|w| w.strong_count() > 0));
        if self.entries.len() != before {
            self.bytes = self.entries.values().map(|e| e.tile.bytes().len()).sum();
        }
        if self.bytes <= max_bytes {
            return;
        }
        let mut by_age: Vec<(u64, Key)> = self.entries.iter().filter(|(_, e)| e.used != epoch).map(|(k, e)| (e.used, *k)).collect();
        by_age.sort_unstable_by_key(|(used, _)| *used);
        for (_, k) in by_age {
            if self.bytes <= max_bytes {
                break;
            }
            if let Some(e) = self.entries.remove(&k) {
                self.bytes = self.bytes.saturating_sub(e.tile.bytes().len());
            }
        }
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.memos.clear();
        self.bytes = 0;
    }

    /// `s` reduced to `level` (`s` itself at 0).
    pub fn surface(&mut self, s: &Surface, level: u32) -> Surface {
        if level == 0 || s.tile_count() == 0 {
            return self.reduce(s, level);
        }
        let fp = fingerprint(s);
        let epoch = self.epoch;
        if let Some(m) = self.memos.get_mut(&(fp, level))
            && m.sources.len() == s.tile_count()
            && m.sources.iter().zip(s.tiles()).all(|((c, w), (c2, t))| c == c2 && w.upgrade().is_some_and(|w| Arc::ptr_eq(&w, t)))
        {
            m.used = epoch;
            return m.out.clone();
        }
        let out = self.reduce(s, level);
        let sources = s.tiles().map(|(c, t)| (*c, Arc::downgrade(t))).collect();
        self.memos.insert((fp, level), Memo { sources, out: out.clone(), used: epoch });
        out
    }

    fn reduce(&mut self, s: &Surface, level: u32) -> Surface {
        let mut cur = s.clone();
        for _ in 0..level.min(MAX_LEVEL) {
            if cur.tile_count() == 0 {
                break;
            }
            cur = self.half(&cur);
        }
        cur
    }

    /// `doc` with every surface reduced to `level` (see the module docs); `doc` itself at 0.
    pub fn document(&mut self, doc: &Document, level: u32) -> Document {
        let level = level.min(MAX_LEVEL);
        crate::proxy::proxy_document_with(doc, 1 << level, &mut |s| self.surface(s, level))
    }

    /// One level down: each tile of the result from the 2×2 tiles of `s` it covers.
    fn half(&mut self, s: &Surface) -> Surface {
        let fmt = s.format();
        let default = s.default_bytes().to_vec();
        let tag = format_tag(fmt, &default);
        let mut groups: BTreeMap<TileCoord, [Option<&Arc<Tile>>; 4]> = BTreeMap::new();
        for (c, t) in s.tiles() {
            let parent = TileCoord::new(c.tx.div_euclid(2), c.ty.div_euclid(2));
            let slot = (c.tx.rem_euclid(2) + 2 * c.ty.rem_euclid(2)) as usize;
            if let Some(g) = groups.entry(parent).or_insert([None; 4]).get_mut(slot) {
                *g = Some(t);
            }
        }
        let mut out = Surface::with_default(fmt, &s.default_pixel());
        let epoch = self.epoch;
        let mut found = Vec::with_capacity(groups.len());
        let mut todo = Vec::new();
        for (parent, children) in groups {
            let key = (children.map(|c| c.map_or(0, |t| Arc::as_ptr(t) as usize)), tag);
            match self.entries.get_mut(&key) {
                Some(e) if same_children(&e.children, &children) => {
                    e.used = epoch;
                    found.push((parent, e.tile.clone()));
                }
                _ => todo.push((parent, key, children)),
            }
        }
        let Ok(blank) = out.try_default_tile() else { return out };
        let reduce = |children: &[Option<&Arc<Tile>>; 4]| {
            let mut t = blank.clone();
            reduce_into(fmt, &default, children.map(|c| c.map(|t| t.bytes())), t.bytes_mut());
            Arc::new(t)
        };
        #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
        let made: Vec<Arc<Tile>> = {
            use rayon::prelude::*;
            todo.par_iter().map(|(_, _, c)| reduce(c)).collect()
        };
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        let made: Vec<Arc<Tile>> = todo.iter().map(|(_, _, c)| reduce(c)).collect();
        for ((parent, key, children), tile) in todo.into_iter().zip(made) {
            self.bytes += tile.bytes().len();
            let old = self.entries.insert(key, Entry { children: children.map(|c| c.map(Arc::downgrade)), tile: tile.clone(), used: epoch });
            if let Some(old) = old {
                self.bytes = self.bytes.saturating_sub(old.tile.bytes().len());
            }
            found.push((parent, tile));
        }
        out.put_tiles(found);
        out
    }
}

/// Identity of a surface's pixels: its tile pointers and coordinates, format and default pixel.
fn fingerprint(s: &Surface) -> u64 {
    let mut h = format_tag(s.format(), s.default_bytes()) ^ s.tile_count() as u64;
    for (c, t) in s.tiles() {
        let v = (Arc::as_ptr(t) as usize as u64) ^ (u64::from(c.tx as u32) << 32 | u64::from(c.ty as u32)).rotate_left(17);
        h = (h ^ v).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn same_children(held: &[Option<Weak<Tile>>; 4], now: &[Option<&Arc<Tile>>; 4]) -> bool {
    held.iter().zip(now).all(|(h, n)| match (h, n) {
        (None, None) => true,
        (Some(w), Some(t)) => w.upgrade().is_some_and(|w| Arc::ptr_eq(&w, t)),
        _ => false,
    })
}

fn format_tag(fmt: PixelFormat, default: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fmt.hash(&mut h);
    default.hash(&mut h);
    h.finish()
}

/// Fill `out` (one tile of `fmt`) with the 2×2 box reduction of `children` (top-left, top-right,
/// bottom-left, bottom-right; `None` reads as the `default` pixel), each child landing in its
/// quadrant. Colour is averaged weighted by alpha when the format has alpha.
pub fn reduce_into(fmt: PixelFormat, default: &[u8], children: [Option<&[u8]>; 4], out: &mut [u8]) {
    let bpp = fmt.bytes_per_pixel();
    let side = TILE_SIZE as usize;
    let half = side / 2;
    if bpp == 0 || out.len() < side * side * bpp || default.len() != bpp {
        return;
    }
    for (q, child) in children.iter().enumerate() {
        let (qx, qy) = ((q % 2) * half, (q / 2) * half);
        let Some(src) = child.filter(|c| c.len() >= side * side * bpp) else {
            // A missing child is all default: so is its quadrant.
            for y in 0..half {
                let row = (qy + y) * side + qx;
                if let Some(dst) = out.get_mut(row * bpp..(row + half) * bpp) {
                    for px in dst.chunks_exact_mut(bpp) {
                        px.copy_from_slice(default);
                    }
                }
            }
            continue;
        };
        for y in 0..half {
            let (r0, r1) = (2 * y * side * bpp, (2 * y + 1) * side * bpp);
            let (Some(a), Some(b)) = (src.get(r0..r0 + side * bpp), src.get(r1..r1 + side * bpp)) else { continue };
            let o = ((qy + y) * side + qx) * bpp;
            let Some(dst) = out.get_mut(o..o + half * bpp) else { continue };
            match fmt.sample {
                SampleType::U8 => reduce_row_u8(a, b, dst, bpp, fmt.alpha),
                _ => reduce_row_any(a, b, dst, fmt),
            }
        }
    }
}

/// One output row of 8-bit samples from source rows `a` and `b`.
fn reduce_row_u8(a: &[u8], b: &[u8], dst: &mut [u8], n: usize, alpha: bool) {
    if alpha && n >= 2 {
        let ac = n - 1;
        for ((d, pa), pb) in dst.chunks_exact_mut(n).zip(a.chunks_exact(2 * n)).zip(b.chunks_exact(2 * n)) {
            let al = [u32::from(pa[ac]), u32::from(pa[n + ac]), u32::from(pb[ac]), u32::from(pb[n + ac])];
            let sum = al[0] + al[1] + al[2] + al[3];
            if sum == 0 {
                d.fill(0);
                continue;
            }
            for c in 0..ac {
                let v = u32::from(pa[c]) * al[0] + u32::from(pa[n + c]) * al[1] + u32::from(pb[c]) * al[2] + u32::from(pb[n + c]) * al[3];
                d[c] = ((v + sum / 2) / sum) as u8;
            }
            d[ac] = ((sum + 2) / 4) as u8;
        }
    } else {
        for ((d, pa), pb) in dst.chunks_exact_mut(n).zip(a.chunks_exact(2 * n)).zip(b.chunks_exact(2 * n)) {
            for c in 0..n {
                d[c] = ((u32::from(pa[c]) + u32::from(pa[n + c]) + u32::from(pb[c]) + u32::from(pb[n + c]) + 2) / 4) as u8;
            }
        }
    }
}

/// [`reduce_row_u8`] for 16-bit and float samples, through normalised floats.
fn reduce_row_any(a: &[u8], b: &[u8], dst: &mut [u8], fmt: PixelFormat) {
    let n = fmt.channels();
    let st = fmt.sample;
    let bpp = fmt.bytes_per_pixel();
    let pixels = dst.len() / bpp.max(1);
    for i in 0..pixels {
        let at = |rows: &[u8], px: usize, c: usize| read_sample(rows, st, px * n + c);
        let src = [(a, 2 * i), (a, 2 * i + 1), (b, 2 * i), (b, 2 * i + 1)];
        if fmt.alpha && n >= 2 {
            let ac = n - 1;
            let al = src.map(|(r, p)| at(r, p, ac));
            let sum: f32 = al.iter().sum();
            for c in 0..ac {
                let v = if sum > 0.0 { src.iter().zip(al).map(|(&(r, p), w)| at(r, p, c) * w).sum::<f32>() / sum } else { 0.0 };
                write_sample(dst, st, i * n + c, v);
            }
            write_sample(dst, st, i * n + ac, sum / 4.0);
        } else {
            for c in 0..n {
                write_sample(dst, st, i * n + c, src.iter().map(|&(r, p)| at(r, p, c)).sum::<f32>() / 4.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{Color, ColorMode};
    use photocraft_doc::Size;
    use photocraft_geom::Rect;

    fn noise(s: &mut Surface, r: Rect, seed: u32) {
        let mut v = seed;
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                v = v.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let a = ((v >> 8) & 255) as f32 / 255.0;
                s.write_pixel(x, y, &[((v >> 16) & 255) as f32 / 255.0, ((v >> 4) & 255) as f32 / 255.0, 0.5, a]);
            }
        }
    }

    /// The alpha-weighted mean of the `k`×`k` block at level pixel (x, y), straight from `s`.
    fn block_mean(s: &Surface, x: i32, y: i32, k: i32) -> [f32; 4] {
        let mut acc = [0.0f32; 4];
        for yy in y * k..(y + 1) * k {
            for xx in x * k..(x + 1) * k {
                let p = s.rgba(xx, yy);
                for c in 0..3 {
                    acc[c] += p[c] * p[3];
                }
                acc[3] += p[3];
            }
        }
        let n = (k * k) as f32;
        if acc[3] > 0.0 { [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3] / n] } else { [0.0; 4] }
    }

    #[test]
    fn levels_are_alpha_weighted_block_means_at_every_depth() {
        for fmt in [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F] {
            let mut s = Surface::new(fmt);
            noise(&mut s, Rect::new(-260, -10, 600, 530), 3);
            let mut cache = MipCache::default();
            for level in 1..=3 {
                let k = 1 << level;
                let r = cache.surface(&s, level);
                // 8-bit levels round at each step: at most a step per level.
                let tol = if fmt.sample == SampleType::U8 { level as f32 * 1.01 / 255.0 } else { 1e-3 };
                for (x, y) in [(0, 0), (3, 7), (-1, -1), (-30, 2), (600 / k - 1, 530 / k - 1), (17, 33)] {
                    let (got, want) = (r.rgba(x, y), block_mean(&s, x, y, k));
                    // Colour only matters where there is coverage.
                    let cols = if want[3] > 0.02 { 0..4 } else { 3..4 };
                    for c in cols {
                        assert!((got[c] - want[c]).abs() <= tol + 1e-6, "{fmt:?} L{level} ({x},{y}) c{c}: {got:?} vs {want:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn opaque_gray_masks_average_plainly_and_keep_their_default() {
        let mut s = Surface::with_default(PixelFormat::GRAY8, &[1.0]);
        s.fill_rect(Rect::new(0, 0, 2, 1), &[0.0]);
        let mut cache = MipCache::default();
        let r = cache.surface(&s, 1);
        assert!((r.pixel(0, 0)[0] - 0.5).abs() < 0.003);
        assert_eq!(r.pixel(1, 0), vec![1.0], "untouched pixels keep the default");
        assert_eq!(r.pixel(5000, 5000), vec![1.0], "missing tiles read as the default");
    }

    #[test]
    fn an_edit_recomputes_only_the_reduced_tiles_above_it() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 2048, 2048), &[0.2, 0.4, 0.6, 1.0]);
        noise(&mut s, Rect::new(0, 0, 1024, 1024), 9);
        let mut cache = MipCache::default();
        cache.begin();
        let a = cache.surface(&s, 2);
        let held = cache.len();
        // The solid tiles share one pointer: their reductions are shared too.
        assert!(held < 16 + 4, "{held}");
        let mut edited = s.clone();
        edited.write_pixel(1500, 1500, &[1.0, 0.0, 0.0, 1.0]);
        cache.begin();
        let b = cache.surface(&edited, 2);
        // Only the reduced tiles above the edited tile are new (a few), not the 16 + 4 of a rebuild.
        assert!(cache.len() > held && cache.len() <= held + 3, "{held} → {}", cache.len());
        let same = |x: &Surface, y: &Surface, c: TileCoord| Arc::ptr_eq(x.tile(c).unwrap(), y.tile(c).unwrap());
        assert!(same(&a, &b, TileCoord::new(0, 0)));
        assert!(!same(&a, &b, TileCoord::new(1, 1)));
        assert!(b.rgba(375, 375)[0] > a.rgba(375, 375)[0]);
        // The old state's sources are gone once only the new document holds tiles.
        drop((s, a));
        cache.trim(0);
        assert!(cache.bytes() > 0, "this epoch's tiles stay");
        cache.begin();
        cache.trim(0);
        assert_eq!((cache.len(), cache.bytes()), (0, 0));
    }

    #[test]
    fn an_unchanged_surface_reuses_its_whole_reduction() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        noise(&mut s, Rect::new(0, 0, 700, 600), 4);
        let mut cache = MipCache::default();
        let a = cache.surface(&s, 2);
        let b = cache.surface(&s.clone(), 2);
        assert!(a.tiles().zip(b.tiles()).all(|((_, x), (_, y))| Arc::ptr_eq(x, y)));
        let mut edited = s.clone();
        edited.write_pixel(10, 10, &[1.0, 0.0, 0.0, 1.0]);
        let c = cache.surface(&edited, 2);
        assert_ne!(c.rgba(2, 2), a.rgba(2, 2), "an edit is not served from the old memo");
        assert_eq!(c.rgba(150, 120), a.rgba(150, 120));
    }

    #[test]
    fn reduced_documents_keep_structure_and_shrink_by_powers_of_two() {
        let mut doc = Document::with_background("d", Size::new(1000, 600), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut cache = MipCache::default();
        let p = cache.document(&doc, 2);
        assert_eq!((p.size.width, p.size.height), (250, 150));
        assert_eq!(p.layers[0].id, doc.layers[0].id);
        assert_eq!(p.layers[0].surface().unwrap().pixel(249, 149), vec![1.0; 4]);
        let same = cache.document(&doc, 0);
        assert_eq!(same.size, doc.size);
        doc.size = Size::new(1001, 601);
        assert_eq!(cache.document(&doc, 1).size, Size::new(501, 301), "partial blocks round up");
    }

    /// A page-like document: white paper, a Multiply tone, opaque and soft-edged ink layers.
    fn page() -> Document {
        let mut doc = Document::with_background("p", Size::new(700, 900), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut tone = photocraft_doc::Layer::raster("tone", PixelFormat::RGBA8);
        if let photocraft_doc::LayerContent::Raster(s) = &mut tone.content {
            noise(s, Rect::new(0, 0, 700, 900), 5);
        }
        tone.blend = photocraft_color::blend::BlendMode::Multiply;
        tone.opacity = 0.4;
        doc.layers.push(tone);
        for (i, r) in [Rect::new(40, 60, 420, 500), Rect::new(300, 380, 690, 880)].into_iter().enumerate() {
            let mut l = photocraft_doc::Layer::raster(format!("ink{i}"), PixelFormat::RGBA8);
            if let photocraft_doc::LayerContent::Raster(s) = &mut l.content {
                s.fill_rect(r, &[0.1 + 0.4 * i as f32, 0.3, 0.8, 1.0]);
                noise(s, Rect::new(r.x0, r.y0, r.x0 + 64, r.y1), 11 + i as u32);
            }
            doc.layers.push(l);
        }
        doc
    }

    #[test]
    fn render_rects_matches_render_for_each_rect() {
        let doc = page();
        let rects = [Rect::new(0, 0, 258, 258), Rect::new(254, 254, 512, 512), Rect::new(600, 800, 700, 900), Rect::new(5, 5, 5, 9)];
        let got = crate::render_rects(&doc, &rects);
        for (r, b) in rects.iter().zip(&got) {
            assert_eq!(b, &crate::render(&doc, *r), "{r:?}");
        }
        assert!(crate::render_rects(&doc, &[]).is_empty());
    }

    #[test]
    fn a_reduced_document_composites_like_the_reduced_canvas() {
        // LOD views differ from the exact reduction only by resampling: per-layer box filtering
        // before blending, which for these blends stays within a few 8-bit steps.
        let doc = page();
        let mut cache = MipCache::default();
        for level in 1..=2u32 {
            let k = 1 << level;
            let small = cache.document(&doc, level);
            let (w, h) = (small.size.width, small.size.height);
            let got = crate::render(&small, small.bounds());
            let want = crate::render_reduced(&doc, w, h);
            let mut worst = 0.0f32;
            let mut sum = 0.0f32;
            for (a, b) in got.px.iter().zip(&want.px) {
                let d = (0..4).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max);
                worst = worst.max(d);
                sum += d;
            }
            let mean = sum / got.px.len() as f32;
            assert!(mean < 1.5 / 255.0 && worst < 0.08, "level {level} (×{k}): mean {mean}, worst {worst}");
        }
    }

    #[test]
    fn premultiplied_conversion_matches_the_per_pixel_rounding() {
        let mut px = Vec::new();
        for a in 0..=255u32 {
            for c in (0..=255u32).step_by(3) {
                px.push([c as f32 / 255.0, 1.0 - c as f32 / 255.0, 0.5, a as f32 / 255.0]);
            }
        }
        px.push([2.0, -1.0, f32::NAN, 0.7]);
        let buf = crate::Buffer { rect: Rect::new(0, 0, px.len() as i32, 1), px };
        let straight = buf.to_rgba8();
        let want: Vec<[u8; 4]> = straight.pixels.as_chunks::<4>().0.iter().map(|p| crate::premultiply_px(*p)).collect();
        assert_eq!(buf.to_premultiplied_rgba8(), want);
    }

    #[test]
    fn layer_effects_scale_with_the_level() {
        use photocraft_doc::effects::{Effect, FxCommon, FxPaint, Shadow, StrokeFx, StrokePosition};
        let mut doc = page();
        let ink = &mut doc.layers[2];
        ink.effects.enabled = true;
        ink.effects.items.push(Effect::DropShadow(Shadow {
            common: FxCommon::new(photocraft_color::blend::BlendMode::Multiply, 0.75),
            color: Color::BLACK,
            angle: 120.0,
            use_global_light: false,
            distance: 24.0,
            spread: 0.0,
            size: 12.0,
            contour: Default::default(),
            anti_alias: false,
            noise: 0.0,
            knocks_out: false,
        }));
        ink.effects.items.push(Effect::Stroke(StrokeFx {
            common: FxCommon::new(photocraft_color::blend::BlendMode::Normal, 1.0),
            size: 8.0,
            position: StrokePosition::Outside,
            paint: FxPaint::Color(Color::rgb(1.0, 1.0, 1.0)),
            overprint: false,
        }));
        assert!(crate::effects::has_effects(&doc.layers[2]));
        let mut cache = MipCache::default();
        let small = cache.document(&doc, 2);
        let got = crate::render(&small, small.bounds());
        let want = crate::render_reduced(&doc, small.size.width, small.size.height);
        let diff: Vec<f32> = got.px.iter().zip(&want.px).map(|(a, b)| (0..4).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)).collect();
        let mean = diff.iter().sum::<f32>() / diff.len() as f32;
        // Effects drawn at the level's scale differ from the reduced full-size ones at their edges.
        assert!(mean < 4.0 / 255.0, "mean {mean}");
    }

    #[test]
    fn hostile_buffers_do_not_panic() {
        let mut out = vec![0u8; 16];
        reduce_into(PixelFormat::RGBA8, &[0; 4], [Some(&[1, 2, 3][..]), None, None, None], &mut out);
        let mut out = vec![0u8; (TILE_SIZE * TILE_SIZE * 4) as usize];
        reduce_into(PixelFormat::RGBA8, &[0; 3], [None; 4], &mut out);
        reduce_into(PixelFormat::RGBA8, &[0; 4], [Some(&[1, 2, 3][..]), None, None, None], &mut out);
    }
}
