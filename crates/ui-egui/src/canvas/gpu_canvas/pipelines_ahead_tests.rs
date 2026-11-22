//! The compositor made ahead on a worker thread (`build_pipelines_ahead`): the telemetry's
//! `gpu.pipelines` freezes (1.3 to 2.4 s on D3D12 on the first page opened, up to 19 s on an HD
//! 520) were the compositor's shader module and first pipelines made on that page's first frame,
//! and every pipeline built later held another frame.

use super::*;
use photocraft_doc::{Document, Layer, LayerContent, Size};

/// A page with a layer with a Drop Shadow and an adjustment: kernels past the first three, and
/// effect maps.
fn page() -> Document {
    use photocraft_color::{Color, ColorMode, SampleType};
    let mut doc = Document::with_background("page", Size::new(256, 256), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = doc.pixel_format();
    let mut l = Layer::raster("fx", fmt);
    if let Some(s) = l.surface_mut() {
        s.fill_rect(photocraft_geom::Rect::new(64, 64, 192, 192), &photocraft_raster::from_rgba(&fmt, [1.0, 0.2, 0.1, 1.0]));
    }
    l.effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    doc.layers.push(l);
    doc.layers.push(Layer::new("invert", LayerContent::Adjustment(photocraft_doc::Adjustment::Invert)));
    doc
}

#[test]
fn the_compositor_is_made_on_a_worker_and_the_first_frames_build_no_pipeline() {
    let _gpu = photocraft_testkit::gpu_lock();
    let rs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| egui_kittest::wgpu::create_render_state(wgpu_setup(), Default::default())));
    let Ok(rs) = rs else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    if rs.adapter.get_info().backend == wgpu::Backend::Gl {
        eprintln!("skipping: GL builds its pipelines a frame at a time instead");
        return;
    }
    build_pipelines_ahead(true);
    let t0 = now_ms();
    let gpu = GpuCanvas::new(&rs);
    build_pipelines_ahead(false);
    let made = now_ms() - t0;
    let doc = page();
    // Until the worker hands the compositor over the canvas says it can't draw (the CPU
    // compositor does) and nothing waits for it.
    let mut handed = None;
    while handed.is_none() {
        let t = now_ms();
        let building = gpu.warm_up();
        assert!(now_ms() - t < 5.0, "a poll of the worker took {:.1} ms", now_ms() - t);
        if building {
            assert!(!gpu.supports(&doc), "the canvas drew with a compositor still being made");
            std::thread::sleep(std::time::Duration::from_millis(2));
        } else {
            handed = Some(now_ms() - t0);
        }
        assert!(now_ms() - t0 < 300_000.0, "the compositor never arrived");
    }
    assert!(gpu.supports(&doc));
    // The worker goes on with every other pipeline.
    let all = photocraft_gpu::plan::Kernel::DRAWN.iter().map(|k| if k.is_map() { 2 } else { 1 }).sum::<usize>();
    while gpu.pipelines_built().unwrap_or(0) < all && now_ms() - t0 < 300_000.0 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let built = gpu.pipelines_built().unwrap_or(0);
    eprintln!("canvas made in {made:.1} ms; compositor handed over after {:.0} ms; {built} of {all} pipelines after {:.0} ms", handed.unwrap_or(0.0), now_ms() - t0);
    assert_eq!(built, all, "pipelines left for the frames to build");
    crate::perf_span::begin_frame();
    gpu.composite(&doc, doc.bounds(), false).expect("composite");
    let spans = crate::perf_span::frame();
    let build_ms: f64 = spans.iter().filter(|(n, _)| n.starts_with("gpu.pipelines")).map(|(_, ms)| ms).sum();
    assert_eq!(gpu.pipelines_built(), Some(all), "the first composite built a pipeline");
    assert!(build_ms < 1.0, "the first composite spent {build_ms:.1} ms on pipelines: {spans:?}");
}
