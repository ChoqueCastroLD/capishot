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

/// What the kernel says of this thread so far: the time it ran and the time it waited for a core
/// while ready to run (milliseconds; Linux's `schedstat`). A frame's wall time less its wait for a
/// core is the frame's own: what it computed and what it was blocked for (a lock, a sleep, a job).
/// On this shared server the wait for a core is other people's builds.
fn scheduled() -> Option<(f64, f64)> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let mut ns = stat.split_whitespace().map(|v| v.parse::<f64>().ok());
    Some((ns.next()?? / 1e6, ns.next()?? / 1e6))
}

/// The monotonic clock in seconds, as `perf` stamps its samples (`-k CLOCK_MONOTONIC`).
fn monotonic() -> f64 {
    let up = std::fs::read_to_string("/proc/uptime").ok();
    // (`/proc/uptime` is the boot clock; without a suspend it is the monotonic one.)
    up.and_then(|u| u.split_whitespace().next()?.parse().ok()).unwrap_or(0.0)
}

/// The frames of one kind of action.
#[derive(Default)]
struct Action {
    name: &'static str,
    /// Each frame's own time: wall time less the wait for a core ([`scheduled`]).
    ms: Vec<f64>,
    /// The slowest frame: its spans, and how its wall time splits.
    worst: (f64, String),
    /// From each action to the frame that shows it complete.
    shown_after: Vec<f64>,
}

struct Bench {
    h: Harness,
    actions: Vec<Action>,
    /// Render each frame after it is timed, as the app does (the GPU canvas's paint callbacks
    /// run then).
    render: bool,
}

