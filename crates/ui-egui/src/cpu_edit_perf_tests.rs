//! What an edit costs on the CPU canvas (rendering mode `cpu`, the owner's PC, 2026-11-22: a Move
//! release, hiding a layer, a reorder, a gradient, a Free Transform commit and big brushes each
//! froze a frame for 140 to 430 ms, because the edit redrew the page on the window's thread).
//! The real app runs offscreen on the CPU canvas with the page whole in view at level 0; every
//! action is played and each of its frames timed: the command and the frame that takes it, the
//! frames until the canvas shows the edit complete, then the frames in which the canvas makes
//! what it keeps for the next edit. Prints median, p99 and max per action, and how long after
//! the action the canvas showed it.

use egui::{Event, Modifiers, PointerButton};
use photocraft_doc::{LayerContent, LayerId};
use serde_json::json;

use crate::PhotocraftApp;
use crate::move_repeat_tests::{Device, PSD, harness, pct, screen};
use crate::state::Tool;

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

/// The synthetic 2000x3000 page of 200 layers the owner's PC was measured with.
const PAGE200: &str = "/root/capi-workers/pc/h/pages/page200.psd";

/// The frames of one kind of action.
#[derive(Default)]
struct Action {
    name: &'static str,
    ms: Vec<f64>,
    /// The slowest frame's spans.
    worst: (f64, String),
    /// From each action to the frame that shows it complete.
    shown_after: Vec<f64>,
}

struct Bench {
    h: Harness,
    actions: Vec<Action>,
}

impl Bench {
    /// `path` open on the CPU canvas at `zoom` around the page's middle, settled.
    fn open(path: &str, zoom: f64) -> Option<Bench> {
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} isn't here");
            return None;
        }
        let mut h = harness(Device::Cpu)?;
        // Edits composited on the worker pool, as in the app (tests composite in the frame).
        crate::cpu_canvas::set_flight_wait(Some(4.0));
        {
            let app = h.state_mut();
            app.background_jobs = false;
            app.open_path(path).expect("open");
            app.sync_views();
            app.ui.tool = Tool::Move;
            app.ui.tool_options.move_auto_select = false;
            app.ui.extras.snap = false;
            app.ui.view.show.smart_guides = false;
            app.ui.extras.rulers = false;
        }
        let size = h.state().session.active().expect("doc").doc.size;
        let ctx = h.ctx.clone();
        let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"zoom": zoom, "center": [size.width / 2, size.height / 2]}));
        crate::control::handle(h.state_mut(), &ctx, &req);
        let mut b = Bench { h, actions: Vec::new() };
        b.settle();
        Some(b)
    }

    fn app(&mut self) -> &mut PhotocraftApp {
        self.h.state_mut()
    }

    /// Frames until the canvas has nothing left to draw or make ahead.
    fn settle(&mut self) {
        for _ in 0..400 {
            self.h.step();
            if crate::cpu_canvas::settled(self.h.state()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        self.h.run_steps(3);
    }

    fn record(&mut self, name: &'static str, ms: f64) {
        let spans: Vec<String> = crate::perf_span::frame().iter().take(6).map(|(n, ms)| format!("{n} {ms:.1}")).collect();
        let a = match self.actions.iter_mut().position(|a| a.name == name) {
            Some(i) => &mut self.actions[i],
            None => {
                self.actions.push(Action { name, ..Default::default() });
                self.actions.last_mut().expect("pushed")
            }
        };
        if ms > a.worst.0 {
            a.worst = (ms, spans.join(", "));
        }
        a.ms.push(ms);
    }

    /// One frame of `name`: `act` (a command, an input event) and the frame that shows it.
    fn frame(&mut self, name: &'static str, act: impl FnOnce(&mut Harness)) {
        crate::perf_span::begin_frame();
        let t = std::time::Instant::now();
        act(&mut self.h);
        self.h.step();
        self.record(name, t.elapsed().as_secs_f64() * 1000.0);
    }

    /// `act`, then the frames until the canvas shows it complete and has nothing left to make
    /// (16 ms apart, as a display's are): all counted as `name`'s.
    fn action(&mut self, name: &'static str, act: impl FnOnce(&mut Harness)) {
        let t = std::time::Instant::now();
        self.frame(name, act);
        let mut shown = None;
        for _ in 0..120 {
            let made = self.h.state().transform_preview.as_ref().is_none_or(|pv| pv.made());
            if shown.is_none() && made && crate::cpu_canvas::shows_document(self.h.state()) {
                shown = Some(t.elapsed().as_secs_f64() * 1000.0);
            }
            if shown.is_some() && crate::cpu_canvas::settled(self.h.state()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(if shown.is_some() { 16 } else { 2 }));
            self.frame(name, |_| {});
        }
        if let Some(a) = self.actions.iter_mut().find(|a| a.name == name) {
            a.shown_after.push(shown.unwrap_or(f64::INFINITY));
        }
    }

    fn run(&mut self, name: &'static str, cmd: &'static str, params: serde_json::Value) {
        self.action(name, move |h| {
            h.state_mut().run(cmd, params).unwrap_or_else(|e| panic!("{cmd}: {e}"));
        });
    }

    /// A real drag with the tool in hand from document point `from` by `step` a frame, `n` frames.
    fn drag(&mut self, name: &'static str, from: [f32; 2], step: [f32; 2], n: usize) {
        let p0 = screen(&self.h, from[0], from[1]);
        self.h.event(Event::PointerMoved(p0));
        self.h.step();
        self.frame(name, |h| h.event(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }));
        let mut p = p0;
        for i in 1..=n {
            p = screen(&self.h, from[0] + step[0] * i as f32, from[1] + step[1] * i as f32);
            self.frame(name, |h| h.event(Event::PointerMoved(p)));
        }
        self.action(name, |h| h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }));
        self.settle();
    }

    fn report(&self, doc: &str) -> Vec<(&'static str, f64, f64)> {
        let mut out = Vec::new();
        for a in &self.actions {
            let (p50, p99, max) = (pct(&a.ms, 0.5), pct(&a.ms, 0.99), pct(&a.ms, 1.0));
            eprintln!(
                "EDIT cpu {doc} {:<28} n {:>3}  median {p50:>6.1}  p99 {p99:>6.1}  max {max:>6.1} ms  shown after median {:>6.1} max {:>6.1} ms   worst: {}",
                a.name,
                a.ms.len(),
                pct(&a.shown_after, 0.5),
                pct(&a.shown_after, 1.0),
                a.worst.1
            );
            out.push((a.name, p50, max));
        }
        out
    }
}

