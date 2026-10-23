//! Level-of-detail composites (`Compositor::render_lod`): level 0 is the full-size render; a
//! reduced level matches the full-size composite box-filtered down (exactly at level 1 for Normal
//! layers over an opaque backdrop, within resampling differences otherwise), the same whether
//! the layers are paged or not and for any sub-region. Skips without a GPU adapter with 32-bit
//! float targets.

use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Effect, FxCommon, Layer, LayerMask, Shadow, Size};
use photocraft_geom::Rect;
use photocraft_gpu::{Compositor, level_bounds, level_rect, render_to_vec_lod};

static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    comp: Compositor,
    paged: Compositor,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn gpu() -> Option<Gpu> {
    let lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let adapter = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    if Compositor::preferred_acc_format(&adapter) != wgpu::TextureFormat::Rgba32Float {
        eprintln!("skipping: adapter can't render Rgba32Float");
        return None;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let comp = Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).ok()?;
    let mut paged = Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).ok()?;
    paged.set_texture_limit(256);
    Some(Gpu { device, queue, comp, paged, _lock: lock })
}

fn rnd(seed: u32, i: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9e37_79b9) ^ i.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535.0
}

/// Noisy pixels (fine detail a reduced composite must filter, not alias) in `rect`.
fn noise(name: &str, fmt: PixelFormat, rect: Rect, seed: u32) -> Layer {
    let mut l = Layer::raster(name, fmt);
    let n = rect.width() * rect.height();
    let data: Vec<f32> = (0..n).flat_map(|i| [rnd(seed, i), rnd(seed + 1, i), rnd(seed + 2, i), 0.3 + 0.7 * rnd(seed + 3, i)]).collect();
    l.surface_mut().unwrap().write_region(rect, &data);
    l
}

/// Normal layers only, side by side over white: over a constant backdrop compositing is linear
/// in the (premultiplied) layer, so a box-filtered layer composites to the box-filtered result.
/// (Over a varying backdrop it is not: stacked layers differ by resampling.)
fn normal_doc() -> Document {
    let mut d = Document::with_background("normal", Size::new(600, 520), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    d.layers.push(noise("a", fmt, Rect::new(20, 30, 590, 190), 11));
    let mut b = noise("b", fmt, Rect::new(100, 200, 300, 500), 23);
    b.opacity = 0.7;
    let mut m = LayerMask::reveal_all();
    let mdata: Vec<f32> = (0..200 * 300).map(|i| rnd(5, i)).collect();
    m.surface.write_region(Rect::new(100, 200, 300, 500), &mdata);
    b.mask = Some(m);
    d.layers.push(b);
    d
}

/// Everything else a page has: blend modes, a group, an effect, a small layer.
fn mixed_doc() -> Document {
    let mut d = normal_doc();
    let fmt = d.pixel_format();
    let mut tone = Layer::raster("tone", fmt);
    tone.surface_mut().unwrap().fill_rect(d.bounds(), &photocraft_raster::from_rgba(&fmt, [0.6, 0.8, 0.7, 1.0]));
    tone.blend = BlendMode::Multiply;
    tone.opacity = 0.5;
    let mut screen = noise("screen", fmt, Rect::new(300, 10, 560, 300), 41);
    screen.blend = BlendMode::Screen;
    let mut small = Layer::raster("small", fmt);
    small.surface_mut().unwrap().fill_rect(Rect::new(37, 41, 75, 90), &photocraft_raster::from_rgba(&fmt, [0.1, 0.2, 0.9, 1.0]));
    small.effects.items = vec![Effect::DropShadow(Shadow {
        common: FxCommon::new(BlendMode::Multiply, 0.75),
        color: Color::rgb(0.0, 0.0, 0.0),
        angle: 120.0,
        use_global_light: false,
        distance: 6.0,
        spread: 0.0,
        size: 5.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: false,
        noise: 0.0,
        knocks_out: true,
    })];
    let mut group = Layer::group("g", vec![screen, small]);
    group.blend = BlendMode::Normal;
    group.opacity = 0.9;
    d.layers.push(tone);
    d.layers.push(group);
    d
}

/// The CPU composite box-filtered down to `level` (premultiplied), as the canvas mips hold it.
fn reduced_cpu(doc: &Document, level: u32) -> Vec<[f32; 4]> {
    let full = photocraft_compose::flatten(doc);
    let (w, h) = (doc.size.width as usize, doc.size.height as usize);
    let s = 1usize << level;
    let lb = level_bounds(doc.bounds(), level);
    let (lw, lh) = (lb.width() as usize, lb.height() as usize);
    let mut out = vec![[0.0f32; 4]; lw * lh];
    for y in 0..lh {
        for x in 0..lw {
            let mut acc = [0.0f32; 4];
            for dy in 0..s {
                for dx in 0..s {
                    let p = full.px[(y * s + dy).min(h - 1) * w + (x * s + dx).min(w - 1)];
                    acc[0] += p[0] * p[3];
                    acc[1] += p[1] * p[3];
                    acc[2] += p[2] * p[3];
                    acc[3] += p[3];
                }
            }
            let n = (s * s) as f32;
            out[y * lw + x] = acc.map(|v| v / n);
        }
    }
    out
}

fn premul(v: &[[f32; 4]]) -> Vec<[f32; 4]> {
    v.iter().map(|p| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]).collect()
}

