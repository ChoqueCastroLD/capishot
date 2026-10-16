//! A brush stroke on a layer below the lower-stack cache's checkpoint: the first dab in a chunk
//! renders the whole chunk (filling the cache), the dabs after it there reuse the lower stack,
//! and every dab matches the CPU compositor. Skips without a GPU adapter with float targets.

use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_geom::Rect;
use photocraft_gpu::{Compositor, render_to_vec_stats};

#[test]
fn moving_dabs_reuse_the_lower_stack_after_the_first() {
    let Ok(adapter) = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())) else { return };
    if Compositor::preferred_acc_format(&adapter) != wgpu::TextureFormat::Rgba32Float {
        return;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut doc = Document::with_background("dabs", Size::new(900, 700), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = doc.pixel_format();
    for i in 0..12 {
        let mut l = Layer::raster(format!("l{i}"), fmt);
        let c = [0.1 + 0.07 * i as f32, 0.5, 0.9 - 0.06 * i as f32, 0.6];
        l.surface_mut().unwrap().fill_rect(Rect::new(i * 40, i * 30, 900 - i * 20, 700 - i * 25), &photocraft_raster::from_rgba(&fmt, c));
        l.blend = [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen][i as usize % 3];
        doc.layers.push(l);
    }
    let mut comp = Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).unwrap();
    render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).unwrap();
    let target = 9;
    let mut hits = 0;
    for f in 0..8 {
        let (x, y) = (100 + f * 37, 120 + f * 29);
        let dab = Rect::new(x, y, x + 24, y + 24);
        doc.layers[target].surface_mut().unwrap().fill_rect(dab, &photocraft_raster::from_rgba(&fmt, [0.05, 0.05, 0.05, 1.0]));
        let (got, st) = render_to_vec_stats(&mut comp, &device, &queue, &doc, dab).unwrap();
        hits += st.prefix_hits;
        let cpu = photocraft_compose::render(&doc, dab);
        let worst = cpu.px.iter().zip(&got).map(|(c, g)| (0..4).map(|k| (c[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 2.0 / 255.0, "dab {f}: {:.2}/255", worst * 255.0);
    }
    assert!(hits >= 6, "{hits} dabs reused the lower stack");
}