/// The layers the actions use: a type layer, a layer as large as the page (a Smart Object when
/// the page has one), a layer with effects.
struct Targets {
    text: LayerId,
    big: LayerId,
    styled: LayerId,
}

fn targets(app: &PhotocraftApp) -> Targets {
    let doc = app.session.active().expect("doc").doc.clone();
    let walk = doc.walk();
    let page = u64::from(doc.size.width) * u64::from(doc.size.height);
    let area = |l: &photocraft_doc::Layer| photocraft_compose::change_bounds(l, doc.bounds()).map_or(0, |r| u64::from(r.width()) * u64::from(r.height()));
    let find = |want: &dyn Fn(&photocraft_doc::Layer) -> bool| walk.iter().map(|(_, _, l)| *l).find(|l| l.visible && !doc.is_background(l.id) && want(l)).map(|l| l.id);
    let text = find(&|l| matches!(l.content, LayerContent::Text(_))).expect("a type layer");
    // A Smart Object as large as the page when there is one, else the largest layer.
    let big = find(&|l| matches!(l.content, LayerContent::Smart(_)) && area(l) * 2 > page)
        .or_else(|| walk.iter().map(|(_, _, l)| *l).filter(|l| l.visible && !doc.is_background(l.id) && l.children().is_none()).max_by_key(|l| area(l)).map(|l| l.id))
        .expect("a layer");
    let styled = find(&|l| matches!(l.content, LayerContent::Raster(_) | LayerContent::Shape(_)) && photocraft_compose::effects::has_effects(l)).expect("a layer with effects");
    Targets { text, big, styled }
}