fn diffs(a: &[[f32; 4]], b: &[[f32; 4]]) -> (f32, f32) {
    let mut worst = 0.0f32;
    let mut at = 0;
    let mut sum = 0.0f32;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        for k in 0..4 {
            let d = (x[k] - y[k]).abs();
            if d > worst {
                worst = d;
                at = i;
            }
            sum += d;
        }
    }
    if worst > 2.0 / 255.0 {
        eprintln!("worst at pixel {at}: {:?} vs {:?}", a.get(at), b.get(at));
    }
    (worst, sum / (a.len() * 4).max(1) as f32)
}

#[test]
fn level_zero_is_the_full_render() {
    let Some(mut g) = gpu() else { return };
    let d = mixed_doc();
    let full = photocraft_gpu::render_to_vec(&mut g.comp, &g.device, &g.queue, &d, d.bounds()).unwrap();
    let (lod0, st) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, d.bounds(), 0).unwrap();
    assert_eq!(st.level, 0);
    assert_eq!(diffs(&full, &lod0).0, 0.0);
}

#[test]
fn level_one_of_normal_layers_is_the_box_filtered_composite() {
    let Some(mut g) = gpu() else { return };
    let d = normal_doc();
    let (got, st) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, d.bounds(), 1).unwrap();
    assert_eq!(st.level, 1);
    assert_eq!(st.pixels, 300 * 260);
    let (worst, mean) = diffs(&premul(&got), &reduced_cpu(&d, 1));
    assert!(worst <= 2.0 / 255.0, "level 1: worst {:.2}/255 mean {:.4}/255", worst * 255.0, mean * 255.0);
}

#[test]
fn reduced_levels_differ_only_by_resampling() {
    let Some(mut g) = gpu() else { return };
    let d = mixed_doc();
    for level in 1..=3 {
        let (got, st) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, d.bounds(), level).unwrap();
        let lb = level_bounds(d.bounds(), level);
        assert_eq!(st.pixels, u64::from(lb.width()) * u64::from(lb.height()), "level {level}");
        let (worst, mean) = diffs(&premul(&got), &reduced_cpu(&d, level));
        eprintln!("level {level}: worst {:.1}/255 mean {:.2}/255", worst * 255.0, mean * 255.0);
        // Up to 4 × 4 taps per output pixel (exact through level 2, a subsample of the noise
        // above), and the nonlinear modes blend filtered layers: small on average, bounded.
        let limit = if level <= 2 { 3.0 } else { 12.0 };
        assert!(mean <= limit / 255.0, "level {level}: mean {:.2}/255", mean * 255.0);
        assert!(worst <= 0.5, "level {level}: worst {:.1}/255", worst * 255.0);
    }
}

