//! A layer's effective mask (pixel mask × vector mask) as one surface, for backends that sample
//! masks from textures (the GPU compositor). Rasterising a vector mask costs a path coverage
//! pass, so results are cached per mask state; unchanged masks return the same tiles (cheap to
//! clone, and the GPU sees no change).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::Layer;
use photocraft_geom::Rect;
use photocraft_raster::Surface;

const FORMAT: PixelFormat = PixelFormat { mode: ColorMode::Grayscale, sample: SampleType::F32, alpha: false };

struct Cache {
    map: HashMap<u64, (Surface, u64)>,
    tick: u64,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { map: HashMap::new(), tick: 0 }))
}

/// Entries kept (least recently used dropped first).
const CAPACITY: usize = 64;

fn key(layer: &Layer, canvas: Rect) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    layer.id.0.hash(&mut h);
    (canvas.x0, canvas.y0, canvas.x1, canvas.y1).hash(&mut h);
    format!("{:?}", layer.vector_mask).hash(&mut h);
    if let Some(m) = &layer.mask {
        (m.enabled, m.density.to_bits()).hash(&mut h);
        format!("{:?}", m.surface.default_pixel()).hash(&mut h);
        for (c, t) in m.surface.tiles() {
            (c.tx, c.ty, Arc::as_ptr(t) as usize).hash(&mut h);
        }
    }
    h.finish()
}

/// The layer's mask values (pixel mask with density × vector mask, exactly as the CPU
/// compositor applies them) as a single-channel f32 surface over `canvas`, or `None` when the
/// layer has no enabled vector mask (use the pixel mask directly). Values outside the computed
/// area are the surface's default.
pub fn combined_mask(layer: &Layer, canvas: Rect) -> Option<Surface> {
    let vm = layer.vector_mask.as_ref().filter(|v| v.enabled)?;
    let k = key(layer, canvas);
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.tick += 1;
        let tick = c.tick;
        if let Some(e) = c.map.get_mut(&k) {
            e.1 = tick;
            return Some(e.0.clone());
        }
    }
    // Far outside the path the vector mask is constant.
    let far = Rect::from_xywh(canvas.x0 - 1_000_000, canvas.y0 - 1_000_000, 1, 1);
    let v_out = photocraft_vector::vector_mask_values(vm, far)[0];
    let pixel = layer.mask.as_ref().filter(|m| m.enabled);
    let p_def = pixel.map_or(1.0, |m| {
        let d = m.surface.default_pixel().first().copied().unwrap_or(1.0);
        1.0 - m.density * (1.0 - d)
    });
    // Where the product can differ from `p_def × v_out`: the path's bounds, plus the pixel
    // mask's painted tiles when the vector mask lets them through outside the path.
    let mut area = match vm.path.control_bounds() {
        Some((x0, y0, x1, y1)) => Rect::new(x0.floor() as i32 - 2, y0.floor() as i32 - 2, x1.ceil() as i32 + 2, y1.ceil() as i32 + 2),
        None => Rect::EMPTY,
    };
    if let Some(m) = pixel
        && v_out > 0.0
    {
        let b = m.surface.content_bounds();
        area = if area.is_empty() {
            b
        } else if b.is_empty() {
            area
        } else {
            area.union(&b)
        };
    }
    let area = area.intersect(&canvas);
    let mut s = Surface::with_default(FORMAT, &[p_def * v_out]);
    if !area.is_empty() {
        let mut v = photocraft_vector::vector_mask_values(vm, area);
        if let Some(m) = pixel {
            let mut pm = Vec::new();
            m.values_into(area, &mut pm);
            for (a, b) in v.iter_mut().zip(pm) {
                *a *= b;
            }
        }
        s.write_region(area, &v);
    }
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.map.len() >= CAPACITY
        && let Some(old) = c.map.iter().min_by_key(|e| e.1.1).map(|e| *e.0)
    {
        c.map.remove(&old);
    }
    let tick = c.tick;
    c.map.insert(k, (s.clone(), tick));
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::vector::{Knot, Path, Subpath, VectorMask};

    #[test]
    fn matches_the_cpu_mask_and_is_cached() {
        let mut l = Layer::raster("l", PixelFormat::RGBA8);
        let pts = [(5.0, 5.0), (30.0, 6.0), (20.0, 28.0)];
        let knots = pts.iter().map(|&(x, y)| Knot::corner(x, y)).collect();
        let mut path = Path::default();
        path.subpaths.push(Subpath { knots, closed: true, ..Default::default() });
        let mut vm = VectorMask::new(path);
        vm.density = 0.9;
        l.vector_mask = Some(vm);
        let canvas = Rect::new(0, 0, 40, 32);
        let s = combined_mask(&l, canvas).unwrap();
        let want = photocraft_vector::vector_mask_values(l.vector_mask.as_ref().unwrap(), canvas);
        let mut got = Vec::new();
        s.read_region_into(canvas, &mut got);
        for (a, b) in got.iter().zip(&want) {
            assert!((a - b).abs() < 1e-6);
        }
        let again = combined_mask(&l, canvas).unwrap();
        assert!(s.tiles().zip(again.tiles()).all(|(a, b)| Arc::ptr_eq(a.1, b.1)));
        l.vector_mask = None;
        assert!(combined_mask(&l, canvas).is_none());
    }
}