impl Bench {
    /// `path` open on the CPU canvas at `zoom` around the page's middle, settled.
    fn open(path: &str, zoom: f64) -> Option<Bench> {
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} isn't here");
            return None;
        }
        let mut h = harness(Device::Cpu)?;
        if std::env::var_os("CPU_EDIT_PERF_TRACE").is_some() {
            eprintln!("BENCH thread {:?}", std::fs::read_link("/proc/thread-self"));
        }
        // Edits composited on the worker pool, as in the app (tests composite in the frame).
        crate::cpu_canvas::set_pooled(true);
        // Strokes rendered on the worker pool, as in the app (tests render them in the frame).
        crate::canvas::set_strokes_off_frame(true);
        // Selection outlines traced off the frame, as in the app (tests trace them in the frame).
        crate::outline::set_off_frame(true);
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
        let mut b = Bench { h, actions: Vec::new(), render: false };
        b.settle();
        Some(b)
    }

    fn app(&mut self) -> &mut PhotocraftApp {
        self.h.state_mut()
    }

    /// Frames until the canvas has nothing left to draw or make ahead.
    fn settle(&mut self) {
        // (A slow worker pool takes seconds over what it makes ahead.)
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_secs(30) {
            self.h.step();
            if crate::cpu_canvas::settled(self.h.state()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        self.h.run_steps(3);
    }

    /// A frame of `name` took `wall` ms, of which the thread ran `ran` and waited `queued` for a
    /// core (`None`: the kernel doesn't say; the wall time is the frame's).
    fn record(&mut self, name: &'static str, wall: f64, sched: Option<(f64, f64)>) {
        let (ran, queued) = sched.unwrap_or((wall, 0.0));
        let ms = (wall - queued).max(0.0);
        let mut spans: Vec<String> = crate::perf_span::frame().iter().take(6).map(|(n, ms)| format!("{n} {ms:.1}")).collect();
        spans.push(format!("[wall {wall:.1}: ran {ran:.1}, blocked {:.1}, no core {queued:.1}]", (ms - ran).max(0.0)));
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
        let (t, before) = (std::time::Instant::now(), scheduled());
        act(&mut self.h);
        self.h.step();
        let wall = t.elapsed().as_secs_f64() * 1000.0;
        let sched = before.zip(scheduled()).map(|(b, a)| (a.0 - b.0, a.1 - b.1));
        // For a trace of the run (`perf record -t <thread>`, the thread `Bench::open` names): the
        // frames over 25 ms of their own, with when they ended (the monotonic clock, seconds).
        if std::env::var_os("CPU_EDIT_PERF_TRACE").is_some()
            && let Some((ran, queued)) = sched
            && wall - queued > 25.0
        {
            let spans: Vec<String> = crate::perf_span::frame().iter().take(8).map(|(n, ms)| format!("{n} {ms:.1}")).collect();
            eprintln!("SLOW {name}: {:.1} ms (ran {ran:.1}, blocked {:.1}), ended at {:.4}: {}", wall - queued, wall - ran - queued, monotonic(), spans.join(", "));
        }
        self.record(name, wall, sched);
        if self.render {
            self.h.render().expect("render");
        }
    }

    /// `act`, then the frames until the canvas shows it complete and has nothing left to make
    /// (16 ms apart, as a display's are): all counted as `name`'s.
    fn action(&mut self, name: &'static str, act: impl FnOnce(&mut Harness)) {
        let t = std::time::Instant::now();
        self.frame(name, act);
        let mut shown = None;
        // (A slow worker pool shows an edit seconds later: the frames go on until it does.)
        while t.elapsed() < std::time::Duration::from_secs(30) {
            let made = self.h.state().transform_preview.as_ref().is_none_or(|pv| pv.made());
            let applied = !self.h.state().session.has_jobs() && self.h.state().landing_stroke.is_none();
            if shown.is_none() && made && applied && crate::cpu_canvas::shows_document(self.h.state()) {
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
        // Free Transform commit: a background job, as in the app (`CPU_EDIT_PERF_INLINE_JOBS` runs
        // the command in the frame).
        b.app().background_jobs = std::env::var_os("CPU_EDIT_PERF_INLINE_JOBS").is_none();
        for (name, id) in [("transform commit type", t.text), ("transform commit page-size", t.big)] {
            b.app().run("layer.select", json!({"layer": id.0})).expect("select");
            b.settle();
            b.run(name, "edit.transform", json!({"scaleX": 104, "scaleY": 104}));
            b.run(name, "edit.undo", json!({}));
        }
        b.app().background_jobs = false;
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
    // The commands of a click and the crop: jobs, as in the app (`CPU_EDIT_PERF_INLINE_JOBS` runs
    // them in the frame, as before). On a new layer over the page, reading every layer.
    b.app().run("layer.new.layer", json!({"name": "bucket"})).expect("layer");
    b.settle();
    b.app().background_jobs = std::env::var_os("CPU_EDIT_PERF_INLINE_JOBS").is_none();
    let (cx, cy) = (size.width / 2, size.height / 2);
    for _ in 0..3 {
        b.run("paint bucket", "paint.bucket", json!({"x": cx, "y": cy, "tolerance": 32, "sampleAllLayers": true, "color": "#3366cc", "opacity": 40}));
        b.run("fill", "edit.fill", json!({"contents": "color", "color": "#22aa55", "opacity": 30}));
        b.run("magic wand", "select.magicWand", json!({"x": cx, "y": cy, "tolerance": 32, "sampleAllLayers": true}));
        b.run("deselect", "select.deselect", json!({}));
        b.run("crop", "image.crop", json!({"x": size.width / 4, "y": size.height / 4, "width": cx, "height": cy}));
        b.run("crop undo", "edit.undo", json!({}));
    }
    b.app().background_jobs = false;
    let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path).to_owned();
    Some(b.report(&name))
}

/// The retouching tools drawn across `path` with a large soft tip, on a pixel layer holding the
/// page (Stamp Visible): a press, twelve moves a frame apart and the release, as the app runs
/// them (strokes rendered and committed off the window's thread, commands that are jobs as jobs).
/// `None` when the file isn't here.
fn retouch(path: &str, zoom: f64) -> Option<Vec<(&'static str, f64, f64)>> {
    let _gpu = photocraft_testkit::gpu_lock();
    let mut b = Bench::open(path, zoom)?;
    let size = b.app().session.active().expect("doc").doc.size;
    let (w, h) = (size.width as f32, size.height as f32);
    b.app().run("layer.stampVisible", json!({})).expect("a pixel layer with the page");
    b.settle();
    b.app().background_jobs = true;
    b.app().session.tools.brush.hardness = 0.0;
    b.app().ui.clone_source = Some([f64::from(w) * 0.6, f64::from(h) * 0.7]);
    let tools = [
        ("clone stamp 600 px", Tool::CloneStamp, "paint.cloneStamp", 600.0),
        ("healing brush 300 px", Tool::Healing, "paint.healingBrush", 300.0),
        ("spot healing 100 px", Tool::SpotHealing, "paint.spotHealing", 100.0),
        ("dodge 600 px", Tool::Dodge, "paint.dodge", 600.0),
        ("burn 600 px", Tool::Burn, "paint.burn", 600.0),
        ("sponge 600 px", Tool::Sponge, "paint.sponge", 600.0),
        ("smudge 300 px", Tool::Smudge, "paint.smudge", 300.0),
        ("blur 300 px", Tool::Blur, "paint.blur", 300.0),
        ("sharpen 300 px", Tool::Sharpen, "paint.sharpen", 300.0),
        ("mixer brush 600 px", Tool::MixerBrush, "paint.mixerBrush", 600.0),
    ];
    for _ in 0..2 {
        for (name, tool, cmd, px) in tools {
            b.app().ui.tool = tool;
            b.app().session.tools.brush.size = px;
            let strokes = |b: &mut Bench| b.app().session.journal.iter().filter(|(id, _)| id == cmd).count();
            let before = strokes(&mut b);
            b.drag(name, [w * 0.25, h * 0.4], [w * 0.03, h * 0.015], 12);
            assert_eq!(strokes(&mut b), before + 1, "{name}: the drag committed a stroke");
        }
    }
    b.app().background_jobs = false;
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
#[ignore = "a timing bar on a shared machine: run alone with --ignored"]
fn edits_on_his_file_on_the_cpu_canvas_stay_within_a_frame() {
    let Some(rows) = on_the_apps_stack(|| edits(PSD, 0.7)) else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, p50, max)| *p50 > 16.0 || *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "over the bar (median 16 ms, max 33 ms):\n{}", over.join("\n"));
}

/// The same bar for a frame's own work on a weak machine: a worker pool of two threads whose
/// jobs take eight times as long (the owner's PC composites 6 to 8 times slower than the build
/// server). An edit shows later there; no frame may be held longer for it, which fails as soon as
/// a frame composites, waits for the pool or makes a page of textures itself. Run alone, in its
/// own process (the pool's size is the process's): `--ignored --test-threads=1` with this name.
#[test]
#[ignore = "a timing bar on a shared machine: run alone with --ignored"]
fn edits_on_his_file_with_a_slow_worker_pool_hold_no_frame() {
    let rows = on_the_apps_stack(|| {
        // (Fails when the pool was made already: the test then runs with this machine's threads.)
        let two = rayon::ThreadPoolBuilder::new().num_threads(2).build_global().is_ok();
        eprintln!("EDIT cpu slow pool: {} threads, jobs 8 times as long", if two { 2 } else { rayon::current_num_threads() });
        crate::cpu_canvas::set_pool_slowdown(8);
        let rows = edits(PSD, 0.7);
        crate::cpu_canvas::set_pool_slowdown(1);
        rows
    });
    let Some(rows) = rows else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, _, max)| *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "a frame's own work over 33 ms with a slow worker pool:\n{}", over.join("\n"));
}

