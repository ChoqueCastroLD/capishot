//! The frames of Free Transform and of a reorder on the GPU canvas (the owner's PC, 0.13.41 to
//! 0.13.45): opening the box on a Smart Object held frames of 22 to 41 ms in a different panel
//! each time (`worker_pool`: the box's images filled the pool and the window's thread waited for a
//! core), and a reorder and the box's commit composited the whole view again (21 to 43 ms of
//! `gpu.submit` on a 200-layer page; `step_damage` of a reorder, `transform_tool::damage`).

use serde_json::json;

use crate::PhotocraftApp;
use crate::cpu_edit_perf_tests::gpu_settled;
use crate::move_repeat_tests::{Device, Harness, harness};

/// The panels' share of the frame record: every UI span's own time (the canvas, commands and
/// shortcuts apart).
fn ui_ms() -> f64 {
    crate::perf_span::frame().iter().filter(|(n, _)| ["ui.", "dock.", "layers."].iter().any(|p| n.starts_with(p))).map(|e| e.1).sum()
}

/// How long this thread has waited for a core while ready to run, so far (ms; Linux's
/// `schedstat`, 0 elsewhere).
fn no_core_ms() -> f64 {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
    stat.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok()).map_or(0.0, |ns| ns / 1e6)
}

/// How many of the process's threads are ready to run (Linux; 0 elsewhere).
fn ready_threads() -> usize {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return 0 };
    let ready = |s: &str| s.rsplit(')').next().is_some_and(|rest| rest.trim_start().starts_with('R'));
    tasks.filter_map(Result::ok).filter(|t| std::fs::read_to_string(t.path().join("stat")).is_ok_and(|s| ready(&s))).count()
}

/// His credits page on `device`, its Smart Object (larger than the page, a Gaussian Blur smart
/// filter, a Drop Shadow) selected, the canvas settled.
fn credits_on(device: Device) -> Option<Harness> {
    let mut h = harness(device)?;
    h.state_mut().ui.extras.rulers = false;
    let smart = crate::move_repeat_tests::credits(h.state_mut()).smart;
    let app = h.state_mut();
    app.run("layer.layerStyle.dropShadow", json!({"layer": smart.0})).expect("drop shadow");
    app.run("layer.select", json!({"layer": smart.0})).expect("select");
    app.sync_views();
    if device != Device::Cpu && !crate::move_repeat_tests::gpu_drawing(&mut h, device) {
        return None;
    }
    settle(&mut h);
    Some(h)
}

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

/// The frame Ctrl+T opens the box on the styled Smart Object: the panels lay out in a few ms (their
/// spans in the frame record). The frames after, until its images are made off the frame, are
/// reported with the window thread's wait for a core and how many threads were ready to run.
#[test]
fn opening_the_box_on_a_styled_smart_object_lays_out_the_panels_in_a_few_ms() {
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| {
            let _gpu = photocraft_testkit::gpu_lock();
            let Some(mut h) = credits_on(Device::Gpu).or_else(|| credits_on(Device::Cpu)) else { return };
            let device = if h.state().gpu.is_some() { "GPU" } else { "CPU" };
            crate::transform_tool::set_off_frame(true);
            let mut opens = Vec::new();
            for round in 0..3 {
                let ctx = h.ctx.clone();
                // Per frame: the panels' time, the window thread's wait for a core, and how many of
                // the process's threads were ready to run as it ended (cores: `nproc`).
                let mut frames: Vec<(f64, f64, usize)> = Vec::new();
                // Until the images are made, and the two frames after (the first that draws them).
                let mut after = 0;
                while after < 3 {
                    crate::perf_span::begin_frame();
                    if frames.is_empty() {
                        crate::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})).expect("the box");
                    }
                    let waited = no_core_ms();
                    h.step();
                    frames.push((ui_ms(), no_core_ms() - waited, ready_threads()));
                    if frames.len() == 1 {
                        let spans: Vec<String> = crate::perf_span::frame().iter().take(5).map(|(n, ms)| format!("{n} {ms:.1}")).collect();
                        eprintln!("  the open frame's spans: {}", spans.join(", "));
                    }
                    after += usize::from(h.state().transform_preview.as_ref().is_some_and(|pv| pv.made()));
                    assert!(frames.len() < 2000, "the box's images were never made");
                    std::thread::sleep(std::time::Duration::from_millis(8));
                }
                let worst = frames.iter().map(|f| f.0).fold(0.0, f64::max);
                let waited: f64 = frames.iter().map(|f| f.1).sum();
                let ready = frames.iter().map(|f| f.2).max().unwrap_or(0);
                eprintln!(
                    "{device} round {round}: {} frames, the open frame's panels {:.1} ms, the slowest frame's {worst:.1} ms; waited for a core {waited:.1} ms; at most {ready} threads ready ({} cores, a pool of {})",
                    frames.len(),
                    frames[0].0,
                    std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
                    rayon::current_num_threads()
                );
                opens.push(frames[0].0);
                crate::transform_tool::cancel(h.state_mut());
                settle(&mut h);
            }
            crate::transform_tool::set_off_frame(false);
            opens.sort_by(f64::total_cmp);
            // (The median of the rounds: a shared machine takes a core away now and then.)
            assert!(opens[1] < 8.0, "the panels took {:.1} ms in the frame that opened the box", opens[1]);
        })
        .expect("a thread")
        .join()
        .expect("the run");
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
