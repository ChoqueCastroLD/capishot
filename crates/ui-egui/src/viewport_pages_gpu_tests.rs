//! Viewport pages on the real GPU canvas (#5332): a document over the texture budget stays on the
//! GPU, within the budget, with the pixels of the texture set; a pan draws the backdrop instead
//! of a hole; edits and closing behave. Renders the real app offscreen; skips without an adapter.

use crate::PhotocraftApp;
use crate::canvas::viewport_pages::stream_key;
use crate::gpu_canvas::{DEFAULT_TILE, canvas_texture_bytes};
use serde_json::json;

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

fn harness() -> Option<Harness> {
    let built = std::panic::catch_unwind(|| {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(64)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.set_wgpu(rs.clone());
                }
                app.ui.view.single_document = true;
                app
            })
    });
    built.map_err(|_| eprintln!("skipping: no GPU adapter")).ok()
}

/// Frames until the active document's pages are all rendered and uploaded.
fn settle(h: &mut Harness) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        h.step();
        let app = h.state();
        let id = app.session.active().expect("document").doc.id;
        if app.streams.get(&id).is_some_and(|s| !s.busy() && s.settled()) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "the viewport pages didn't finish");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn centre(h: &mut Harness) -> [u8; 3] {
    let r = h.state().last_canvas_rect;
    let img = h.render().expect("render");
    let p = img.get_pixel(r.center().x as u32, r.center().y as u32).0;
    [p[0], p[1], p[2]]
}

fn close(a: [u8; 3], b: [u8; 3], by: u8) -> bool {
    (0..3).all(|i| a[i].abs_diff(b[i]) <= by)
}

#[test]
fn a_canvas_over_the_texture_budget_draws_its_viewport_pages_on_the_gpu() {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some(mut h) = harness() else { return };
    let run = |h: &mut Harness, cmd: &str, p: serde_json::Value| h.state_mut().session.execute(cmd, p).expect(cmd);
    run(&mut h, "file.new", json!({"width": 3000, "height": 2000, "background": "#336699"}));
    h.state_mut().sync_views();
    h.run_steps(8);
    if !h.state().perf.gpu {
        eprintln!("skipping: the canvas fell back to the CPU here");
        return;
    }
    let whole = centre(&mut h);
    let gpu = h.state().gpu.clone().expect("gpu canvas");
    let id = h.state().session.active().expect("document").doc.id;
    let key = stream_key(id.0);
    // Room for pages (half the budget), not for the document's texture set.
    let set = canvas_texture_bytes([3000, 2000], DEFAULT_TILE, 4);
    let budget = set - 1;
    gpu.set_canvas_budget(budget);
    settle(&mut h);
    assert!(h.state().perf.gpu, "still on the GPU canvas");
    assert!(gpu.texture_info(id.0).is_none(), "the texture set was freed");
    let (pages, bytes) = gpu.pages(key);
    let stats = h.state().streams[&id].stats.clone();
    assert_eq!(pages.len(), stats.resident);
    assert!(stats.wanted > 0 && stats.last_error.is_none(), "{stats:?}");
    assert!(bytes <= budget / 2, "pages stay within half the budget: {bytes}");
    assert_eq!(gpu.texture_info(key).map(|(_, b)| b), Some(bytes), "and that is what the canvas counts for them");
    let shown = centre(&mut h);
    assert!(close(shown, whole, 2), "pages {shown:?}, texture set {whole:?}");
    // An edit re-renders the pages it reaches.
    let uploads = stats.uploads;
    run(&mut h, "edit.fill", json!({"color": "#cc8844"}));
    settle(&mut h);
    let stats = h.state().streams[&id].stats.clone();
    assert!(stats.dirtied > 0 && stats.uploads + stats.partial > uploads, "{stats:?}");
    assert!(close(centre(&mut h), [0xcc, 0x88, 0x44], 3), "the fill is shown");
    // A small edit re-renders only the part of each page it reaches, into the page held.
    let (held, partial) = (gpu.pages(key).0.len(), stats.partial);
    run(&mut h, "select.rect", json!({"x": 1400, "y": 900, "width": 200, "height": 200}));
    settle(&mut h);
    run(&mut h, "edit.fill", json!({"color": "#22aa22"}));
    settle(&mut h);
    let stats = h.state().streams[&id].stats.clone();
    assert!(stats.partial > partial, "{stats:?}");
    assert_eq!(gpu.pages(key).0.len(), held, "no new page textures");
    assert!(close(centre(&mut h), [0x22, 0xaa, 0x22], 3), "the small fill is shown");
    run(&mut h, "select.deselect", json!({}));
    // A pan and zoom to pages never rendered: every frame draws the document, from the backdrop
    // until its own pages arrive.
    let view = h.state_mut().ui.views.get_mut(0).expect("view");
    view.zoom = 4.0;
    view.center = [2800.0, 200.0];
    view.fit_pending = false;
    for _ in 0..4 {
        h.step();
        assert!(close(centre(&mut h), [0xcc, 0x88, 0x44], 8), "no hole while the pages render");
    }
    settle(&mut h);
    assert!(close(centre(&mut h), [0xcc, 0x88, 0x44], 3));
    assert!(gpu.pages(key).1 <= budget / 2);
    // A budget too small for one page draws it on the CPU canvas, and frees the pages.
    gpu.set_canvas_budget(1);
    h.run_steps(3);
    assert!(gpu.pages(key).0.is_empty(), "pages freed");
    assert!(!h.state().streams.contains_key(&id), "stream dropped");
    assert!(close(centre(&mut h), [0xcc, 0x88, 0x44], 8), "the CPU canvas draws it");
    gpu.set_canvas_budget(budget);
    settle(&mut h);
    // Closing the document cancels its work and frees its pages.
    run(&mut h, "file.close", json!({"save": false}));
    h.run_steps(3);
    assert!(h.state().streams.is_empty());
    assert!(gpu.pages(key).0.is_empty());
}