/// The retouching tools on his file with a large tip: no frame of a stroke renders a dab, so
/// none is over 33 ms. Timing on a shared server: run alone.
#[test]
#[ignore = "a timing bar on a shared machine: run alone with --ignored"]
fn retouch_strokes_on_his_file_on_the_cpu_canvas_hold_no_frame() {
    let Some(rows) = on_the_apps_stack(|| retouch(PSD, 0.7)) else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, _, max)| *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "a frame over 33 ms:\n{}", over.join("\n"));
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

/// Whether the GPU canvas shows the active document as it is and has nothing left to draw.
pub(crate) fn gpu_settled(app: &PhotocraftApp) -> bool {
    let Some(st) = app.session.active() else { return true };
    let (Some(gpu), Some(c)) = (app.gpu.as_ref(), app.canvases.get(&(st.doc.id, crate::canvas::GPU_OUTPUT))) else { return false };
    !app.session.has_jobs() && c.on_gpu && c.revision == st.revision && !gpu.has_stale(st.doc.id.0) && !gpu.shows_before(st.doc.id.0)
}

/// A crop of his file, its undo and its redo on the GPU canvas (lavapipe here), as the app runs
/// them (the crop a job): every frame's own time until the canvas is settled, with its spans.
fn gpu_crops(path: &str, zoom: f64) -> Option<Vec<(&'static str, f64, f64)>> {
    let _gpu = photocraft_testkit::gpu_lock();
    if !std::path::Path::new(path).exists() {
        eprintln!("skipping: {path} isn't here");
        return None;
    }
    let mut h = harness(Device::Gpu)?;
    {
        let app = h.state_mut();
        app.open_path(path).expect("open");
        app.sync_views();
        app.ui.extras.rulers = false;
    }
    let size = h.state().session.active().expect("doc").doc.size;
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"zoom": zoom, "center": [size.width / 2, size.height / 2]}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    // (`CROP_FRAME_WORK_MS`: the canvas's work a frame, as another build had it.)
    crate::gpu_canvas::set_frame_work_ms(std::env::var("CROP_FRAME_WORK_MS").ok().and_then(|v| v.parse().ok()));
    let mut b = Bench { h, actions: Vec::new(), render: true };
    let settle = |b: &mut Bench| {
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_secs(60) && !gpu_settled(b.h.state()) {
            b.h.step();
            b.h.render().expect("render");
        }
        b.h.run_steps(3);
    };
    settle(&mut b);
    b.app().background_jobs = true;
    for _ in 0..3 {
        for (name, cmd, params) in [
            ("gpu crop", "image.crop", json!({"x": size.width / 4 + 3, "y": size.height / 4 + 5, "width": size.width / 2, "height": size.height / 2})),
            ("gpu crop undo", "edit.undo", json!({})),
            ("gpu crop redo", "edit.redo", json!({})),
        ] {
            let t = std::time::Instant::now();
            b.frame(name, |h| {
                h.state_mut().run(cmd, params).unwrap_or_else(|e| panic!("{cmd}: {e}"));
            });
            let mut shown = None;
            while t.elapsed() < std::time::Duration::from_secs(60) {
                if gpu_settled(b.h.state()) {
                    shown = Some(t.elapsed().as_secs_f64() * 1000.0);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
                b.frame(name, |_| {});
            }
            if let Some(a) = b.actions.iter_mut().find(|a| a.name == name) {
                a.shown_after.push(shown.unwrap_or(f64::INFINITY));
            }
            settle(&mut b);
        }
        b.run_undo_to_start();
    }
    let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path).to_owned();
    Some(b.report(&name))
}

