//! The frames of Free Transform and of a reorder on the GPU canvas (the owner's PC, 0.13.43 to
//! 0.13.45): a reorder and the box's commit composited the whole view again (21 to 43 ms of
//! `gpu.submit` on a 200-layer page; `step_damage` of a reorder, `transform_tool::damage`).

use serde_json::json;

use crate::PhotocraftApp;
use crate::cpu_edit_perf_tests::gpu_settled;
use crate::move_repeat_tests::{Device, Harness, harness};

fn settle(h: &mut Harness) {
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_secs(60) && !settled(h.state()) {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    h.run_steps(3);
}

fn settled(app: &PhotocraftApp) -> bool {
    if app.gpu.is_some() { gpu_settled(app) } else { crate::cpu_canvas::settled(app) }
}

/// The pixels the GPU canvas composites from `act` until it is settled again.
fn composited(h: &mut Harness, act: impl FnOnce(&mut Harness)) -> u64 {
    let px = h.state().perf.refreshed_px;
    act(h);
    settle(h);
    h.state().perf.refreshed_px - px
}

/// The whole view composited once, as an edit with no bounds has it (the canvas's pixels at fit).
fn whole_view(h: &mut Harness) -> u64 {
    let id = h.state().session.active().expect("doc").doc.id.0;
    composited(h, |h| {
        h.state().gpu.as_ref().expect("gpu").evict(id);
        h.step();
    })
}

/// Free Transform on the GPU canvas: opening the box (the layer hidden under it) and committing a
/// scale (the document again, the layer transformed) composite where the layer was and is, not
/// the whole view.
#[test]
fn opening_and_committing_the_box_composite_only_where_its_layer_draws() {
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| {
            let _gpu = photocraft_testkit::gpu_lock();
            let Some(mut h) = harness(Device::Gpu) else { return };
            h.state_mut().ui.extras.rulers = false;
            let app = h.state_mut();
            app.run("file.new", json!({"width": 2000, "height": 3000, "background": "white"})).expect("a page");
            crate::move_repeat_tests::stripes(app, "art", 2000, 3000, 0);
            let shape = crate::move_repeat_tests::layer_id(app.run("shape.create", json!({"rect": [300, 400, 700, 650], "fill": "#8090b0"})).expect("shape"));
            app.run("layer.layerStyle.dropShadow", json!({"layer": shape.0})).expect("shadow");
            // Over the shape: the box draws it apart, over the layer it moves.
            crate::move_repeat_tests::styled_text(app, 380, 720, "CAPITULO 1", 90);
            app.run("layer.select", json!({"layer": shape.0})).expect("select");
            app.sync_views();
            let ctx = h.ctx.clone();
            let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"fit": true}));
            crate::control::handle(h.state_mut(), &ctx, &req);
            if !crate::move_repeat_tests::gpu_drawing(&mut h, Device::Gpu) {
                return;
            }
            settle(&mut h);
            let whole = whole_view(&mut h);
            let opened = composited(&mut h, |h| {
                crate::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})).expect("the box");
            });
            let committed = composited(&mut h, |h| {
                let app = h.state_mut();
                let t = app.ui.transform.as_mut().expect("the box");
                let [x0, y0, x1, y1] = t.rect;
                t.quad = [[x0, y0], [x1 + 60.0, y0], [x1 + 60.0, y1 + 40.0], [x0, y1 + 40.0]];
                crate::transform_tool::commit(app);
            });
            eprintln!("GPU canvas at fit, 2000x3000: whole view {whole} px; opening the box composited {opened} px, its commit {committed} px");
            assert!(h.state().ui.transform.is_none(), "committed");
            assert!(opened * 4 < whole, "opening the box composited {opened} px of a {whole} px view");
            assert!(committed * 4 < whole, "the commit composited {committed} px of a {whole} px view");
        })
        .expect("a thread")
        .join()
        .expect("the run");
}

/// A layer brought forward and sent back on the GPU canvas composites where it overlaps the layer
/// it passes, not the whole view.
#[test]
fn a_reorder_composites_only_where_the_layers_overlap_on_the_gpu_canvas() {
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| {
            let _gpu = photocraft_testkit::gpu_lock();
            let Some(mut h) = harness(Device::Gpu) else { return };
            h.state_mut().ui.extras.rulers = false;
            let app = h.state_mut();
            app.run("file.new", json!({"width": 2000, "height": 3000, "background": "white"})).expect("a page");
            crate::move_repeat_tests::stripes(app, "art", 2000, 3000, 0);
            let a = crate::move_repeat_tests::layer_id(app.run("shape.create", json!({"rect": [300, 400, 900, 900], "fill": "#8090b0"})).expect("shape"));
            app.run("layer.layerStyle.dropShadow", json!({"layer": a.0})).expect("shadow");
            app.run("shape.create", json!({"rect": [700, 700, 1300, 1200], "fill": "#b08090"})).expect("shape");
            crate::move_repeat_tests::styled_text(app, 900, 2400, "CAPITULO 1", 90);
            app.sync_views();
            let ctx = h.ctx.clone();
            let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"fit": true}));
            crate::control::handle(h.state_mut(), &ctx, &req);
            if !crate::move_repeat_tests::gpu_drawing(&mut h, Device::Gpu) {
                return;
            }
            settle(&mut h);
            let whole = whole_view(&mut h);
            for cmd in ["layer.arrange.bringForward", "layer.arrange.sendBackward"] {
                let px = composited(&mut h, |h| {
                    h.state_mut().run(cmd, json!({"layer": a.0})).unwrap_or_else(|e| panic!("{cmd}: {e}"));
                });
                eprintln!("GPU canvas at fit, 2000x3000: {cmd} composited {px} px of a {whole} px view");
                assert!(px * 4 < whole, "{cmd} composited {px} px of a {whole} px view");
            }
        })
        .expect("a thread")
        .join()
        .expect("the run");
}
