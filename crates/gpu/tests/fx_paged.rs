//! Effect maps of layers larger than a page cell are kept per cell and let go least recently
//! used first, within the effect budget the device's memory sets. An 8K strip (7680x12000) of
//! styled shapes thousands of pixels wide held each layer's maps whole: one layer's shape,
//! distance fields and maps were hundreds of megabytes (more than a 4 GB device's whole effect
//! budget), the cache could only drop whole layers, and a frame built one in one go (311-443 ms
//! of gpu.fx on lavapipe, a peak of 8.7 GB for the strip). Prepared ahead, a big layer's maps are
//! built a cell at a time, each in bands.

use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{BlendMode, Document, Effect, FxCommon, FxPaint, Layer, Size, StrokeFx, StrokePosition};
use photocraft_geom::Rect;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let format = photocraft_gpu::Compositor::preferred_acc_format(&adapter);
    Some((device, queue, format))
}

fn worst(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max)
}

/// A `w`x`h` page with four blocks of `bw`x`bh` px apart, each on a layer with a Drop Shadow and
/// an outside Stroke (as the 8K bench's shapes).
fn page(w: u32, h: u32, (bw, bh): (i32, i32)) -> Document {
    let mut d = Document::with_background("strip", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    for i in 0..4 {
        let (x, y) = (200 + (i % 2) * (w as i32 / 2), 300 + (i / 2) * (h as i32 / 2));
        let mut l = Layer::raster(&format!("shape {i}"), fmt);
        l.surface_mut().expect("raster").fill_rect(Rect::new(x, y, x + bw, y + bh), &photocraft_raster::from_rgba(&fmt, [0.5, 0.56, 0.7, 1.0]));
        l.effects.items.push(Effect::default_drop_shadow());
        let white = FxPaint::Color(Color::WHITE);
        l.effects.items.push(Effect::Stroke(StrokeFx { common: FxCommon::new(BlendMode::Normal, 1.0), size: 8.0, position: StrokePosition::Outside, paint: white, overprint: false }));
        d.layers.push(l);
    }
    d
}

/// A strip drawn in 1024-pixel pieces (as the canvas refines a page) by a 4 GB device's
/// compositor: the effect cache stays within its budget and a cell's worth of maps, where whole
/// layers' maps went far past it.
#[test]
fn a_strips_effect_maps_stay_within_a_small_devices_budget() {
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page(7680, 12000, (3400, 5600));
    let mut comp = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    if let Err(e) = comp.supports(&doc) {
        eprintln!("skipping: {}", e.0);
        return;
    }
    // A 4 GB device's compositor budget (`gpu_canvas::memory_budget`: its sixteenth, 256 MB).
    comp.set_memory_budget(256 << 20);
    let mut peak = 0usize;
    for y in (0..12000).step_by(1024) {
        for x in (0..7680).step_by(1024) {
            let r = Rect::new(x, y, (x + 1024).min(7680), (y + 1024).min(12000));
            if let Err(e) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, r) {
                eprintln!("skipping: {}", e.0);
                return;
            }
            peak = peak.max(comp.fx_cache_bytes());
        }
    }
    eprintln!("effect cache peak {} MB", peak >> 20);
    // The budget's 48 MB, and the maps of the cell a piece draws (2048 px and the effects'
    // reach: the shape and the stroke's distance field on the CPU and the GPU, two maps): about
    // 70 MB. A layer's whole maps were 300 MB.
    assert!(peak <= 160 << 20, "the effect cache peaked at {} MB on a 4 GB device", peak >> 20);
}

/// A layer larger than a page cell prepared ahead with time for one part of the work a call:
/// each builds a part of one cell's maps (its shape and fields in bands), and the render after builds none.
/// The pixels match a compositor that built them in the render.
#[test]
fn a_layer_larger_than_a_cell_is_prepared_a_cell_at_a_time() {
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page(5000, 5000, (2400, 2400));
    let mut comp = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    // Cells of 2048 px whatever the adapter's texture limit: these layers' maps are paged.
    comp.set_texture_limit(2048);
    if let Err(e) = comp.supports(&doc) {
        eprintln!("skipping: {}", e.0);
        return;
    }
    let view = Rect::new(0, 0, 3000, 3000);
    let (mut calls, mut slowest) = (0, 0.0f64);
    loop {
        let t = std::time::Instant::now();
        let mut time = true;
        let ready = match comp.prepare(&device, &queue, &doc, view, 0, &mut || std::mem::replace(&mut time, false)) {
            Ok(r) => r,
            Err(e) if calls == 0 => {
                eprintln!("skipping: {}", e.0);
                return;
            }
            Err(e) => panic!("prepare: {}", e.0),
        };
        slowest = slowest.max(t.elapsed().as_secs_f64() * 1000.0);
        calls += 1;
        if ready || calls > 5000 {
            break;
        }
    }
    let b = photocraft_gpu::fx_builds();
    let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, view).expect("render");
    let in_render = photocraft_gpu::fx_builds() - b;
    eprintln!("prepared in {calls} calls (slowest {slowest:.1} ms), the render built {in_render} layers' maps");
    assert_eq!(in_render, 0, "the render built maps prepare left");
    assert!(calls > 20, "prepared in {calls} calls: a cell's maps in parts");
    let mut fresh = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    fresh.set_texture_limit(2048);
    let (want, _) = photocraft_gpu::render_to_vec_stats(&mut fresh, &device, &queue, &doc, view).expect("reference");
    let off = worst(&want, &got);
    assert!(off <= 1.0 / 255.0, "{:.2}/255 off the maps built in the render", off * 255.0);
}