impl Bench {
    /// Back to the file as opened (each round crops it again).
    fn run_undo_to_start(&mut self) {
        self.app().run("edit.undo", json!({})).expect("undo");
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_secs(60) && !gpu_settled(self.h.state()) {
            self.h.step();
            self.h.render().expect("render");
        }
    }
}

/// A crop and its undo and redo on the GPU canvas: the frames each takes until the page shows.
/// Timing on a shared server: run alone with --ignored.
#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn crops_on_his_file_on_the_gpu_canvas() {
    let Some(rows) = on_the_apps_stack(|| gpu_crops(PSD, 0.7)) else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, _, max)| *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "over 33 ms:\n{}", over.join("\n"));
}

/// The same on the 200-layer page (the owner's PC: frames of 518 to 675 ms in a crop's undo). Timing
/// on a shared server: run alone with --ignored.
#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn crops_on_the_200_layer_page_on_the_gpu_canvas() {
    let Some(rows) = on_the_apps_stack(|| gpu_crops(PAGE200, 0.3)) else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, _, max)| *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "over 33 ms:\n{}", over.join("\n"));
}

/// The 200-layer page opened on the GPU canvas at fit (the owner's PC, 0.13.43: 308 ms in the
/// first frames, gpu.fx 173, gpu.submit 70, gpu.upload 48): every frame's own time until it shows
/// sharp, with the spans of the slowest. Timing on a shared server: run alone with --ignored.
#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn opening_the_200_layer_page_on_the_gpu_canvas() {
    let rows = on_the_apps_stack(|| {
        let _gpu = photocraft_testkit::gpu_lock();
        if !std::path::Path::new(PAGE200).exists() {
            eprintln!("skipping: {PAGE200} isn't here");
            return None;
        }
        let mut h = harness(Device::Gpu)?;
        // The compositor ready on a small page first (the app builds its pipelines at start).
        h.state_mut().ui.extras.rulers = false;
        h.state_mut().run("file.new", json!({"width": 64, "height": 64, "background": "white"})).expect("a page");
        h.run_steps(4);
        if !crate::move_repeat_tests::gpu_drawing(&mut h, Device::Gpu) {
            return None;
        }
        // Its other pipelines built ahead (on a thread at start; long before a user opens a page).
        let gpu = h.state().gpu.clone()?;
        let (mut built, mut still) = (0, std::time::Instant::now());
        while still.elapsed() < std::time::Duration::from_secs(2) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(50));
            let now = gpu.pipelines_built().unwrap_or(0);
            if now != built {
                (built, still) = (now, std::time::Instant::now());
            }
        }
        // The Move tool in hand, as the app starts.
        h.state_mut().ui.tool = Tool::Move;
        // Decoded before (a user's open decodes in a job).
        let doc = photocraft_io::import_with_options(PAGE200, &std::fs::read(PAGE200).expect("read"), &Default::default()).expect("decode").document;
        let mut b = Bench { h, actions: Vec::new(), render: true };
        let t = std::time::Instant::now();
        b.frame("gpu open page200", move |h| {
            let app = h.state_mut();
            app.session.add_document(doc, None);
            app.sync_views();
        });
        let ctx = b.h.ctx.clone();
        let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"fit": true}));
        crate::control::handle(b.app(), &ctx, &req);
        let mut shown = None;
        while t.elapsed() < std::time::Duration::from_secs(120) {
            if gpu_settled(b.h.state()) {
                shown = Some(t.elapsed().as_secs_f64() * 1000.0);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            b.frame("gpu open page200", |_| {});
        }
        if let Some(a) = b.actions.first_mut() {
            a.shown_after.push(shown.unwrap_or(f64::INFINITY));
        }
        Some(b.report("page200.psd"))
    });
    let Some(rows) = rows else { return };
    if std::env::var_os("CPU_EDIT_PERF_REPORT_ONLY").is_some() {
        return;
    }
    let over: Vec<String> = rows.iter().filter(|(_, _, max)| *max > 33.0).map(|(n, p50, max)| format!("{n}: median {p50:.1} ms, max {max:.1} ms")).collect();
    assert!(over.is_empty(), "over 33 ms:\n{}", over.join("\n"));
}

