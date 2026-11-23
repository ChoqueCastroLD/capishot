//! A crop, its undo and its redo on the GPU canvas: the page's size changes, so the canvas needs a
//! new display set. The frame that landed one made it whole: every layer uploaded at its new
//! place, every effect map built and the whole view composited (0.3 to 0.8 s on his PSD on
//! lavapipe). The page before now stays on screen, where the view drew it, while the new one fills
//! in pieces within the frames' work; the new one then replaces it in one frame. Drives the real
//! app offscreen and compares every presented frame: the old page exactly as it was, or the new
//! one as the CPU composites it, never a mix and never a blank canvas. Skips without a GPU adapter.

use serde_json::json;

use crate::PhotocraftApp;
use crate::move_repeat_tests::{screen, stripes, styled_text};

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

const W: u32 = 3000;
const H: u32 = 2000;

fn harness() -> Option<Harness> {
    std::panic::catch_unwind(|| {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(2300.0, 1400.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(64)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.set_wgpu(rs.clone());
                }
                app
            })
    })
    .ok()
}

struct Shot {
    w: u32,
    px: Vec<u8>,
}

impl Shot {
    fn at(&self, p: egui::Pos2) -> Option<&[u8]> {
        let (x, y) = (p.x.floor(), p.y.floor());
        if x < 0.0 || y < 0.0 || x as u32 >= self.w {
            return None;
        }
        self.px.get((y as usize * self.w as usize + x as usize) * 4..)?.get(..4)
    }
}

fn shot(h: &mut Harness) -> Shot {
    let img = h.render().expect("render");
    Shot { w: img.width(), px: img.into_raw() }
}

/// Document points sampled across the part of the page the view shows, with where they are on
/// screen now.
fn samples(h: &Harness) -> Vec<((i32, i32), egui::Pos2)> {
    let st = h.state().session.active().expect("doc");
    let (w, hh) = (st.doc.size.width as i32, st.doc.size.height as i32);
    let canvas = h.state().last_canvas_rect;
    let mut out = Vec::new();
    for y in (3..hh - 3).step_by(37) {
        for x in (3..w - 3).step_by(29) {
            let p = screen(h, x as f32 + 0.5, y as f32 + 0.5);
            if canvas.shrink(20.0).contains(p) {
                out.push(((x, y), p));
            }
        }
    }
    out
}

/// The share of `at` whose pixels in `img` differ from the CPU composite of the active document.
fn off_document(h: &Harness, img: &Shot, at: &[((i32, i32), egui::Pos2)]) -> f64 {
    let doc = h.state().session.active().expect("doc").doc.clone();
    let buf = photocraft_compose::render(&doc, doc.bounds());
    let mut want = vec![[0u8; 4]; buf.px.len()];
    photocraft_compose::quantize_rgba8(&mut want, &buf.px);
    let bad = at
        .iter()
        .filter(|((x, y), p)| {
            let w = want[*y as usize * doc.size.width as usize + *x as usize];
            img.at(*p).is_none_or(|got| (0..3).any(|k| got[k].abs_diff(w[k]) > 12))
        })
        .count();
    bad as f64 / at.len().max(1) as f64
}

/// The share of `at` (screen points of the page before) whose pixels in `img` differ from `was`.
fn off_before(img: &Shot, was: &Shot, at: &[((i32, i32), egui::Pos2)]) -> f64 {
    let bad = at.iter().filter(|(_, p)| img.at(*p) != was.at(*p)).count();
    bad as f64 / at.len().max(1) as f64
}

fn settle(h: &mut Harness) {
    let gpu = h.state().gpu.clone().expect("the GPU canvas");
    for _ in 0..2000 {
        h.step();
        let st = h.state().session.active().expect("doc");
        let current = h.state().canvases.get(&(st.doc.id, crate::canvas::GPU_OUTPUT)).is_some_and(|c| c.on_gpu && c.revision == st.revision);
        if current && !gpu.has_stale(st.doc.id.0) && !gpu.shows_before(st.doc.id.0) {
            break;
        }
    }
    h.run_steps(3);
    gpu.finish();
}

