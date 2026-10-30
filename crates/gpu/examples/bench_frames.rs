//! Interactive-frame GPU costs on a manga-like page: full refresh, brush dabs, opacity scrub,
//! visibility toggle and a Move drag, each a synchronized `Compositor::render` of the damage the
//! app would refresh. Wall time with the GPU waited for (on a software adapter such as lavapipe
//! that is the GPU time; compare runs relative to each other).
//!
//! `cargo run --release -p photocraft-gpu --example bench_frames -- [--size 2000x3000] [--layers 50] [--reps 8] [--only a,b]`
use std::time::Instant;

use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Document, Effect, FxCommon, Layer, LayerMask, Shadow, Size};
use photocraft_geom::Rect;
use photocraft_gpu::{Compositor, Stats, Translated, surface_fingerprint};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u32) -> i32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % u64::from(n.max(1))) as i32
    }
}

/// White page; every fifth layer a full-page tone (Multiply 30 %), the rest panels and ink;
/// some with masks, a drop shadow, other blend modes; every other block of ten in a group.
pub fn manga(w: u32, h: u32, layers: usize) -> Document {
    let mut d = Document::with_background("page", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    let mut rng = Rng(7);
    let mut block: Vec<Layer> = Vec::new();
    for i in 0..layers {
        let mut l = Layer::raster(format!("L{i}"), fmt);
        let s = l.surface_mut().expect("raster");
        if i % 5 == 0 {
            let g = 0.55 + rng.next(30) as f32 / 100.0;
            s.fill_rect(Rect::new(0, 0, w as i32, h as i32), &photocraft_raster::from_rgba(&fmt, [g, g, g * 0.9, 1.0]));
            l.blend = BlendMode::Multiply;
            l.opacity = 0.3;
        } else {
            let (rw, rh) = (300 + rng.next(900), 300 + rng.next(1200));
            let (x, y) = (rng.next(w.saturating_sub(rw as u32).max(1)), rng.next(h.saturating_sub(rh as u32).max(1)));
            let c = [rng.next(255) as f32 / 255.0, rng.next(255) as f32 / 255.0, rng.next(255) as f32 / 255.0, 1.0];
            s.fill_rect(Rect::new(x, y, x + rw, y + rh), &photocraft_raster::from_rgba(&fmt, c));
            for _ in 0..12 {
                let (sx, sy) = (rng.next(w), rng.next(h));
                s.fill_rect(Rect::new(sx, sy, sx + 4 + rng.next(300), sy + 3 + rng.next(8)), &photocraft_raster::from_rgba(&fmt, [0.07, 0.07, 0.07, 1.0]));
            }
            if i % 7 == 3 {
                let mut m = LayerMask::reveal_all();
                m.surface.fill_rect(Rect::new(x + rw / 4, y, x + rw / 2, y + rh), &[0.0]);
                l.mask = Some(m);
            }
            if i % 6 == 4 {
                l.blend = [BlendMode::Screen, BlendMode::Overlay, BlendMode::Darken][i % 3];
            }
            if i % 11 == 9 {
                l.effects.items = vec![Effect::DropShadow(Shadow {
                    common: FxCommon::new(BlendMode::Multiply, 0.6),
                    color: Color::rgb(0.0, 0.0, 0.0),
                    angle: 120.0,
                    use_global_light: false,
                    distance: 8.0,
                    spread: 0.0,
                    size: 10.0,
                    contour: photocraft_doc::Contour::Linear,
                    anti_alias: false,
                    noise: 0.0,
                    knocks_out: true,
                })];
            }
        }
        block.push(l);
        if block.len() == 10 {
            let children = std::mem::take(&mut block);
            if (i / 10) % 2 == 1 {
                let mut g = Layer::group(format!("G{}", i / 10), children);
                if (i / 10) % 4 == 3 {
                    g.blend = BlendMode::Normal;
                    g.opacity = 0.9;
                }
                d.layers.push(g);
            } else {
                d.layers.extend(children);
            }
        }
    }
    d.layers.extend(block);
    d
}

/// The root-level layer index nearest the middle of the stack that is a plain raster layer.
fn mid(d: &Document) -> usize {
    let n = d.layers.len();
    (0..n).map(|k| (n / 2 + k) % n).find(|&i| d.layers[i].surface().is_some() && d.layers[i].effects.items.is_empty()).unwrap_or(n - 1)
}

fn sync(device: &wgpu::Device) {
    let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
}

static LOD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Damage at least this large renders at `--lod` (like the app's `LOD_MIN_PX`).
const LOD_MIN_PX: u64 = 512 * 512;

fn render(comp: &mut Compositor, device: &wgpu::Device, queue: &wgpu::Queue, doc: &Document, r: Rect) -> Res<(f64, Stats)> {
    let big = u64::from(r.width()) * u64::from(r.height()) >= LOD_MIN_PX;
    let level = if big { LOD.load(std::sync::atomic::Ordering::Relaxed) } else { 0 };
    let t = Instant::now();
    let stats = comp.render_lod(device, queue, doc, r, level, |_, _| {})?;
    sync(device);
    Ok((t.elapsed().as_secs_f64() * 1000.0, stats))
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(((v.len() as f64 * p).ceil() as usize).clamp(1, v.len().max(1)) - 1).copied().unwrap_or(0.0)
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    let (w, h) =
        arg(&args, "--size").and_then(|s| s.split_once('x').map(|(a, b)| (a.parse().unwrap_or(2000), b.parse().unwrap_or(3000)))).unwrap_or((2000, 3000));
    let layers: usize = arg(&args, "--layers").and_then(|v| v.parse().ok()).unwrap_or(50);
    let reps: usize = arg(&args, "--reps").and_then(|v| v.parse().ok()).unwrap_or(8).max(1);
    let only = arg(&args, "--only");
    photocraft_gpu::plan::set_fusion(!args.iter().any(|a| a == "--no-fuse"));
    LOD.store(arg(&args, "--lod").and_then(|v| v.parse().ok()).unwrap_or(0), std::sync::atomic::Ordering::Relaxed);

    let want = |n: &str| only.as_ref().is_none_or(|o| o.split(',').any(|x| x == n));
    let adapter = pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
    let format = Compositor::preferred_acc_format(&adapter);
    let mut doc = manga(w, h, layers);
    let mut comp = Compositor::try_new_with_format(&device, format)?;
    if args.iter().any(|a| a == "--no-reuse") {
        comp.set_composite_cache_budget(0);
    }
    let plan = photocraft_gpu::plan(&doc)?;
    println!(
        "adapter={} ({:?}) acc={format:?} doc={w}x{h} layers={layers} passes={} slots={}",
        adapter.get_info().name,
        adapter.get_info().backend,
        plan.passes.len(),
        plan.slots
    );
    if args.iter().any(|a| a == "--dump-plan") {
        for (i, p) in plan.passes.iter().enumerate() {
            println!("{i:3} {:?} dst {} a {:?} b {:?} clip {:?} members {}", p.kernel, p.dst, p.a, p.b, p.clip, p.members.len());
        }
    }
    drop(plan);
    let (cold, _) = render(&mut comp, &device, &queue, &doc, doc.bounds())?;
    println!("{:<14} {:>9.1} ms", "cold", cold);
    let report = |name: &str, mut v: Vec<f64>, st: &Stats| {
        let p50 = pct(&mut v, 0.5);
        let p95 = pct(&mut v, 0.95);
        println!(
            "{name:<14} p50 {p50:>8.1} ms  p95 {p95:>8.1} ms  passes {} level {} px {} chunks {} uploads {} prefix_hits {} skipped {}",
            st.passes, st.level, st.pixels, st.chunks, st.tiles_uploaded, st.prefix_hits, st.prefix_passes_skipped
        );
    };
    let m = mid(&doc);
    let bounds = photocraft_gpu::bounds::composite_bounds(&doc.layers[m], doc.bounds()).unwrap_or(doc.bounds());
    if want("full") {
        let mut v = Vec::new();
        let mut st = Stats::default();
        for _ in 0..reps {
            let (ms, s) = render(&mut comp, &device, &queue, &doc, doc.bounds())?;
            v.push(ms);
            st = s;
        }
        report("full", v, &st);
    }
    if want("brush") {
        // 60 px dabs along a diagonal on the middle layer, each frame refreshing its damage.
        let fmt = doc.pixel_format();
        let mut v = Vec::new();
        let mut st = Stats::default();
        let before = doc.layers[m].clone();
        for f in 0..reps * 4 {
            let (x, y) = (w as i32 / 5 + f as i32 * 23, h as i32 / 4 + f as i32 * 31);
            let dab = Rect::new(x, y, x + 60, y + 60);
            doc.layers[m].surface_mut().expect("raster").fill_rect(dab, &photocraft_raster::from_rgba(&fmt, [0.1, 0.1, 0.1, 1.0]));
            let (ms, s) = render(&mut comp, &device, &queue, &doc, dab)?;
            v.push(ms);
            st = s;
        }
        doc.layers[m] = before;
        report("brush", v, &st);
    }
    if want("opacity") {
        let mut v = Vec::new();
        let mut st = Stats::default();
        for f in 0..reps {
            doc.layers[m].opacity = if f % 2 == 0 { 0.6 } else { 0.8 };
            let (ms, s) = render(&mut comp, &device, &queue, &doc, bounds)?;
            v.push(ms);
            st = s;
        }
        doc.layers[m].opacity = 1.0;
        report("opacity", v, &st);
    }
    if want("visibility") {
        let mut v = Vec::new();
        let mut st = Stats::default();
        for _ in 0..reps {
            doc.layers[m].visible = !doc.layers[m].visible;
            let (ms, s) = render(&mut comp, &device, &queue, &doc, bounds)?;
            v.push(ms);
            st = s;
        }
        doc.layers[m].visible = true;
        report("visibility", v, &st);
    }
    if want("move") {
        let before = doc.clone();
        let mut v = Vec::new();
        let mut st = Stats::default();
        let src = before.layers[m].surface().expect("raster").clone();
        let mut prev = bounds;
        for f in 1..=reps * 2 {
            let off = (f as i32 * 7, f as i32 * 5);
            let moved = src.translated(off.0, off.1, src.content_bounds());
            comp.set_translated(vec![Translated {
                layer: before.layers[m].id,
                mask: false,
                shown: surface_fingerprint(&moved),
                source: src.clone(),
                offset: off,
            }]);
            *doc.layers[m].surface_mut().expect("raster") = moved;
            let now = photocraft_gpu::bounds::composite_bounds(&doc.layers[m], doc.bounds()).unwrap_or(doc.bounds());
            let (ms, s) = render(&mut comp, &device, &queue, &doc, now.union(&prev))?;
            prev = now;
            v.push(ms);
            st = s;
        }
        comp.set_translated(Vec::new());
        drop(before);
        report("move", v, &st);
    }
    Ok(())
}
