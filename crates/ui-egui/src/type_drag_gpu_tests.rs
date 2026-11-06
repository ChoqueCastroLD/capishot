//! Scrubbing a type layer's size draws on the GPU canvas while the pointer is down, not only on
//! release. Renders the real app offscreen; skips without a GPU adapter.

use crate::PhotocraftApp;
use serde_json::json;

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

fn harness(w: u32, h: u32) -> Option<Harness> {
    std::panic::catch_unwind(move || {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(64)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.set_wgpu(rs.clone());
                }
                app.run("file.new", json!({"width": w, "height": h, "background": "white"})).unwrap();
                app.run("type.create", json!({"text": "HOLA", "size": 120, "x": w / 8, "y": h / 3})).unwrap();
                app.ui.tool = crate::state::Tool::Type;
                app.sync_views();
                app
            })
    })
    .ok()
}

/// Dark pixels drawn on the canvas.
fn ink(h: &mut Harness) -> usize {
    let c = h.state().last_canvas_rect;
    let img = h.render().expect("render");
    let (x0, y0, x1, y1) = (c.left() as u32 + 4, c.top() as u32 + 4, c.right() as u32 - 30, c.bottom() as u32 - 30);
    (y0..y1).map(|y| (x0..x1).filter(|x| img.get_pixel(*x, y).0[1] < 90).count()).sum()
}

fn scrub_shows_while_held(w: u32, height: u32, editing: bool) {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some(mut h) = harness(w, height) else { return };
    h.run_steps(12);
    if !h.state().perf.gpu {
        eprintln!("skipping: the canvas fell back to the CPU here");
        return;
    }
    if editing {
        crate::type_tool::edit_active(h.state_mut());
        h.run_steps(6);
        assert!(h.state().ui.text_edit.is_some(), "the text is open");
    }
    let at = egui::pos2(467.0, 49.0);
    let before = ink(&mut h);
    h.hover_at(at);
    h.run_steps(3);
    h.drag_at(at);
    h.run_steps(2);
    for i in 1..=12 {
        h.hover_at(at + egui::vec2(i as f32 * 6.0, 0.0));
        h.run_steps(2);
    }
    let size = |h: &Harness| match &h.state().session.active().unwrap().doc.walk().last().map(|(_, _, l)| l.content.clone()) {
        Some(photocraft_doc::LayerContent::Text(t)) => t.size_pt,
        _ => 0.0,
    };
    let held = ink(&mut h);
    eprintln!("{w}x{height} editing={editing}: before {before}, held {held}, size {}", size(&h));
    h.drop_at(at + egui::vec2(72.0, 0.0));
    h.run_steps(4);
    let released = ink(&mut h);
    eprintln!("{w}x{height} editing={editing}: released {released}, size {}", size(&h));
    assert_ne!(released, before, "the scrub changed the text");
    assert_ne!(held, before, "and shows while the pointer is held");
}

#[test]
fn a_size_scrub_shows_on_the_gpu_canvas_while_held_at_full_size() {
    scrub_shows_while_held(800, 600, false);
}

#[test]
fn a_size_scrub_shows_on_the_gpu_canvas_while_held_zoomed_out() {
    scrub_shows_while_held(1600, 2200, false);
}

#[test]
fn a_size_scrub_shows_on_the_gpu_canvas_while_held_editing_the_text() {
    scrub_shows_while_held(800, 600, true);
    scrub_shows_while_held(1600, 2200, true);
}
