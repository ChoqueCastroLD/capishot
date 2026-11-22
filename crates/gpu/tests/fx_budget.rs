//! Effect maps on a big page of many styled layers within a small effect budget (a 4 GB device's
//! 32 MB): a render builds the maps of the layers it draws only, a budget too small for every
//! layer's whole state keeps their maps (lets go of their distance fields and CPU shapes first),
//! and drawing the page again, or a part of it, builds no map again. Both match a compositor with
//! room for everything.

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

/// 2400x3600, 24 rectangles with two drop shadows (one spread), spread over it.
fn page() -> Document {
    let mut d = Document::with_background("page", Size::new(2400, 3600), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    for i in 0..24 {
        let (x, y) = (60 + (i % 4) * 580, 60 + (i / 4) * 590);
        let mut l = Layer::raster(&format!("balloon {i}"), fmt);
        l.surface_mut().expect("raster").fill_rect(Rect::new(x, y, x + 420, y + 380), &photocraft_raster::from_rgba(&fmt, [0.3, 0.4, 0.8, 1.0]));
        l.effects.items.push(Effect::default_drop_shadow());
        // A spread shadow: a distance field besides its map.
        if let Effect::DropShadow(mut s) = Effect::default_drop_shadow() {
            s.spread = 0.5;
            s.size = 12.0;
            s.distance = 9.0;
            l.effects.items.push(Effect::DropShadow(s));
        }
        d.layers.push(l);
    }
    d
}

fn worst(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max)
}

#[test]
fn a_small_effect_budget_keeps_the_maps_and_builds_none_again() {
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page();
    let view = Rect::new(0, 0, 1000, 1000);
    let mut roomy = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    let want_view = match photocraft_gpu::render_to_vec_stats(&mut roomy, &device, &queue, &doc, view) {
        Ok((v, _)) => v,
        // GL without float render targets draws layer effects on the CPU.
        Err(e) => {
            eprintln!("skipping: {}", e.0);
            return;
        }
    };
    let (want_all, _) = photocraft_gpu::render_to_vec_stats(&mut roomy, &device, &queue, &doc, doc.bounds()).expect("reference page");

    let mut comp = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    // A 4 GB device's: 256 MB, a quarter for caches, half of it effect maps.
    comp.set_memory_budget(256 << 20);
    // A view of the page's top left builds the maps of the layers it shows: 4 of the 24.
    let b0 = photocraft_gpu::fx_builds();
    let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, view).expect("view");
    let built = photocraft_gpu::fx_builds() - b0;
    assert!(worst(&want_view, &got) <= 1.0 / 255.0, "the view differs by {:.2}/255", worst(&want_view, &got) * 255.0);
    assert!(built <= 4, "a view of 4 styled layers built the effect maps of {built} layers");
    // The whole page, twice: the second time builds nothing (the maps were kept within the
    // budget, their distance fields and shapes let go).
    let b1 = photocraft_gpu::fx_builds();
    let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).expect("page");
    assert!(worst(&want_all, &got) <= 1.0 / 255.0, "the page differs by {:.2}/255", worst(&want_all, &got) * 255.0);
    let first = photocraft_gpu::fx_builds() - b1;
    let b2 = photocraft_gpu::fx_builds();
    let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).expect("page again");
    let again = photocraft_gpu::fx_builds() - b2;
    eprintln!("effect maps built: view {built}, page {first}, page again {again}; cache {} MB", comp.fx_cache_bytes() >> 20);
    assert!(worst(&want_all, &got) <= 1.0 / 255.0, "the page again differs by {:.2}/255", worst(&want_all, &got) * 255.0);
    assert_eq!(again, 0, "drawing the page again built effect maps again");
    // The view again too.
    let b3 = photocraft_gpu::fx_builds();
    let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, view).expect("view again");
    assert!(worst(&want_view, &got) <= 1.0 / 255.0);
    assert_eq!(photocraft_gpu::fx_builds() - b3, 0, "the view again built effect maps");
}