#[test]
fn reduced_levels_are_the_same_paged_and_for_any_region() {
    let Some(mut g) = gpu() else { return };
    let d = mixed_doc();
    for level in [1, 2, 4] {
        let (whole, _) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, d.bounds(), level).unwrap();
        let (paged, st) = render_to_vec_lod(&mut g.paged, &g.device, &g.queue, &d, d.bounds(), level).unwrap();
        assert!(st.cells > 1, "pages of 256 px");
        // Paged effect maps are built per cell (exact inside it, up to float rounding).
        assert!(diffs(&whole, &paged).0 <= 1.0 / 255.0, "paged, level {level}");
        let lb = level_bounds(d.bounds(), level);
        for region in [Rect::new(33, 17, 401, 333), Rect::new(250, 250, 270, 260), Rect::new(512, 0, 600, 520)] {
            let (part, _) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, region, level).unwrap();
            let lr = level_rect(region, level).intersect(&lb);
            assert_eq!(part.len(), (lr.width() * lr.height()) as usize);
            let w = lb.width() as usize;
            for y in 0..lr.height() as usize {
                for x in 0..lr.width() as usize {
                    let a = part[y * lr.width() as usize + x];
                    let b = whole[(lr.y0 as usize + y) * w + lr.x0 as usize + x];
                    assert_eq!(a, b, "level {level} region {region:?} at {x},{y}");
                }
            }
        }
    }
}

#[test]
fn level_rects_round_out() {
    assert_eq!(level_rect(Rect::new(1, 2, 3, 5), 1), Rect::new(0, 1, 2, 3));
    assert_eq!(level_rect(Rect::new(-3, 0, 16, 17), 2), Rect::new(-1, 0, 4, 5));
    assert_eq!(level_rect(Rect::new(5, 5, 6, 6), 0), Rect::new(5, 5, 6, 6));
    assert_eq!(level_bounds(Rect::new(0, 0, 2001, 3), 3), Rect::new(0, 0, 250, 1));
}

#[test]
fn reduced_renders_reuse_the_reduced_lower_stack() {
    let Some(mut g) = gpu() else { return };
    let mut d = mixed_doc();
    let top = d.layers.len() - 1;
    for level in [2, 1, 2] {
        render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, d.bounds(), level).unwrap();
        for (k, op) in [0.3f32, 0.6, 0.45].into_iter().enumerate() {
            d.layers[top].opacity = op;
            let (got, st) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, d.bounds(), level).unwrap();
            let mut fresh = Compositor::try_new_with_format(&g.device, wgpu::TextureFormat::Rgba32Float).unwrap();
            fresh.set_composite_cache_budget(0);
            let (want, _) = render_to_vec_lod(&mut fresh, &g.device, &g.queue, &d, d.bounds(), level).unwrap();
            assert!(diffs(&got, &want).0 <= 1e-5, "level {level} edit {k}: cached differs");
            if k > 0 {
                assert!(st.prefix_hits > 0, "level {level} edit {k}: the lower stack is reused");
            }
            // A damage rect alone, from the cache.
            let r = Rect::new(120, 90, 333, 260);
            let (part, _) = render_to_vec_lod(&mut g.comp, &g.device, &g.queue, &d, r, level).unwrap();
            let lr = level_rect(r, level).intersect(&level_bounds(d.bounds(), level));
            let w = level_bounds(d.bounds(), level).width() as usize;
            for y in 0..lr.height() as usize {
                for x in 0..lr.width() as usize {
                    let (a, b) = (part[y * lr.width() as usize + x], want[(lr.y0 as usize + y) * w + lr.x0 as usize + x]);
                    assert!((0..4).all(|c| (a[c] - b[c]).abs() <= 1e-5), "level {level} damage at {x},{y}");
                }
            }
        }
    }
}
