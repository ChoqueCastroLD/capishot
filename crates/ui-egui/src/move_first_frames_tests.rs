//! Moving layers of naxu's credits PSD right after opening it (the owner's 0.13.33-0.13.34 report
//! on an RTX 3060 Ti: pressing a text layer with the Move tool froze the window, then the app
//! died): every text layer (Outer Glow, Gradient Overlay, Stroke), the shapes and the Smart
//! Objects, pressed and dragged from the first frames after the file opens, on the CPU canvas, on
//! the GPU canvas while its compositor is still being made (frames composited on the CPU into its
//! textures, as his System Info showed), and on the GPU canvas. No frame may take more than
//! [`MAX_FRAME_S`]; a frame that never ends aborts the run (a watchdog), and every drag moves its
//! layer. Skips without his file or a GPU adapter.

use egui::{Modifiers, PointerButton};
use photocraft_doc::{LayerContent, LayerId};
use serde_json::json;

use crate::move_repeat_tests::{Device, PSD, harness, screen};

/// The longest a frame may take (a debug build on a loaded server; a hang is what this catches).
const MAX_FRAME_S: f64 = 10.0;

/// A frame still running after this aborts the test process (a hang: nothing else would end it).
const HANG_S: u64 = 120;

/// How the drags start: from the first frames after the file opened, or zoomed to 75 % with the
/// pointer resting on each layer first (its drag cache made ahead).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Start {
    FirstFrames,
    Rested75,
}

fn drag_everything(device: Device, pending: bool) {
    drags(device, pending, Start::FirstFrames);
}