/// An 8K page (7680x4320, or a 7680x12000 strip) of a few dozen layers: pixel layers as large as
/// the page, Smart Objects, type and shapes with styles. Opened, then the main tools played on
/// the GPU canvas or the CPU canvas: every frame's own time (wall less the wait for a core) and
/// the spans of the slowest of each action. The owner's bar at 8K: no hang, no crash, no frame
/// over about 50-100 ms. Timing on a shared server: run alone with --ignored.
fn eight_k(device: Device, h_px: u32) -> Option<Vec<(&'static str, f64, f64)>> {
    let _gpu = photocraft_testkit::gpu_lock();
    let mut h = harness(device)?;
    // Strokes rendered and selection outlines traced off the frame on either canvas, and edits
    // composited on the worker pool on the CPU one, as in the app (tests do them in the frame).
    crate::canvas::set_strokes_off_frame(true);
    crate::outline::set_off_frame(true);
    if device == Device::Cpu {
        crate::cpu_canvas::set_pooled(true);
    }
    let (w, ht) = (7680i32, h_px as i32);
    let (text, shape) = {
        let app = h.state_mut();
        app.background_jobs = false;
        app.ui.extras.rulers = false;
        app.run("file.new", json!({"width": w, "height": ht, "background": "white", "name": "8k"})).expect("doc");
        crate::move_repeat_tests::stripes(app, "art", w, ht, 0);
        crate::move_repeat_tests::stripes(app, "tones", w, ht, 5);
        for i in 0..3 {
            crate::move_repeat_tests::stripes(app, &format!("smart {i}"), w / 3, ht / 3, 11 + i);
            app.run("layer.smartObjects.convertToSmartObject", json!({})).expect("smart");
        }
        let mut shape = None;
        for i in 0..8 {
            let (x, y) = (200 + (i % 4) * 1800, 300 + (i / 4) * (ht / 2));
            let l = crate::move_repeat_tests::layer_id(app.run("shape.create", json!({"rect": [x, y, x + 1400, y + 900], "fill": "#8090b0"})).expect("shape"));
            app.run("layer.layerStyle.dropShadow", json!({"layer": l.0})).expect("shadow");
            app.run("layer.layerStyle.stroke", json!({"layer": l.0, "size": 8, "position": "outside", "color": "#ffffff"})).expect("stroke");
            shape.get_or_insert(l);
        }
        let mut text = None;
        for i in 0..16u32 {
            let l = crate::move_repeat_tests::styled_text(app, 300 + (i % 4) * 1800, 200 + (i / 4) * (h_px / 4), "TEXTO DE PRUEBA", 140);
            text.get_or_insert(l);
        }
        app.sync_views();
        (text?, shape?)
    };
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"fit": true}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    let settled = |app: &PhotocraftApp| {
        !app.session.has_jobs() && app.landing_stroke.is_none() && if device == Device::Cpu { crate::cpu_canvas::settled(app) } else { gpu_settled(app) }
    };
    let mut b = Bench { h, actions: Vec::new(), render: device != Device::Cpu };
    // Until the canvas shows the action and has nothing left to draw (60 s at most).
    fn until(b: &mut Bench, name: &'static str, settled: &dyn Fn(&PhotocraftApp) -> bool) {
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_secs(60) && !settled(b.h.state()) {
            std::thread::sleep(std::time::Duration::from_millis(4));
            b.frame(name, |_| {});
        }
    }
    // A real drag with the tool in hand, then the frames until it has landed.
    fn drag(b: &mut Bench, name: &'static str, from: [f32; 2], step: [f32; 2], n: usize, settled: &dyn Fn(&PhotocraftApp) -> bool) {
        let p0 = screen(&b.h, from[0], from[1]);
        b.h.event(Event::PointerMoved(p0));
        b.h.step();
        b.frame(name, |h| h.event(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }));
        let mut p = p0;
        for i in 1..=n {
            p = screen(&b.h, from[0] + step[0] * i as f32, from[1] + step[1] * i as f32);
            b.frame(name, |h| h.event(Event::PointerMoved(p)));
        }
        b.frame(name, |h| h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }));
        until(b, name, settled);
    }
    // `EIGHT_K_ACTIONS=move,brush`: only the actions whose names hold one of these (all by default).
    let only = std::env::var("EIGHT_K_ACTIONS").ok();
    let wanted = |name: &str| only.as_deref().is_none_or(|o| o.split(',').any(|w| name.contains(w)));
    let t0 = std::time::Instant::now();
    b.frame("open (first frames)", |_| {});
    until(&mut b, "open (first frames)", &settled);
    eprintln!("8K {device:?} 7680x{h_px}: shown after {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    b.app().background_jobs = true;
    let at = |b: &mut Bench, id: LayerId| {
        let doc = b.app().session.active().expect("doc").doc.clone();
        let r = doc.layer(id).and_then(|l| photocraft_compose::change_bounds(l, doc.bounds())).expect("bounds");
        [r.x0 as f32 + r.width() as f32 / 3.0, (r.y0 + r.y1) as f32 / 2.0]
    };
    b.app().ui.tool = Tool::Move;
    b.app().ui.tool_options.move_auto_select = false;
    for (name, id) in [("move type", text), ("move styled shape", shape)].into_iter().filter(|(n, _)| wanted(n)) {
        b.app().run("layer.select", json!({"layer": id.0})).expect("select");
        until(&mut b, name, &settled);
        let from = at(&mut b, id);
        drag(&mut b, name, from, [40.0, 16.0], 10, &settled);
    }
    b.app().run("layer.new.layer", json!({"name": "paint"})).expect("layer");
    b.app().ui.tool = Tool::Brush;
    b.app().session.tools.brush.hardness = 0.0;
    for (name, px) in [("brush 300 px soft", 300.0), ("brush 1200 px soft", 1200.0)].into_iter().filter(|(n, _)| wanted(n)) {
        b.app().session.tools.brush.size = px;
        drag(&mut b, name, [w as f32 * 0.25, ht as f32 * 0.4], [w as f32 * 0.03, ht as f32 * 0.01], 12, &settled);
    }
    b.app().run("layer.select", json!({"layer": text.0})).expect("select");
    for _ in 0..if wanted("typing") { 4 } else { 0 } {
        b.frame("typing", |h| {
            h.state_mut().run("type.insertText", json!({"layer": text.0, "text": "a"})).expect("type");
        });
        until(&mut b, "typing", &settled);
    }
    for (name, cmd, params) in [
        ("select all", "select.all", json!({})),
        ("deselect", "select.deselect", json!({})),
        ("transform commit type", "edit.transform", json!({"layer": text.0, "scaleX": 104, "scaleY": 104})),
        ("undo", "edit.undo", json!({})),
        ("crop", "image.crop", json!({"x": w / 4, "y": ht / 4, "width": w / 2, "height": ht / 2})),
        ("crop undo", "edit.undo", json!({})),
    ]
    .into_iter()
    .filter(|(n, ..)| wanted(n))
    {
        b.frame(name, |h| {
            h.state_mut().run(cmd, params).unwrap_or_else(|e| panic!("{cmd}: {e}"));
        });
        until(&mut b, name, &settled);
    }
    let rss = std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines().find(|l| l.starts_with("VmHWM")).map(str::to_owned));
    eprintln!("8K {device:?} 7680x{h_px}: peak memory {rss:?}");
    Some(b.report(if h_px > 5000 { "8k-strip" } else { "8k" }))
}

#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn an_8k_page_on_the_gpu_canvas() {
    on_the_apps_stack(|| eight_k(Device::Gpu, 4320));
}

#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn an_8k_page_on_the_cpu_canvas() {
    on_the_apps_stack(|| eight_k(Device::Cpu, 4320));
}

#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn an_8k_strip_on_the_gpu_canvas() {
    on_the_apps_stack(|| eight_k(Device::Gpu, 12000));
}

#[test]
#[ignore = "a timing bench on a shared machine: run alone with --ignored"]
fn an_8k_strip_on_the_cpu_canvas() {
    on_the_apps_stack(|| eight_k(Device::Cpu, 12000));
}
