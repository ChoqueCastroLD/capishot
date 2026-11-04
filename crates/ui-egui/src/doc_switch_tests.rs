//! `editor.showDocument` on the GPU canvas: a prewarmed page swaps in the frame of the call and a
//! cold one is drawn offscreen first, so no rendered frame is blank or half switched. Renders the
//! real app offscreen; skips without a GPU adapter.

use crate::PhotocraftApp;
use crate::control::ControlRequest;
use serde_json::{Value, json};

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

fn page(app: &mut PhotocraftApp, color: &str, hidden: bool) -> u64 {
    let made = app.session.execute("file.new", json!({"width": 300, "height": 400, "background": "white", "hidden": hidden})).unwrap();
    let id = made["id"].as_u64().unwrap();
    let target = Some(photocraft_doc::DocId(id));
    app.session.execute_scoped("edit.fill", json!({"color": color}), target, photocraft_engine::HistoryMode::None).unwrap();
    id
}

fn harness(tx: &mut Option<std::sync::mpsc::Sender<ControlRequest>>) -> Option<(Harness, [u64; 3])> {
    let (send, rx) = std::sync::mpsc::channel();
    *tx = Some(send);
    let ids = std::sync::Arc::new(std::sync::Mutex::new([0; 3]));
    let out = ids.clone();
    let built = std::panic::catch_unwind(move || {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(64)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default()).with_control(rx);
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.set_wgpu(rs.clone());
                }
                let made = [page(&mut app, "#ff0000", false), page(&mut app, "#0000ff", true), page(&mut app, "#00ff00", true)];
                *out.lock().unwrap() = made;
                app.ui.view.single_document = true;
                app.sync_views();
                app
            })
    });
    let h = built.map_err(|_| eprintln!("skipping: no GPU adapter")).ok()?;
    let ids = *ids.lock().unwrap();
    Some((h, ids))
}

/// The rendered colour at the centre of the canvas area.
fn centre(h: &mut Harness) -> [u8; 3] {
    let r = h.state().last_canvas_rect;
    let img = h.render().expect("render");
    let p = img.get_pixel(r.center().x as u32, r.center().y as u32).0;
    [p[0], p[1], p[2]]
}

fn is(c: [u8; 3], want: [u8; 3]) -> bool {
    (0..3).all(|i| c[i].abs_diff(want[i]) <= 10)
}

fn request(tx: &std::sync::mpsc::Sender<ControlRequest>, method: &str, params: Value) -> std::sync::mpsc::Receiver<Value> {
    let (req, rx) = ControlRequest::new(method, params);
    tx.send(req).unwrap();
    rx
}

#[test]
fn shown_pages_swap_in_one_frame_without_a_blank_one() {
    let _gpu = photocraft_testkit::gpu_lock();
    let mut tx = None;
    let Some((mut h, [a, b, c])) = harness(&mut tx) else { return };
    let tx = tx.unwrap();
    h.run_steps(8);
    if !h.state().perf.gpu {
        eprintln!("skipping: the canvas fell back to the CPU here");
        return;
    }
    const RED: [u8; 3] = [255, 0, 0];
    const BLUE: [u8; 3] = [0, 0, 255];
    const GREEN: [u8; 3] = [0, 255, 0];
    assert!(is(centre(&mut h), RED), "page A shown");
    // Prewarm B while A shows: A stays on screen, B's texture is drawn.
    let warm = request(&tx, "document.prewarm", json!({"id": b, "viewport": "keep"}));
    let mut reply = None;
    for _ in 0..30 {
        h.step();
        assert!(is(centre(&mut h), RED), "page A stays while B is drawn ahead");
        if let Ok(r) = warm.try_recv() {
            reply = Some(r);
            break;
        }
    }
    let reply = reply.expect("the prewarm replied");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["result"]["gpuBytes"].as_u64().unwrap() > 0, "{reply}");
    // The switch: the very next frame shows B, and the reply says it waited for nothing.
    let shown = request(&tx, "editor.showDocument", json!({"id": b, "viewport": "keep"}));
    h.step();
    assert!(is(centre(&mut h), BLUE), "B in the frame of the call");
    let r = shown.try_recv().expect("replied in that frame");
    assert_eq!((r["result"]["frames"].clone(), r["result"]["shown"].clone()), (json!(0), json!(true)), "{r}");
    assert_eq!(h.state().session.active().unwrap().doc.id.0, b);
    // A cold page: every frame is B until C is drawn, then C.
    let cold = request(&tx, "editor.showDocument", json!({"id": c}));
    let mut seen_c = false;
    for _ in 0..30 {
        h.step();
        let px = centre(&mut h);
        assert!(is(px, if seen_c { GREEN } else { px }) && (is(px, BLUE) || is(px, GREEN)), "no blank or partial frame: {px:?}");
        seen_c |= is(px, GREEN);
        if seen_c && cold.try_recv().is_ok() {
            break;
        }
    }
    assert!(seen_c, "C shown");
    // A, hidden and warm since, swaps back at once.
    let back = request(&tx, "editor.showDocument", json!({"id": a}));
    h.step();
    assert!(is(centre(&mut h), RED));
    assert_eq!(back.try_recv().expect("replied")["result"]["frames"], 0);
    assert_eq!(h.state().session.visible_count(), 1, "one shown page");
    // The budget evicts the hidden pages' textures; they stay open.
    let budget = request(&tx, "editor.setMemoryBudget", json!({"mb": 0}));
    h.step();
    let evicted = budget.try_recv().unwrap();
    assert_eq!(evicted["result"]["evicted"].as_array().map(Vec::len), Some(2), "{evicted}");
    let gpu = h.state().gpu.clone().unwrap();
    assert!(gpu.texture_info(b).is_none() && gpu.texture_info(c).is_none() && gpu.texture_info(a).is_some());
    assert_eq!(h.state().session.documents().len(), 3);
}