fn drags(device: Device, pending: bool, start: Start) {
    let _gpu = photocraft_testkit::gpu_lock();
    if !std::path::Path::new(PSD).exists() {
        eprintln!("skipping: {PSD} isn't here");
        return;
    }
    let Some(mut h) = harness(device) else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    if device != Device::Cpu && h.state().gpu.is_none() {
        eprintln!("skipping: no GPU canvas");
        return;
    }
    let _hold = pending.then(|| h.state().gpu.as_ref().map(crate::gpu_canvas::GpuCanvas::hold_compositor)).flatten();
    {
        let app = h.state_mut();
        app.background_jobs = false;
        app.open_path(PSD).expect("open his file");
        app.sync_views();
        app.ui.tool = crate::state::Tool::Move;
        app.ui.tool_options.move_auto_select = false;
        app.ui.extras.snap = false;
    }
    // The watchdog: the frame that started at `started` (0: none running) must end within HANG_S.
    let started = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let watch = started.clone();
    let t0 = std::time::Instant::now();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let s = watch.load(std::sync::atomic::Ordering::Relaxed);
            if s > 0 && t0.elapsed().as_secs().saturating_sub(s) > HANG_S {
                eprintln!("HANG: a frame has been running for over {HANG_S} s");
                std::process::abort();
            }
        }
    });
    let mut worst = 0.0f64;
    let mut frame = |h: &mut egui_kittest::Harness<'static, crate::PhotocraftApp>, what: &str| {
        started.store(t0.elapsed().as_secs().max(1), std::sync::atomic::Ordering::Relaxed);
        let t = std::time::Instant::now();
        h.step();
        started.store(0, std::sync::atomic::Ordering::Relaxed);
        let s = t.elapsed().as_secs_f64();
        worst = worst.max(s);
        if s > 1.0 {
            eprintln!("SLOW {device:?} pending {pending} {what}: {s:.2} s ({})", h.state().perf.last_refresh);
        }
        assert!(s <= MAX_FRAME_S, "{device:?} pending {pending} {what}: a frame took {s:.1} s");
    };
    // One frame after the file opened: the drags start at once (or zoomed to 75 %: the level large
    // edits take there is 1, the one a drag draws at 0).
    if start == Start::Rested75 {
        let ctx = h.ctx.clone();
        let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"zoom": 0.75}));
        crate::control::handle(h.state_mut(), &ctx, &req);
    }
    frame(&mut h, "open");
    let doc = h.state().session.active().expect("doc").doc.clone();
    let targets: Vec<(LayerId, String)> = doc
        .walk()
        .into_iter()
        .map(|(_, _, l)| l)
        .filter(|l| l.visible && matches!(l.content, LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_)))
        .map(|l| (l.id, l.name.clone()))
        .collect();
    assert!(targets.len() >= 5, "his text layers, shapes and smart objects: {}", targets.len());
    for (id, name) in targets {
        h.state_mut().run("layer.select", json!({"layer": id.0})).expect("select");
        if start == Start::Rested75 {
            // The pointer resting: the cache of the selected layer made ahead.
            for _ in 0..200 {
                let doc = h.state().session.active().expect("doc").doc.clone();
                if h.state().gpu.as_ref().is_none_or(|g| g.drag_ready_for(&doc, &[id])) {
                    break;
                }
                frame(&mut h, "rest");
            }
        }
        let now = h.state().session.active().expect("doc").doc.clone();
        let all = photocraft_geom::Rect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4);
        let before = now.layer(id).and_then(|l| photocraft_compose::change_bounds(l, all));
        // Where the view shows it (zoomed in, a layer may go past the window).
        let xf = crate::canvas::ViewXform::active(h.state()).expect("a view");
        let (lo, hi) = (xf.to_doc(xf.rect.min), xf.to_doc(xf.rect.max));
        let shown = photocraft_geom::Rect::new(lo[0].ceil() as i32 + 8, lo[1].ceil() as i32 + 8, hi[0].floor() as i32 - 8, hi[1].floor() as i32 - 8).intersect(&now.bounds());
        let b = before.unwrap_or(now.bounds()).intersect(&shown);
        let b = if b.is_empty() { now.bounds() } else { b };
        // A third of the way in: the middle of a layer's box holds the transform controls'
        // reference point.
        let start = [b.x0 as f32 + b.width() as f32 / 3.0 + 0.5, (b.y0 + b.y1) as f32 / 2.0 + 0.5];
        // Half a second apart (egui's double click, on a text layer, would edit its type).
        for _ in 0..30 {
            frame(&mut h, "pause");
        }
        let p0 = screen(&h, start[0], start[1]);
        h.event(egui::Event::PointerMoved(p0));
        h.event(egui::Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        frame(&mut h, &format!("{name} press"));
        for i in 1..=6 {
            h.event(egui::Event::PointerMoved(egui::pos2(p0.x + 9.0 * i as f32, p0.y + 4.0 * i as f32)));
            frame(&mut h, &format!("{name} move {i}"));
        }
        let pe = egui::pos2(p0.x + 54.0, p0.y + 24.0);
        h.event(egui::Event::PointerButton { pos: pe, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        frame(&mut h, &format!("{name} release"));
        let after = h.state().session.active().expect("doc").doc.layer(id).and_then(|l| photocraft_compose::change_bounds(l, all));
        assert_ne!(after, before, "{device:?} pending {pending}: dragging `{name}` moved it");
    }
    eprintln!("FIRST FRAMES {device:?} pending {pending}: longest frame {worst:.2} s");
}

#[test]
fn moving_his_layers_right_after_opening_never_hangs_on_the_cpu_canvas() {
    drag_everything(Device::Cpu, false);
}

#[test]
fn moving_his_layers_while_the_gpu_compositor_is_being_made_never_hangs() {
    drag_everything(Device::Gpu, true);
}

/// 0.13.33-0.13.34 on an RTX 3060 Ti: the cache made ahead at the level large edits take (1 at
/// 75 %) said it was ready, the drag drew at the view's level (0), so each frame the canvas redrew
/// the page and asked the cache again, without end.
#[test]
fn moving_his_layers_after_the_pointer_rested_at_75_percent_never_hangs() {
    drags(Device::Gpu, false, Start::Rested75);
}

#[test]
fn moving_his_layers_right_after_opening_never_hangs_on_the_gpu_canvas() {
    drag_everything(Device::Gpu, false);
}
