//! Level of detail and deferred regions on the GPU canvas (`GpuCanvas::refresh_for` /
//! `catch_up`): a zoomed-out view composites large edits at its mip level, edits away from the
//! view wait for it, and once the edits pause everything is brought to full size, ending exactly
//! where a plain full refresh ends. Skips without a GPU adapter or where the document isn't on
//! the wgpu compositor.

use eframe::egui_wgpu::RenderState;
use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerMask, Size};
use photocraft_geom::Rect;
use photocraft_ui_egui::gpu_canvas::{self, GpuCanvas, ViewNeed};

static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn canvas() -> Option<(GpuCanvas, RenderState)> {
    let rs =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| egui_kittest::wgpu::create_render_state(gpu_canvas::wgpu_setup(), Default::default()))).ok();
    let Some(rs) = rs else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    Some((GpuCanvas::new(&rs), rs))
}

/// A white page with a full-page Multiply tone, a masked panel and some ink.
fn page() -> Document {
    let mut d = Document::with_background("page", Size::new(1536, 1100), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    let mut tone = Layer::raster("tone", fmt);
    tone.surface_mut().unwrap().fill_rect(d.bounds(), &photocraft_raster::from_rgba(&fmt, [0.6, 0.7, 0.9, 1.0]));
    tone.blend = BlendMode::Multiply;
    tone.opacity = 0.4;
    let mut panel = Layer::raster("panel", fmt);
    let s = panel.surface_mut().unwrap();
    s.fill_rect(Rect::new(100, 80, 900, 700), &photocraft_raster::from_rgba(&fmt, [0.9, 0.3, 0.1, 1.0]));
    for i in 0..40 {
        s.fill_rect(Rect::new(50 + i * 31, 20 + i * 23, 60 + i * 31 + 200, 23 + i * 23), &photocraft_raster::from_rgba(&fmt, [0.05, 0.05, 0.05, 1.0]));
    }
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(300, 0, 420, 1100), &[0.2]);
    panel.mask = Some(m);
    let mut ink = Layer::raster("ink", fmt);
    ink.surface_mut().unwrap().fill_rect(Rect::new(700, 500, 1500, 1050), &photocraft_raster::from_rgba(&fmt, [0.1, 0.6, 0.2, 0.7]));
    ink.blend = BlendMode::Overlay;
    d.layers.extend([tone, panel, ink]);
    d
}

fn max_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (0..4).map(|k| (x[k] - y[k]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max)
}

