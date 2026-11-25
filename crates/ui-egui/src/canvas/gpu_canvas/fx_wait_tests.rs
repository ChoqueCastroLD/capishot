//! What the view shows waits for its effect maps and layer pages: a change the view shows is
//! composited once a composite of it builds no maps and uploads no pages, those built and
//! uploaded first within the frames' work, the old pixels shown meanwhile. The frame of an 8K
//! page's crop undo built a styled layer's maps whole (gpu.fx 416 ms on lavapipe), and a Fill
//! uploaded the whole layer in its frame (gpu.upload 49-54 ms on an RTX 3060 Ti).

use serde_json::json;

use crate::move_repeat_tests::{Device, gpu_drawing, harness};

/// A new effect on a layer as large as most of the page (a distance field over 4.6 MP to
/// compute on the CPU) is prepared over several frames before the view shows it.
#[test]
fn a_styled_layers_new_maps_are_built_over_frames_before_the_view_shows_them() {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some(mut h) = harness(Device::Gpu) else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let layer = {
        let app = h.state_mut();
        app.ui.extras.rulers = false;
        app.run("file.new", json!({"width": 3000, "height": 2400, "background": "white"})).expect("doc");
        crate::move_repeat_tests::stripes(app, "art", 2400, 1800, 3);
        let l = crate::move_repeat_tests::active(app);
        app.run("layer.layerStyle.dropShadow", json!({"layer": l.0})).expect("shadow");
        app.sync_views();
        l
    };
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"fit": true}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    if !gpu_drawing(&mut h, Device::Gpu) {
        return;
    }
    let settled = |h: &crate::move_repeat_tests::Harness| {
        let app = h.state();
        let st = app.session.active().expect("doc");
        let gpu = app.gpu.as_ref().expect("gpu");
        app.canvases.get(&(st.doc.id, crate::canvas::GPU_OUTPUT)).is_some_and(|c| c.on_gpu && c.revision == st.revision) && !gpu.has_stale(st.doc.id.0)
    };
    for _ in 0..3000 {
        if settled(&h) {
            break;
        }
        h.step();
    }
    assert!(settled(&h), "the page is drawn");
    if photocraft_gpu::fx_builds() == 0 {
        eprintln!("skipping: layer effects are drawn on the CPU here");
        return;
    }
    // Half a millisecond of work a frame: a part of the field at a time.
    super::set_frame_work_ms(Some(0.5));
    let u0 = photocraft_gpu::fx_updates();
    h.state_mut().run("layer.layerStyle.outerGlow", json!({"layer": layer.0, "size": 18})).expect("glow");
    let mut first = None;
    for frame in 1..=3000 {
        h.step();
        if first.is_none() && photocraft_gpu::fx_updates() != u0 {
            first = Some(frame);
        }
        if first.is_some() && settled(&h) {
            break;
        }
    }
    super::set_frame_work_ms(None);
    eprintln!("the glow's maps written in frame {first:?}");
    assert!(settled(&h), "the glow is drawn");
    assert!(first.is_some_and(|f| f >= 4), "the glow's maps were written in frame {first:?}: built whole in the frame that drew them");
}
