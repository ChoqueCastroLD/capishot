//! Pass fusion (`plan::fuse_layer_blends`, `fuse_layer_runs`) changes how many passes run, not
//! what they compute: documents render the same with it on and off, at full size and reduced,
//! paged or not, and with lower-stack reuse warm. One test per binary: fusion is a global switch.

use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Fill, Layer, LayerContent, LayerMask, Size};
use photocraft_geom::Rect;
use photocraft_gpu::{Compositor, plan, render_to_vec_lod};

fn rnd(seed: u32, i: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9e37_79b9) ^ i.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535.0
}

fn noise(name: &str, fmt: PixelFormat, rect: Rect, seed: u32) -> Layer {
    let mut l = Layer::raster(name, fmt);
    let n = rect.width() * rect.height();
    let data: Vec<f32> = (0..n).flat_map(|i| [rnd(seed, i), rnd(seed + 1, i), rnd(seed + 2, i), 0.2 + 0.8 * rnd(seed + 3, i)]).collect();
    l.surface_mut().unwrap().write_region(rect, &data);
    l
}

/// Many plain layers (whole-canvas and small, every blend mode family, masks, opacity, fill,
/// Dissolve, a solid fill layer), with a group and a clipping unit breaking the runs.
fn stack(depth: SampleType) -> Document {
    let mut d = Document::with_background("stack", Size::new(700, 600), ColorMode::Rgb, depth, Color::WHITE);
    let fmt = d.pixel_format();
    let modes = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::ColorDodge,
        BlendMode::Difference,
        BlendMode::Hue,
        BlendMode::Dissolve,
        BlendMode::LinearBurn,
        BlendMode::Luminosity,
    ];
    for i in 0..23u32 {
        let r = if i % 4 == 0 {
            d.bounds()
        } else {
            Rect::new((i * 37 % 500) as i32, (i * 53 % 400) as i32, (i * 37 % 500 + 60 + i * 7) as i32, (i * 53 % 400 + 50 + i * 5) as i32)
        };
        let mut l = noise(&format!("l{i}"), fmt, r, 100 + i * 10);
        l.blend = modes[i as usize % modes.len()];
        l.opacity = 0.5 + 0.5 * rnd(7, i);
        if i % 3 == 1 {
            l.fill_opacity = 0.6;
        }
        if i % 5 == 2 {
            let mut m = LayerMask::reveal_all();
            let data: Vec<f32> = (0..r.width() * r.height()).map(|k| rnd(900 + i, k)).collect();
            m.surface.write_region(r, &data);
            m.density = 0.9;
            l.mask = Some(m);
        }
        d.layers.push(l);
        if i == 9 {
            d.layers.push(Layer::new("solid", LayerContent::Fill(Fill::Solid(Color::rgba(0.2, 0.5, 0.8, 0.4)))));
            let mut c = noise("clipped", fmt, Rect::new(10, 10, 400, 300), 77);
            c.clipped = true;
            d.layers.push(c);
        }
        if i == 15 {
            let mut g = Layer::group("g", vec![noise("g0", fmt, Rect::new(50, 50, 300, 300), 61), noise("g1", fmt, Rect::new(200, 100, 650, 500), 62)]);
            g.blend = BlendMode::Normal;
            g.opacity = 0.8;
            d.layers.push(g);
        }
    }
    d
}

fn worst(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (0..4).map(|k| (x[k] - y[k]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max)
}

#[test]
fn fused_and_unfused_plans_render_the_same() {
    let Ok(adapter) = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("skipping: no adapter");
        return;
    };
    if Compositor::preferred_acc_format(&adapter) != wgpu::TextureFormat::Rgba32Float {
        eprintln!("skipping: adapter can't render Rgba32Float");
        return;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut d = stack(depth);
        plan::set_fusion(true);
        let fused = plan::plan(&d).unwrap();
        assert!(fused.passes.iter().any(|p| p.kernel == plan::Kernel::Layers), "runs are fused");
        let n_fused = fused.passes.len();
        drop(fused);
        plan::set_fusion(false);
        let n_plain = plan::plan(&d).unwrap().passes.len();
        assert!(n_fused * 3 < n_plain * 2, "{n_fused} fused passes vs {n_plain}");
        for (limit, level) in [(None, 0), (Some(256), 0), (None, 1), (Some(256), 2)] {
            let mut out = Vec::new();
            for fuse in [false, true] {
                plan::set_fusion(fuse);
                let mut comp = Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).unwrap();
                if let Some(l) = limit {
                    comp.set_texture_limit(l);
                }
                let (v, _) = render_to_vec_lod(&mut comp, &device, &queue, &d, d.bounds(), level).unwrap();
                // Warm lower-stack reuse: an edit near the top renders from the cached prefix.
                let top = d.layers.len() - 2;
                let was = d.layers[top].opacity;
                d.layers[top].opacity = 0.33;
                let (warm, _) = render_to_vec_lod(&mut comp, &device, &queue, &d, d.bounds(), level).unwrap();
                let (again, st) = render_to_vec_lod(&mut comp, &device, &queue, &d, Rect::new(100, 100, 600, 500), level).unwrap();
                d.layers[top].opacity = was;
                if level == 0 && limit.is_none() {
                    assert!(st.prefix_hits > 0, "fuse {fuse}: prefix reused");
                }
                out.push((v, warm, again));
            }
            let (a, b) = (&out[0], &out[1]);
            for (name, x, y) in [("cold", &a.0, &b.0), ("warm", &a.1, &b.1), ("region", &a.2, &b.2)] {
                let w = worst(x, y);
                if w > 1e-5 {
                    let at = x.iter().zip(y.iter()).position(|(p, q)| (0..4).any(|k| (p[k] - q[k]).abs() > 1e-5)).unwrap_or(0);
                    panic!("{depth:?} limit {limit:?} level {level} {name}: fused differs by {w}, first at {at}: {:?} vs {:?}", x[at], y[at]);
                }
            }
        }
    }
    plan::set_fusion(true);
}
