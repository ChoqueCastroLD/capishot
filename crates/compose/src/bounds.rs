//! `Surface::content_bounds` with the per-tile scan cached by copy-on-write tile identity, so
//! layer bounds cost a hash lookup per tile instead of a pixel scan per render tile (the CPU
//! compositor asks for an effect layer's bounds in every tile; the GPU planner every frame).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use photocraft_geom::{Rect, TILE_SIZE};
use photocraft_raster::{Surface, Tile};

struct Entry {
    tile: Weak<Tile>,
    /// Content bounds inside the tile (tile-local), for one default pixel.
    default: Box<[u8]>,
    bounds: Option<(u16, u16, u16, u16)>,
}

fn cache() -> &'static Mutex<HashMap<usize, Entry>> {
    static C: std::sync::OnceLock<Mutex<HashMap<usize, Entry>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Pixels of `t` that differ from `dp` (tile-local half-open bounds).
fn scan(t: &Tile, dp: &[u8]) -> Option<(u16, u16, u16, u16)> {
    let bpp = dp.len();
    let ts = TILE_SIZE as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (y, row) in t.bytes().chunks_exact(ts * bpp).enumerate() {
        let Some(first) = row.chunks_exact(bpp).position(|px| px != dp) else { continue };
        let last = ts - 1 - row.chunks_exact(bpp).rev().position(|px| px != dp).unwrap_or(0);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x0 != usize::MAX).then_some((x0 as u16, y0 as u16, x1 as u16, y1 as u16))
}

/// `Surface::content_bounds`, cached per tile. Cached tiles count first; the rest are scanned from
/// the outside of the tile grid inwards, skipping any tile that lies inside the bounds found so far
/// (it can't extend them), so a first look at a full-canvas layer scans about its outer ring. The
/// scans run without holding the cache's lock.
pub fn content_bounds(s: &Surface) -> Rect {
    if s.tile_count() == 0 {
        return Rect::EMPTY;
    }
    let fmt = s.format();
    let mut dp = vec![0u8; fmt.bytes_per_pixel()];
    photocraft_raster::encode_pixel(&fmt, &s.default_pixel(), &mut dp);
    let mut out = Rect::EMPTY;
    let add = |coord: &photocraft_geom::TileCoord, b: Option<(u16, u16, u16, u16)>, out: &mut Rect| {
        if let Some((x0, y0, x1, y1)) = b {
            let o = coord.rect();
            let r = Rect::new(o.x0 + x0 as i32, o.y0 + y0 as i32, o.x0 + x1 as i32, o.y0 + y1 as i32);
            *out = if out.is_empty() { r } else { out.union(&r) };
        }
    };
    let mut misses = Vec::new();
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        if c.len() > 1 << 16 {
            c.retain(|_, e| e.tile.strong_count() > 0);
        }
        for (coord, t) in s.tiles() {
            let key = Arc::as_ptr(t) as usize;
            match c.get(&key).filter(|e| e.tile.upgrade().is_some_and(|u| Arc::ptr_eq(&u, t)) && *e.default == *dp) {
                Some(e) => add(coord, e.bounds, &mut out),
                None => misses.push((coord, t)),
            }
        }
    }
    if misses.is_empty() {
        return out;
    }
    let (mut gx0, mut gy0, mut gx1, mut gy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (c, _) in s.tiles() {
        (gx0, gy0, gx1, gy1) = (gx0.min(c.tx), gy0.min(c.ty), gx1.max(c.tx), gy1.max(c.ty));
    }
    misses.sort_by_key(|(c, _)| (c.tx - gx0).min(gx1 - c.tx).min(c.ty - gy0).min(gy1 - c.ty));
    let mut scanned = Vec::new();
    for (coord, t) in misses {
        if !out.is_empty() && out.contains_rect(&coord.rect()) {
            continue;
        }
        let b = scan(t, &dp);
        add(coord, b, &mut out);
        scanned.push((Arc::as_ptr(t) as usize, Arc::downgrade(t), b));
    }
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    for (key, tile, bounds) in scanned {
        c.insert(key, Entry { tile, default: dp.clone().into_boxed_slice(), bounds });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, PixelFormat, SampleType};

    /// The cached, ring-ordered scan finds exactly what the plain scan finds: cold, warm, after a
    /// tile changes, on full-canvas layers (inner tiles skipped) and with a non-transparent default.
    #[test]
    fn cached_bounds_match_the_plain_scan() {
        let fmt = PixelFormat::new(ColorMode::Rgb, SampleType::U8, true);
        let mut s = Surface::new(fmt);
        assert_eq!(content_bounds(&s), Rect::EMPTY);
        for r in [Rect::new(0, 0, 2000, 3000), Rect::new(-40, 10, 900, 777), Rect::new(1500, 2900, 1501, 2901)] {
            s.fill_rect(r, &[0.2, 0.4, 0.6, 1.0]);
            assert_eq!(content_bounds(&s), s.content_bounds(), "{r:?} cold");
            assert_eq!(content_bounds(&s), s.content_bounds(), "{r:?} warm");
        }
        let mut t = Surface::new(fmt);
        t.fill_rect(Rect::new(300, 300, 1300, 1300), &[1.0, 0.0, 0.0, 1.0]);
        assert_eq!(content_bounds(&t), Rect::new(300, 300, 1300, 1300));
        // Clearing the outer part leaves a smaller box; only the changed tiles are new.
        t.fill_rect(Rect::new(300, 300, 1300, 700), &[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(content_bounds(&t), t.content_bounds());
        assert_eq!(content_bounds(&t), Rect::new(300, 700, 1300, 1300));
        let mut m = Surface::new(PixelFormat::new(ColorMode::Grayscale, SampleType::U8, false));
        m.fill_rect(Rect::new(0, 0, 600, 600), &[1.0]);
        m.fill_rect(Rect::new(10, 20, 30, 40), &[0.5]);
        assert_eq!(content_bounds(&m), m.content_bounds());
    }
}