/// Every measured action on `path`. `None` when the file isn't here.
fn edits(path: &str, zoom: f64) -> Option<Vec<(&'static str, f64, f64)>> {
    let _gpu = photocraft_testkit::gpu_lock();
    let mut b = Bench::open(path, zoom)?;
    let t = targets(b.app());
    let size = b.app().session.active().expect("doc").doc.size;
    let (w, h) = (size.width as f32, size.height as f32);
    let bounds = |b: &mut Bench, id: LayerId| {
        let doc = b.app().session.active().expect("doc").doc.clone();
        doc.layer(id).and_then(|l| photocraft_compose::change_bounds(l, doc.bounds())).expect("bounds")
    };
    for round in 0..2 {
        // Move: a type layer, a layer with effects, the layer as large as the page.
        for (name, id) in [("move type", t.text), ("move styled", t.styled), ("move page-size layer", t.big)] {
            b.app().run("layer.select", json!({"layer": id.0})).expect("select");
            b.settle();
            let r = bounds(&mut b, id);
            let from = [r.x0 as f32 + r.width() as f32 / 3.0 + 0.5, (r.y0 + r.y1) as f32 / 2.0 + 0.5];
            let dir = if round == 0 { 1.0 } else { -1.0 };
            b.drag(name, from, [13.0 * dir, 5.0 * dir], 10);
        }
        // Hide and show.
        for (name, id) in [("hide/show type", t.text), ("hide/show page-size layer", t.big)] {
            b.run(name, "layer.hideLayers", json!({"layer": id.0}));
            b.run(name, "layer.showLayers", json!({"layer": id.0}));
        }
        // Reorder.
        for (name, id) in [("reorder type", t.text), ("reorder page-size layer", t.big)] {
            b.app().run("layer.select", json!({"layer": id.0})).expect("select");
            b.settle();
            b.run(name, "layer.arrange.bringForward", json!({}));
            b.run(name, "layer.arrange.sendBackward", json!({}));
        }
        // Free Transform: the box opens (Ctrl+T) and closes (Esc). Its images are made on another
        // thread, as in the app; `CPU_EDIT_PERF_BOX_IN_FRAME` makes them in the frame, as before.
        crate::transform_tool::set_off_frame(std::env::var_os("CPU_EDIT_PERF_BOX_IN_FRAME").is_none());
        for (name, id) in [("transform open type", t.text), ("transform open page-size", t.big)] {
            b.app().run("layer.select", json!({"layer": id.0})).expect("select");
            b.settle();
            b.action(name, |h| {
                let ctx = h.ctx.clone();
                crate::transform_tool::begin(h.state_mut(), &ctx).expect("the box");
            });
            b.action("transform cancel", |h| crate::transform_tool::cancel(h.state_mut()));
        }
        crate::transform_tool::set_off_frame(false);
        // Free Transform commit.
        for (name, id) in [("transform commit type", t.text), ("transform commit page-size", t.big)] {
            b.app().run("layer.select", json!({"layer": id.0})).expect("select");
            b.settle();
            b.run(name, "edit.transform", json!({"scaleX": 104, "scaleY": 104}));
            b.run(name, "edit.undo", json!({}));
        }
        // Typing (a character put into the layer's text, as a keystroke commits it).
        b.app().run("layer.select", json!({"layer": t.text.0})).expect("select");
        b.settle();
        for _ in 0..6 {
            b.run("typing", "type.insertText", json!({"layer": t.text.0, "text": "a"}));
        }
        // A new layer on top for the brushes; the Gradient tool then makes its fill layer.
        b.app().run("layer.new.layer", json!({"name": "paint"})).expect("layer");
        b.settle();
        b.app().ui.tool = Tool::Brush;
        b.app().session.tools.brush.hardness = 0.0;
        for (name, px) in [("brush 30 px", 30.0), ("brush 300 px soft", 300.0), ("brush 1200 px soft", 1200.0)] {
            b.app().session.tools.brush.size = px;
            let strokes = |b: &mut Bench| b.app().session.journal.iter().filter(|(id, _)| id == "paint.stroke").count();
            let before = strokes(&mut b);
            b.drag(name, [w * 0.25, h * 0.4], [w * 0.04, h * 0.02], 12);
            assert_eq!(strokes(&mut b), before + 1, "{name}: the drag painted a stroke");
        }
        b.app().ui.tool = Tool::Gradient;
        b.drag("gradient drag", [w * 0.2, h * 0.3], [w * 0.05, h * 0.03], 10);
        b.app().ui.tool = Tool::Move;
        b.run("undo", "edit.undo", json!({}));
        b.run("undo", "edit.undo", json!({}));
        b.run("undo", "edit.redo", json!({}));
        b.settle();
    }
    let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path).to_owned();
    Some(b.report(&name))
}