/// A crop, its undo and its redo, each composited a piece a frame (a GPU too slow for more): the
/// frame of the change and every one after composite at most one piece and build the effect maps
/// of at most one layer, and each frame shows
/// either the page before exactly as it was drawn, or the new page whole as the CPU composites
/// it. Without the page kept, the frame of the change composited the whole view (2.3 MP here).
#[test]
fn a_crop_and_its_undo_show_the_page_before_until_the_new_one_is_whole() {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some(mut h) = harness() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    if h.state().gpu.is_none() {
        eprintln!("skipping: no GPU canvas");
        return;
    }
    {
        let app = h.state_mut();
        app.run("file.new", json!({"width": W, "height": H, "background": "white"})).expect("doc");
        stripes(app, "stripes", W as i32, H as i32, 3);
        let id = app.session.active().expect("doc").active_layer.expect("stripes");
        app.run("layer.setProps", json!({"layer": id.0, "opacity": 0.7})).expect("opacity");
        // Rectangles with Drop Shadow, Gradient Overlay and Stroke across the crop's edges, as his
        // (the crop cuts their effect regions: their maps are built again).
        for (i, y) in [300, 700, 1100, 1500].into_iter().enumerate() {
            let x0 = 200 + 60 * i as i32;
            let l = crate::move_repeat_tests::layer_id(
                app.run("shape.create", json!({"rect": [x0, y, x0 + 2700, y + 260], "fill": "#8090b0"})).expect("a rectangle"),
            );
            app.run("layer.layerStyle.dropShadow", json!({"layer": l.0})).expect("drop shadow");
            app.run("layer.layerStyle.gradientOverlay", json!({"layer": l.0, "from": "#c0c8e0", "to": "#506080"})).expect("gradient overlay");
            app.run("layer.layerStyle.stroke", json!({"layer": l.0, "size": 4, "position": "outside", "color": "#ffffff"})).expect("stroke");
        }
        styled_text(app, 700, 600, "capitulo 1", 160);
        styled_text(app, 1500, 1300, "creditos", 120);
        app.ui.extras.rulers = false;
        app.background_jobs = false;
        app.sync_views();
    }
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"zoom": 1.0, "center": [W / 2, H / 2]}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    settle(&mut h);
    let gpu = h.state().gpu.clone().expect("gpu");
    let key = h.state().session.active().expect("doc").doc.id.0;
    crate::gpu_canvas::set_frame_work_ms(Some(0.0));
    let piece = 1024u64 * 1024;
    for (cmd, params) in [("image.crop", json!({"x": 1001, "y": 603, "width": 900, "height": 700})), ("edit.undo", json!({})), ("edit.redo", json!({}))] {
        let was = shot(&mut h);
        let at_before = samples(&h);
        let off = off_document(&h, &was, &at_before);
        assert!(off < 0.01, "{cmd}: the settled page is the document ({:.2}% of {} samples off)", off * 100.0, at_before.len());
        h.state_mut().run(cmd, params).unwrap_or_else(|e| panic!("{cmd}: {e}"));
        let mut frames = 0;
        loop {
            let (px, fx) = (h.state().perf.refreshed_px, photocraft_gpu::fx_updates());
            h.step();
            frames += 1;
            let drawn = h.state().perf.refreshed_px - px;
            assert!(drawn <= piece, "{cmd}: frame {frames} composited {drawn} px, over one piece ({piece})");
            let built = photocraft_gpu::fx_updates() - fx;
            assert!(built <= 1, "{cmd}: frame {frames} built the effect maps of {built} layers, over one");
            let img = shot(&mut h);
            if gpu.shows_before(key) {
                let off = off_before(&img, &was, &at_before);
                assert!(off == 0.0, "{cmd}: frame {frames} drew the page before differently from how it was drawn ({:.2}% of samples)", off * 100.0);
            } else {
                let off = off_document(&h, &img, &samples(&h));
                assert!(off < 0.01, "{cmd}: frame {frames} isn't the new page whole ({:.2}% of samples off)", off * 100.0);
                break;
            }
            assert!(frames < 200, "{cmd}: the new page never showed");
        }
        assert!(frames > 1, "{cmd}: the new page came in pieces, over frames");
        crate::gpu_canvas::set_frame_work_ms(None);
        settle(&mut h);
        crate::gpu_canvas::set_frame_work_ms(Some(0.0));
    }
    crate::gpu_canvas::set_frame_work_ms(None);
}
