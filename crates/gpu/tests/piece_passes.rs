//! A piece of a page of many styled layers binds only the passes that draw in it: their layer
//! pages, uniforms and bind groups. A catch-up piece of the owner's 200-layer page bound every
//! layer's passes (two to three per styled layer) wherever the piece was, so recording one piece
//! cost about as much as recording the whole page (gpu.record 256 to 319 ms in a frame of a crop's
//! undo on an RTX 3060 Ti). The piece renders exactly as the same rect of the whole page does.

use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{Document, Effect, Layer, Size};
use photocraft_geom::Rect;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let format = photocraft_gpu::Compositor::preferred_acc_format(&adapter);
    Some((device, queue, format))
}

/// 2000x3000 with 120 small layers spread over it, every other one with a drop shadow.
fn page() -> Document {
    let mut d = Document::with_background("page", Size::new(2000, 3000), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    for i in 0..120 {
        let (x, y) = (40 + (i % 10) * 196, 40 + (i / 10) * 245);
        let mut l = Layer::raster(&format!("bubble {i}"), fmt);
        let c = [0.2 + 0.006 * i as f32, 0.4, 0.8 - 0.005 * i as f32, 1.0];
        l.surface_mut().expect("raster").fill_rect(Rect::new(x, y, x + 120, y + 90), &photocraft_raster::from_rgba(&fmt, c));
        if i % 2 == 0 {
            l.effects.items.push(Effect::default_drop_shadow());
        }
        d.layers.push(l);
    }
    d
}

fn worst(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max)
}

#[test]
fn a_piece_binds_only_the_passes_that_draw_in_it() {
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page();
    let mut comp = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    let (whole, _) = match photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()) {
        Ok(v) => v,
        // GL without float render targets draws layer effects on the CPU.
        Err(e) => {
            eprintln!("skipping: {}", e.0);
            return;
        }
    };
    // A 256 px piece in the page's middle, as a catch-up fills a page in.
    let piece = Rect::new(900, 1400, 1156, 1656);
    let mut fresh = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    // (As the catch-up draws it: the piece alone, not the chunks around it for the lower-stack cache.)
    fresh.set_fill_prefix(false);
    comp.set_fill_prefix(false);
    let (got, st) = photocraft_gpu::render_to_vec_stats(&mut fresh, &device, &queue, &doc, piece).expect("piece");
    // (What a piece costs once the pages and maps are made, as when a page fills in: printed.)
    let mut ms = Vec::new();
    let mut hits = 0;
    for i in 0..5 {
        let p = piece.translate(-700 + 300 * i, -1000 + 450 * i);
        let t = std::time::Instant::now();
        hits += comp.render(&device, &queue, &doc, p, |_, _| {}).expect("piece").prefix_hits;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!("PIECE 256 px of {} passes: {} bound, render calls {ms:.1?} ms ({hits} lower-stack hits)", st.passes, st.bound);
    assert!(st.passes > 150, "the page plans a pass or more per layer ({} passes)", st.passes);
    assert!(st.bound * 5 < st.passes, "the piece bound {} of {} passes", st.bound, st.passes);
    let w = doc.size.width as usize;
    let want: Vec<[f32; 4]> =
        (piece.y0..piece.y1).flat_map(|y| (piece.x0..piece.x1).map(move |x| (x, y))).map(|(x, y)| whole[y as usize * w + x as usize]).collect();
    let off = worst(&want, &got);
    assert!(off < 1e-5, "the piece as the whole page has it: off by {off}");
}