/// `f` on a thread with the stack the app's own thread has (the whole app's frame, unoptimized
/// in test builds, is deeper than a test thread's 2 MB).
fn on_the_apps_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(16 << 20).spawn(f).expect("a thread").join().expect("the run")
}

/// The owner's bar for CPU mode on his file at 1920x1080: the median frame of every action within
/// 16 ms and no frame over 33 ms. Timing on a shared server: run alone (not in the `fast` set).
#[test]
fn edits_on_his_file_on_the_cpu_canvas_stay_within_a_frame() {
    let Some(rows) = on_the_apps_stack(|| edits(PSD, 0.7)) else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, p50, max)| *p50 > 16.0 || *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "over the bar (median 16 ms, max 33 ms):\n{}", over.join("\n"));
}

/// The 200-layer page: reported.
#[test]
fn edits_on_the_200_layer_page_on_the_cpu_canvas_are_reported() {
    on_the_apps_stack(|| edits(PAGE200, 0.5));
}

/// Ctrl+T on the GPU canvas (his file, a type layer and the layer as large as the page): the
/// frame that opens the box and the frames until its images are made, with the images made off
/// the frame (the app) and in it (as before). Reported; skips without a GPU adapter.
#[test]
fn opening_the_transform_box_on_the_gpu_canvas_is_reported() {
    on_the_apps_stack(|| {
        let _gpu = photocraft_testkit::gpu_lock();
        if !std::path::Path::new(PSD).exists() {
            return;
        }
        let Some(mut h) = harness(Device::Gpu) else { return };
        {
            let app = h.state_mut();
            app.background_jobs = false;
            app.open_path(PSD).expect("open");
            app.sync_views();
            app.ui.extras.rulers = false;
        }
        h.run_steps(20);
        if !crate::move_repeat_tests::gpu_drawing(&mut h, Device::Gpu) {
            return;
        }
        h.run_steps(40);
        let t = targets(h.state());
        for off_frame in [true, false, true, false] {
            crate::transform_tool::set_off_frame(off_frame);
            for (name, id) in [("type", t.text), ("page-size", t.big)] {
                h.state_mut().run("layer.select", json!({"layer": id.0})).expect("select");
                h.run_steps(30);
                let mut frames = Vec::new();
                let start = std::time::Instant::now();
                let ctx = h.ctx.clone();
                crate::transform_tool::begin(h.state_mut(), &ctx).expect("the box");
                h.step();
                frames.push(start.elapsed().as_secs_f64() * 1000.0);
                let mut made_after = None;
                for _ in 0..600 {
                    if made_after.is_none() && h.state().transform_preview.as_ref().is_none_or(|pv| pv.made()) {
                        made_after = Some(start.elapsed().as_secs_f64() * 1000.0);
                        // The frames that draw the box for the first time.
                        for _ in 0..3 {
                            let f = std::time::Instant::now();
                            h.step();
                            frames.push(f.elapsed().as_secs_f64() * 1000.0);
                        }
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    let f = std::time::Instant::now();
                    h.step();
                    frames.push(f.elapsed().as_secs_f64() * 1000.0);
                }
                eprintln!(
                    "BOX gpu {name:<10} images {}: the frame of Ctrl+T {:>6.1} ms, worst frame {:>6.1} ms, median {:>5.1} ms, box drawn after {:>6.1} ms ({} frames)",
                    if off_frame { "off the frame" } else { "in the frame " },
                    frames[0],
                    pct(&frames, 1.0),
                    pct(&frames, 0.5),
                    made_after.unwrap_or(f64::INFINITY),
                    frames.len()
                );
                crate::transform_tool::cancel(h.state_mut());
                h.run_steps(10);
            }
        }
        crate::transform_tool::set_off_frame(false);
    });
}