/// Catch up with an idle view until nothing is out of date (waiting out the refine delay).
fn settle(g: &GpuCanvas, doc: &Document, need: ViewNeed) -> usize {
    let mut frames = 0;
    std::thread::sleep(std::time::Duration::from_millis(gpu_canvas::REFINE_DELAY_MS as u64 + 20));
    while g.has_stale(doc.id.0) {
        if g.catch_up(doc.id.0, doc, None, need, true).refined {
            frames += 1;
        } else {
            // The last piece is still on the GPU.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(frames < 500, "catch-up doesn't finish");
    }
    frames
}

/// `doc` as another document (its own texture and compositor state on the same device).
fn copy(doc: &Document) -> Document {
    let mut d = doc.clone();
    d.id = photocraft_doc::DocId::fresh();
    d
}

#[test]
fn reduced_and_deferred_refreshes_end_like_a_full_refresh() {
    let _gpu = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((g, _rs)) = canvas() else { return };
    let base = page();
    if !g.supports(&base) {
        eprintln!("skipping: not on the wgpu compositor");
        return;
    }
    let full = g.refresh(base.id.0, &base, None, None);
    assert!(full.kind.starts_with("gpu"), "{}", full.kind);
    assert!(!g.has_stale(base.id.0));
    let (_, _, want) = g.read_texels(base.id.0).unwrap();
    let w = 1536usize;

    // A new texture for a view zoomed in on a corner: only that corner (and a margin) now.
    let mut doc = copy(&base);
    let key = doc.id.0;
    let corner = ViewNeed { visible: Rect::new(0, 0, 300, 200), level: 0 };
    let r = g.refresh_for(key, &doc, None, None, Some(corner));
    assert_eq!(r.level, 0);
    assert!(r.px < 1536 * 1100 / 2, "{} px composited for a corner", r.px);
    assert!(g.has_stale(key));
    let (_, _, part) = g.read_texels(key).unwrap();
    for y in 0..200 {
        assert!(max_diff(&part[y * w..y * w + 300], &want[y * w..y * w + 300]) == 0.0, "row {y} of the view");
    }
    let frames = settle(&g, &doc, corner);
    assert!(frames > 0);
    assert_eq!(max_diff(&g.read_texels(key).unwrap().2, &want), 0.0, "settled deferred refresh");

    // Zoomed out to a quarter: an opacity change composites at level 2, then refines.
    let fit = ViewNeed { visible: doc.bounds(), level: 2 };
    doc.layers[2].opacity = 0.5;
    let r = g.refresh_for(key, &doc, Some(doc.bounds()), None, Some(fit));
    assert_eq!(r.level, 2, "{r:?}");
    assert!(r.px <= (1536 / 4 + 1) * (1100 / 4 + 1), "{} px at level 2", r.px);
    assert!(g.has_stale(key));
    // The reference: the same edit refreshed at full size.
    let exact_doc = copy(&doc);
    g.refresh(exact_doc.id.0, &exact_doc, None, None);
    let (_, size, reference) = g.read_texels_level(exact_doc.id.0, 2).unwrap();
    let (_, size2, reduced) = g.read_texels_level(key, 2).unwrap();
    assert_eq!(size, size2);
    // The coarse level the view samples is close to the full-size result filtered down.
    let mean = reference.iter().zip(&reduced).map(|(a, b)| (0..3).map(|k| (a[k] - b[k]).abs()).sum::<f32>() / 3.0).sum::<f32>() / reference.len() as f32;
    assert!(mean < 0.02, "level 2 mean difference {mean}");
    let (_, _, exact) = g.read_texels(exact_doc.id.0).unwrap();
    // A view zoomed back in needs full size where it looks: that much is composited at once.
    let zoomed = ViewNeed { visible: Rect::new(600, 400, 900, 600), level: 0 };
    let c = g.catch_up(key, &doc, None, zoomed, false);
    assert!(c.px > 0 && !c.refined);
    let (_, _, now) = g.read_texels(key).unwrap();
    for y in 400..600 {
        assert_eq!(max_diff(&now[y * w + 600..y * w + 900], &exact[y * w + 600..y * w + 900]), 0.0, "row {y} after zooming in");
    }
    settle(&g, &doc, fit);
    assert_eq!(max_diff(&g.read_texels(key).unwrap().2, &exact), 0.0, "settled reduced refresh");
    assert_eq!(max_diff(&g.read_texels_level(key, 2).unwrap().2, &reference), 0.0, "mips rebuilt");

    // Small edits stay full size even zoomed out (a brush dab).
    let fmt = doc.pixel_format();
    doc.layers[3].surface_mut().unwrap().fill_rect(Rect::new(10, 10, 70, 70), &photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 0.0, 1.0]));
    let r = g.refresh_for(key, &doc, Some(Rect::new(10, 10, 70, 70)), None, Some(fit));
    assert_eq!(r.level, 0);
    assert!(!g.has_stale(key));
}

#[test]
fn view_levels_follow_the_zoom() {
    assert_eq!(gpu_canvas::lod_level(1.0), 0);
    assert_eq!(gpu_canvas::lod_level(2.5), 0);
    assert_eq!(gpu_canvas::lod_level(0.999), 0);
    assert_eq!(gpu_canvas::lod_level(0.9), 0);
    assert_eq!(gpu_canvas::lod_level(0.5), 1);
    assert_eq!(gpu_canvas::lod_level(0.3), 1);
    assert_eq!(gpu_canvas::lod_level(0.25), 2);
    assert_eq!(gpu_canvas::lod_level(0.13), 2);
    assert_eq!(gpu_canvas::lod_level(0.001), photocraft_gpu::MAX_LOD);
    assert_eq!(gpu_canvas::lod_level(f32::NAN), 0);
    assert_eq!(gpu_canvas::lod_level(0.0), 0);
}
