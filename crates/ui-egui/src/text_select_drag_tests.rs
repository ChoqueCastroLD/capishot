//! The Text Selection tool's rectangle, lasso and brush show on the real canvas while they're
//! dragged, not only once released. Renders the real app offscreen; skips without a GPU adapter.

use crate::PhotocraftApp;
use crate::state::{Tool, ZoneShape};
use serde_json::json;

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

/// Finds whatever is dark.
struct Dark;
impl photocraft_ai_textmask::TextMasker for Dark {
    fn model_state(&self) -> photocraft_ai_textmask::ModelState {
        photocraft_ai_textmask::ModelState::Ready
    }
    fn device(&self) -> String {
        "test".into()
    }
    fn detect(&self, page: photocraft_ai_textmask::Page<'_>, _: &dyn Fn() -> bool) -> Result<photocraft_ai_textmask::Detection, photocraft_ai_textmask::Error> {
        let prob: Vec<f32> = page.rgb.as_chunks::<3>().0.iter().map(|c| if c[0] < 128 { 0.995 } else { 0.0 }).collect();
        Ok(photocraft_ai_textmask::regions::find_regions(&prob, page.width, page.height))
    }
}

fn harness() -> Option<Harness> {
    std::panic::catch_unwind(move || {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 800.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(64)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.set_wgpu(rs.clone());
                }
                app.run("file.new", json!({"width": 600, "height": 400, "background": "white"})).unwrap();
                app.session.set_text_masker(Some(std::sync::Arc::new(Dark)));
                for (x, y) in [(40, 40), (400, 260)] {
                    app.session.execute("select.rect", json!({"x": x, "y": y, "width": 60, "height": 40})).unwrap();
                    app.session.execute("edit.fill", json!({"color": "#000000"})).unwrap();
                }
                app.session.execute("select.deselect", json!({})).unwrap();
                app.session.execute("select.detectText", json!({})).unwrap();
                app.ui.tool = Tool::TextSelection;
                app.sync_views();
                app
            })
    })
    .ok()
}

/// Pixels of the canvas that aren't grey (the page is white, the pasteboard grey).
fn tinted(h: &mut Harness) -> usize {
    let c = h.state().last_canvas_rect;
    let img = h.render().expect("render");
    let (x0, y0, x1, y1) = (c.left() as u32 + 4, c.top() as u32 + 4, c.right() as u32 - 30, c.bottom() as u32 - 30);
    (y0..y1)
        .map(|y| {
            (x0..x1)
                .filter(|x| {
                    let p = img.get_pixel(*x, y).0;
                    let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
                    mx - mn > 40
                })
                .count()
        })
        .sum()
}

#[test]
fn a_drag_shows_while_held() {
    let _gpu = photocraft_testkit::gpu_lock();
    for shape in [ZoneShape::Rect, ZoneShape::Lasso, ZoneShape::Brush] {
        let Some(mut h) = harness() else { return };
        h.run_steps(12);
        h.state_mut().ui.tool_options.text_select_shape = shape;
        h.state_mut().session.tools.brush.size = 30.0;
        let c = h.state().last_canvas_rect.center();
        let before = tinted(&mut h);
        let at = c + egui::vec2(-80.0, -60.0);
        h.hover_at(at);
        h.run_steps(2);
        h.drag_at(at);
        h.run_steps(2);
        for i in 1..=12 {
            h.hover_at(at + egui::vec2(i as f32 * 12.0, i as f32 * 8.0));
            h.run_steps(2);
        }
        let held = tinted(&mut h);
        assert!(h.state().session.text_detection().is_some(), "pieces found");
        eprintln!("{shape:?}: before {before}, held {held}, drag {:?}", h.state().drag.as_ref().map(|d| d.points.len()));
        h.drop_at(at + egui::vec2(150.0, 100.0));
        h.run_steps(2);
        assert!(held > before + 100, "{shape:?}: before {before}, held {held}");
    }
}
