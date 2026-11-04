//! UI tests for background jobs (#210): the whole app (eframe harness, CPU canvas) keeps
//! rendering frames while a slow job runs, shows the status-bar progress and the modal progress
//! dialog, and Esc / Cancel leave the document unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_engine::jobs::{JobId, Started};
use serde_json::{Value, json};

use crate::PhotocraftApp;

fn app_harness() -> Harness<'static, PhotocraftApp> {
    app_harness_with(crate::Services::default())
}

/// [`app_harness`] over the given services (the beep test records the hook's calls).
fn app_harness_with(services: crate::Services) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(8).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 320, "height": 240})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("edit.fill", json!({"color": "#3366cc"})).unwrap();
        let mut app = PhotocraftApp::new(s, services);
        app.background_jobs = true;
        app
    });
    h.run_steps(4);
    h
}

/// A fake document job that runs until `gate` opens (or it is cancelled), reporting 40 %.
fn slow_job(app: &mut PhotocraftApp, gate: &Arc<AtomicBool>) -> JobId {
    let gate = gate.clone();
    let r = app
        .session
        .start_job(
            "test.slow",
            json!({}),
            "Slow Filter",
            true,
            move |ctx| {
                ctx.progress(0.4, "Filtering tiles");
                while !gate.load(Ordering::Relaxed) {
                    ctx.check()?;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
            |s, ()| {
                s.edit("Slow Filter", |_, _| Ok(()))?;
                Ok(json!({"slow": true}))
            },
        )
        .unwrap();
    match r {
        Started::Job(id) => id,
        Started::Done(v) => panic!("ran inline: {v}"),
    }
}

/// Step frames for `ms` of wall time; returns (frames, slowest frame in ms).
fn frames_for(h: &mut Harness<'_, PhotocraftApp>, ms: u64) -> (u64, f64) {
    frames_for_at_least(h, ms, 0)
}

/// Prove the frame loop advances while a gated worker stays blocked, without assuming a
/// minimum frame rate on software-rendered CI machines. Also wait for the progress modal.
fn frames_for_at_least(h: &mut Harness<'_, PhotocraftApp>, ms: u64, min_frames: u64) -> (u64, f64) {
    let t = Instant::now();
    let (mut n, mut worst) = (0, 0.0f64);
    while t.elapsed() < Duration::from_millis(ms) || n < min_frames {
        assert!(t.elapsed() < Duration::from_secs(30), "frame observation timed out after {n} frames");
        let f = Instant::now();
        h.step();
        worst = worst.max(f.elapsed().as_secs_f64() * 1000.0);
        n += 1;
    }
    (n, worst)
}

/// Step at least `min` frames and until `ready` holds; returns (frames, slowest frame in ms).
/// The test fails when `ready` still does not hold after 60 s.
fn frames_until(h: &mut Harness<'_, PhotocraftApp>, min: u64, ready: impl Fn(&Harness<'_, PhotocraftApp>) -> bool) -> (u64, f64) {
    let t = Instant::now();
    let (mut n, mut worst) = (0, 0.0f64);
    while n < min || !ready(h) {
        assert!(t.elapsed() < Duration::from_secs(60), "not ready after {n} frames");
        let f = Instant::now();
        h.step();
        worst = worst.max(f.elapsed().as_secs_f64() * 1000.0);
        n += 1;
    }
    (n, worst)
}

#[test]
fn the_frame_loop_keeps_running_during_a_slow_job_and_esc_cancels_it() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let doc_before = h.state().session.active().unwrap().doc.clone();
    let job = slow_job(h.state_mut(), &gate);
    let frame0 = h.state().frame;
    // The worker thread reports "Filtering tiles" when it starts, which can be after the dialog appears.
    let (n, worst) = frames_until(&mut h, 10, |h| h.query_by_label("Filtering tiles").is_some());
    assert!(h.state().session.job(job).is_some(), "the job is still running");
    assert!(h.state().frame - frame0 >= 10, "the UI kept rendering ({n} frames)");
    eprintln!("{n} frames during the job, slowest {worst:.1} ms");
    // The status bar shows the job, and after the delay the modal progress dialog.
    assert!(h.query_all_by_label("Slow Filter").count() >= 2, "job label in the status bar and the dialog title");
    assert!(h.query_by_label("Cancel").is_some(), "the progress dialog's Cancel");
    assert!(h.query_by_label("Filtering tiles").is_some(), "the job's status message");
    // Commands that would edit the busy document are disabled with a clear reason.
    assert!(!h.state().session.is_enabled("layer.new.layer"));
    let err = h.state_mut().run("layer.new.layer", json!({})).unwrap_err();
    assert!(err.contains("Slow Filter"), "{err}");
    // Esc cancels; the document is untouched.
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().session.job(job).is_none());
    assert!(Arc::ptr_eq(&h.state().session.active().unwrap().doc, &doc_before));
    assert!(h.state().ui.status.contains("Cancelled"), "{}", h.state().ui.status);
    assert!(h.query_by_label("Esc to cancel").is_none(), "the dialog closed");
}

#[test]
fn a_job_started_while_drawing_requests_the_follow_up_poll() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let job = slow_job(h.state_mut(), &gate);
    let repaints = Arc::new(AtomicUsize::new(0));
    let seen = repaints.clone();
    h.ctx.set_request_repaint_callback(move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
    });

    // A job may start from a menu or dialog after the frame's initial `jobs_ui::tick`.
    // The UI must still schedule a poll, or an idle Wayland event loop leaves the document
    // locked after the worker finishes and blocks Edit > Undo / Redo.
    assert!(crate::jobs_ui::keep_polling(h.state(), &h.ctx));
    assert_eq!(repaints.load(Ordering::Relaxed), 1);

    crate::jobs_ui::cancel(h.state_mut(), job);
    h.run_steps(2);
}

#[test]
fn a_finished_job_lands_as_one_undo_step_and_the_dialog_cancel_button_works() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let steps = h.state().session.active().unwrap().history.past_len();
    slow_job(h.state_mut(), &gate);
    frames_for(&mut h, 320);
    gate.store(true, Ordering::Relaxed);
    let t = Instant::now();
    while h.state().session.has_jobs() && t.elapsed() < Duration::from_secs(10) {
        h.step();
    }
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);

    // A second job, cancelled from the dialog's button.
    let gate = Arc::new(AtomicBool::new(false));
    let job = slow_job(h.state_mut(), &gate);
    frames_for(&mut h, 320);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().session.job(job).is_none());
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);
}

#[test]
fn filters_from_the_ui_run_in_the_background() {
    let mut h = app_harness();
    let steps = h.state().session.active().unwrap().history.past_len();
    let r = h.state_mut().run("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    assert_eq!(r["pending"], true, "{r}");
    let t = Instant::now();
    while h.state().session.has_jobs() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(2);
    let st = h.state().session.active().unwrap();
    assert_eq!(st.history.past_len(), steps + 1, "one step for the blur");
    assert_eq!(h.state().ui.status, "Gaussian Blur");
}

#[test]
fn control_requests_wait_for_jobs_unless_asked_not_to() {
    let mut h = app_harness();
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::ControlRequest::new("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 2}}));
    let job = match crate::control::handle(h.state_mut(), &ctx, &req) {
        crate::control::Outcome::AfterJob(job) => job,
        _ => panic!("expected a deferred reply"),
    };
    assert!(h.state().session.jobs().iter().any(|j| j.id == job) || h.state().session.jobs_with_recent().iter().any(|j| j.id == job));
    // The app's frame loop answers the request when the job lands.
    let (req, rx) = crate::ControlRequest::new("engine.execute", json!({"command": "jobs.list"}));
    let _ = crate::control::handle(h.state_mut(), &ctx, &req);
    drop(rx);
    let reply = job_reply(&mut h, job);
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["result"]["filter"].is_object(), "{reply}");

    // `wait: false` replies with the job id at once.
    let (req, _rx) = crate::ControlRequest::new("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 2}, "wait": false}));
    let crate::control::Outcome::Done(v) = crate::control::handle(h.state_mut(), &ctx, &req) else { panic!("expected an immediate reply") };
    let id = v["result"]["job"].as_u64().unwrap();
    let (req, _rx) = crate::ControlRequest::new("jobs.cancel", json!({"job": id}));
    let crate::control::Outcome::Done(v) = crate::control::handle(h.state_mut(), &ctx, &req) else { panic!() };
    assert_eq!(v["ok"], true, "{v}");
    let (req, _rx) = crate::ControlRequest::new("jobs.cancel", json!({"job": 99999}));
    let crate::control::Outcome::Done(v) = crate::control::handle(h.state_mut(), &ctx, &req) else { panic!() };
    assert_eq!(v["ok"], false);
}

#[test]
fn accepted_control_job_applies_after_its_dispatch_deadline() {
    let mut h = app_harness();
    let ctx = h.ctx.clone();
    let steps = h.state().session.active().unwrap().history.past_len();
    let deadline = Instant::now() + Duration::from_secs(1);
    let (mut req, reply) = crate::ControlRequest::new("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 2}}));
    req.deadline = Some(deadline);
    let (tx, rx) = std::sync::mpsc::channel();
    h.state_mut().control_rx = Some(rx);
    tx.send(req).unwrap();
    h.state_mut().drain_control(&ctx);
    let job = h.state().jobs.waiters.first().expect("the accepted request waits for its job").0;
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps, "the job has not been applied yet");
    assert!(reply.try_recv().is_err());
    // No frames poll the worker until the transport's deadline has elapsed.
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert!(Instant::now() >= deadline);
    drop(reply);
    let result = job_reply(&mut h, job);
    assert_eq!(result["ok"], true, "{result}");
    assert!(result["result"]["filter"].is_object(), "{result}");
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);
}

/// The control reply for `job`, once the app's frame loop has applied it.
fn job_reply(h: &mut Harness<'static, PhotocraftApp>, job: JobId) -> Value {
    let (tx, rx) = std::sync::mpsc::channel();
    h.state_mut().jobs.waiters.push((job, tx));
    let t = Instant::now();
    loop {
        h.step();
        if let Ok(v) = rx.try_recv() {
            return v;
        }
        assert!(t.elapsed() < Duration::from_secs(30));
    }
}

/// #511: a filter started by a menu item (Blur More has no dialog) or by a filter dialog's OK
/// replies with its result once applied, like `engine.execute`; `wait: false` replies at once.
#[test]
fn menu_and_dialog_requests_wait_for_jobs_unless_asked_not_to() {
    let mut h = app_harness();
    let ctx = h.ctx.clone();
    let control = |h: &mut Harness<'static, PhotocraftApp>, method: &str, params: Value| {
        let (req, _rx) = crate::ControlRequest::new(method, params);
        crate::control::handle(h.state_mut(), &ctx, &req)
    };
    // The job a request started: deferred while waiting, `{job, pending}` at once otherwise.
    // Lets it land and checks the filter's result.
    let land = |h: &mut Harness<'static, PhotocraftApp>, what: &str, outcome: crate::control::Outcome, wait: bool| {
        let job = match (outcome, wait) {
            (crate::control::Outcome::AfterJob(job), true) => job,
            (crate::control::Outcome::Done(v), false) => {
                assert_eq!(v["result"]["pending"], true, "{what}: {v}");
                JobId(v["result"]["job"].as_u64().unwrap())
            }
            _ => panic!("{what}, wait {wait}: wrong reply kind"),
        };
        let reply = job_reply(h, job);
        assert!(reply["result"]["filter"].is_object(), "{what}: {reply}");
    };
    let steps = |h: &Harness<'static, PhotocraftApp>| h.state().session.active().unwrap().history.past_len();
    for wait in [true, false] {
        let before = steps(&h);
        let outcome = control(&mut h, "ui.menu.invoke", json!({"id": "filter.blur.blurMore", "wait": wait}));
        land(&mut h, "Blur More", outcome, wait);
        let crate::control::Outcome::Done(v) = control(&mut h, "ui.menu.invoke", json!({"id": "filter.blur.gaussianBlur"})) else { panic!("dialog") };
        let dialog = v["result"]["dialog"].as_u64().expect("Gaussian Blur opens its dialog");
        control(&mut h, "ui.dialog.set", json!({"dialog": dialog, "field": "radius", "value": 3}));
        let outcome = control(&mut h, "ui.dialog.confirm", json!({"dialog": dialog, "wait": wait}));
        land(&mut h, "Gaussian Blur", outcome, wait);
        assert_eq!(steps(&h), before + 2, "both filters applied");
    }
}

fn psd_bytes() -> Vec<u8> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    let doc = (*s.active().unwrap().doc).clone();
    photocraft_io::export(&doc, "x.psd", &Default::default()).unwrap().bytes
}

#[test]
fn files_open_in_the_background_with_a_tab_and_cancel_closes_it() {
    let mut h = app_harness();
    let bytes = psd_bytes();
    h.state_mut().open_file("/tmp/photocraft-test/bg.psd", &bytes).unwrap();
    // The tab is there before the document.
    assert_eq!(h.state().jobs.opens.len(), 1);
    assert_eq!(h.state().session.documents().len(), 1);
    let t = Instant::now();
    while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(2);
    let st = h.state().session.active().unwrap();
    assert_eq!(st.doc.size.width, 64);
    assert_eq!(st.path.as_deref(), Some("/tmp/photocraft-test/bg.psd"));
    assert_eq!(h.state().ui.recent_files.first().map(String::as_str), Some("/tmp/photocraft-test/bg.psd"));

    // Cancel: the half-open tab closes, no document is added.
    h.state_mut().open_file("/tmp/photocraft-test/bg2.psd", &bytes).unwrap();
    let job = h.state().jobs.opens[0].job;
    crate::jobs_ui::cancel(h.state_mut(), job);
    h.run_steps(3);
    assert!(h.state().jobs.opens.is_empty());
    assert_eq!(h.state().session.documents().len(), 2);
    assert!(h.state().jobs.focus.is_none());
}

/// #2423: a long file name used to wrap the card's title and push the percentage and Cancel
/// below the card. The name stays on one line and every row is inside the card.
#[test]
fn the_opening_card_keeps_a_long_name_and_cancel_inside() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let job = slow_job(h.state_mut(), &gate);
    let long = "a7rv-test_2024_02_17_shot_30_3705_with_an_even_longer_suffix_that_never_ends.psb";
    for name in ["short.psd", long] {
        h.state_mut().jobs.opens.clear();
        h.state_mut().jobs.opens.push(crate::jobs_ui::OpenTab {
            job,
            name: name.to_string(),
            path: None,
            slot: None,
            order: None,
            preview: None,
            texture: None,
            quiet: false,
        });
        h.state_mut().jobs.focus = Some(job);
        h.run_steps(4);
        let r = crate::jobs_ui::open_card_rects(&h.ctx).expect("the opening card was drawn");
        assert!(r.title.height() < 22.0, "{name}: the title is one line, {:?}", r.title);
        assert!(r.card.contains_rect(r.title), "{name}: title {:?} inside card {:?}", r.title, r.card);
        assert!(r.card.contains_rect(r.cancel), "{name}: Cancel {:?} inside card {:?}", r.cancel, r.card);
        assert!(r.title.right() <= r.card.right() - 10.0, "{name}: the title keeps the card's padding");
        assert!(r.cancel.width() > 0.0 && r.cancel.height() > 0.0, "{name}: Cancel was drawn");
    }
    gate.store(true, Ordering::Relaxed);
    h.state_mut().jobs.focus = None;
    h.state_mut().jobs.opens.clear();
    h.run_steps(4);
}

/// An opening PSD's merged image shows in its tab as soon as the job has read it, the card below
/// it; before that, the card alone in the middle. The texture goes with the tab.
#[test]
fn the_opening_card_shows_the_preview_once_read() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let job = slow_job(h.state_mut(), &gate);
    let slot: photocraft_engine::jobs::PreviewSlot = Arc::default();
    let tab = crate::jobs_ui::OpenTab {
        job,
        name: "page.psd".into(),
        path: None,
        slot: None,
        order: None,
        preview: Some(Arc::clone(&slot)),
        texture: None,
        quiet: false,
    };
    h.state_mut().jobs.opens.push(tab);
    h.state_mut().jobs.focus = Some(job);
    h.run_steps(3);
    let before = crate::jobs_ui::open_card_rects(&h.ctx).expect("the opening card was drawn");
    assert!(h.state().jobs.opens[0].texture.is_none(), "nothing read yet");
    let (w, h_) = (60u32, 90u32);
    let rgba: Vec<u8> = (0..w * h_).flat_map(|i| [(i % 251) as u8, 40, 200, 255]).collect();
    *slot.lock().unwrap() = Some(photocraft_engine::jobs::OpenPreview { width: w, height: h_, doc_width: 600, doc_height: 900, gray: false, icc: None, rgba });
    h.run_steps(3);
    assert!(h.state().jobs.opens[0].texture.is_some(), "the preview was uploaded");
    let after = crate::jobs_ui::open_card_rects(&h.ctx).expect("the opening card was drawn");
    assert!(after.card.center().y > before.card.center().y + 50.0, "the card moved below the page: {:?} → {:?}", before.card, after.card);
    let inspect = crate::jobs_ui::inspect(h.state());
    assert_eq!(inspect["opening"][0]["preview"], true);
    gate.store(true, Ordering::Relaxed);
    h.state_mut().jobs.focus = None;
    h.state_mut().jobs.opens.clear();
    h.run_steps(4);
}

/// A quiet open (the embed API's `{quiet: true}`): the card shows until the merged image is read,
/// then the page shows alone (no card over it).
#[test]
fn a_quiet_open_shows_the_preview_alone() {
    let mut h = app_harness();
    // A host's full-screen canvas (no tab strip), as the embedded mode has.
    h.state_mut().ui.view.screen_mode = "full".into();
    h.run_steps(2);
    let gate = Arc::new(AtomicBool::new(false));
    let job = slow_job(h.state_mut(), &gate);
    let slot: photocraft_engine::jobs::PreviewSlot = Arc::default();
    let tab = crate::jobs_ui::OpenTab {
        job,
        name: "page.psd".into(),
        path: None,
        slot: None,
        order: None,
        preview: Some(Arc::clone(&slot)),
        texture: None,
        quiet: true,
    };
    h.state_mut().jobs.opens.push(tab);
    h.state_mut().jobs.focus = Some(job);
    h.run_steps(3);
    assert!(crate::jobs_ui::open_card_rects(&h.ctx).is_some(), "the card until the preview is read");
    assert!(h.state().tab_strip.is_none(), "no tab strip: the canvas keeps its place");
    let rgba: Vec<u8> = (0..60 * 90).flat_map(|i| [(i % 251) as u8, 40, 200, 255]).collect();
    *slot.lock().unwrap() = Some(photocraft_engine::jobs::OpenPreview { width: 60, height: 90, doc_width: 600, doc_height: 900, gray: false, icc: None, rgba });
    h.ctx.data_mut(|d| d.remove::<crate::jobs_ui::OpenCardRects>(super::open_card_rects_id()));
    h.run_steps(3);
    assert!(h.state().jobs.opens[0].texture.is_some(), "the preview shows");
    assert!(crate::jobs_ui::open_card_rects(&h.ctx).is_none(), "no card over it");
    gate.store(true, Ordering::Relaxed);
    h.state_mut().jobs.focus = None;
    h.state_mut().jobs.opens.clear();
    h.run_steps(4);
}

/// A quiet open's landed document keeps its preview until its canvas has drawn the view, then
/// the canvas shows; a landing whose document isn't the active one is dropped.
#[test]
fn a_quiet_open_keeps_its_preview_until_the_canvas_is_drawn() {
    let mut h = app_harness();
    h.state_mut().ui.view.screen_mode = "full".into();
    // Stepped, as in the single-threaded browser build: the merged image is read a frame before
    // the layers.
    h.state_mut().background_jobs = false;
    h.state_mut().stepped_opens = true;
    h.run_steps(2);
    // Not one flat colour: a merged image that can stand for the page.
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    s.execute("filter.render.clouds", json!({})).unwrap();
    let page = photocraft_io::export(&s.active().unwrap().doc, "x.psd", &Default::default()).unwrap().bytes;
    h.state_mut().open_owned("page.psd", page).unwrap();
    h.state_mut().jobs.opens[0].quiet = true;
    h.state_mut().jobs.focus = Some(h.state().jobs.opens[0].job);
    let t = Instant::now();
    let mut landed = false;
    while t.elapsed() < Duration::from_secs(30) {
        let had_texture = h.state().jobs.opens.first().is_some_and(|o| o.texture.is_some());
        h.step();
        if h.state().jobs.opens.is_empty() {
            landed = had_texture;
            break;
        }
    }
    assert!(h.state().jobs.opens.is_empty(), "the open finished");
    assert!(landed, "the preview showed before the layers landed");
    // A small page is drawn in the frame it lands in: no frame of preview after it.
    let id = h.state().session.active().map(|st| st.doc.id).unwrap();
    let idx = h.state().session.active_index().unwrap();
    assert!(h.state().jobs.landing.is_none(), "the canvas took over");
    let ctx = h.ctx.clone();
    assert!(crate::canvas::prewarm(h.state_mut(), &ctx, idx, false), "the landed page's view is drawn");

    // A landing whose canvas isn't drawn yet and has run out of time also gives way.
    let tex = h.ctx.load_texture("t", egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]), egui::TextureOptions::LINEAR);
    let preview = super::PreviewTexture(tex, egui::vec2(1.0, 1.0));
    h.state_mut().jobs.landing = Some(super::Landing { doc: id, texture: preview.clone(), started_ms: -1e9 });
    h.run_steps(1);
    assert!(h.state().jobs.landing.is_none(), "timed out");
    assert_eq!(h.state().session.active().map(|st| st.doc.size.width), Some(64));

    // A landing for a document that isn't the active one goes away without drawing.
    let other = h.state().session.documents()[0].doc.id;
    let active = h.state().session.active().map(|st| st.doc.id);
    if Some(other) != active {
        h.state_mut().jobs.landing = Some(super::Landing { doc: other, texture: preview, started_ms: crate::gpu_canvas::now_ms() });
        h.run_steps(1);
        assert!(h.state().jobs.landing.is_none());
    }
}

/// The single-threaded browser build (no background jobs, opens a step at a time): a PSD handed
/// over opens as a job with an opening tab, not inline; other files still open inline.
#[test]
fn handed_over_psds_open_as_a_job_when_opens_are_stepped() {
    let mut h = app_harness();
    h.state_mut().background_jobs = false;
    h.state_mut().stepped_opens = true;
    let docs = h.state().session.documents().len();
    h.state_mut().open_owned("page.psd", psd_bytes()).unwrap();
    assert_eq!(h.state().jobs.opens.len(), 1, "an opening tab");
    assert_eq!(h.state().session.documents().len(), docs, "not open yet");
    let t = Instant::now();
    while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    assert_eq!(h.state().session.documents().len(), docs + 1);
    assert!(h.state().jobs.opens.is_empty());
}

/// Steps `h` until a tracked open ends; its outcome.
fn outcome_of(h: &mut Harness<'static, PhotocraftApp>, job: JobId) -> crate::jobs_ui::OpenOutcome {
    let t = Instant::now();
    loop {
        h.step();
        if let Some((_, o)) = h.state_mut().jobs.take_open_outcomes().into_iter().find(|(j, _)| *j == job) {
            return o;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "the open never ended");
    }
}

/// A hidden helper PSD the embed API opens decodes off the frame (a worker, or a step at a time
/// in the single-threaded browser build): no tab, no card, no status, nothing made active; its
/// tracked outcome names the hidden document. Bad bytes fail the outcome, nothing added.
#[test]
fn hidden_psds_open_in_a_job_without_a_tab() {
    for (background, stepped) in [(true, false), (false, true)] {
        let mut h = app_harness();
        h.state_mut().background_jobs = background;
        h.state_mut().stepped_opens = stepped;
        let active = h.state().session.active().map(|st| st.doc.id);
        let (docs, shown) = (h.state().session.documents().len(), h.state().session.visible_count());
        let status = h.state().ui.status.clone();
        let Ok(crate::Opening::Job(job)) = h.state_mut().open_hidden_owned("raw.psd", psd_bytes()) else { panic!("a job") };
        h.state_mut().jobs.track_open(job);
        assert!(h.state().jobs.opens.is_empty() && h.state().jobs.focus.is_none(), "no opening tab");
        let (id, _) = outcome_of(&mut h, job).expect("opened");
        let app = h.state();
        let st = app.session.documents().iter().find(|st| st.doc.id == id).expect("the document");
        assert!(st.hidden && st.doc.size.width == 64, "a hidden 64 px document");
        assert_eq!((app.session.documents().len(), app.session.visible_count()), (docs + 1, shown));
        assert_eq!(app.session.active().map(|st| st.doc.id), active, "the active document stays");
        assert_eq!(app.ui.status, status, "nothing said");
        let Ok(crate::Opening::Job(job)) = h.state_mut().open_hidden_owned("bad.psd", b"8BPS broken".to_vec()) else { panic!("a job") };
        h.state_mut().jobs.track_open(job);
        assert!(outcome_of(&mut h, job).is_err());
        assert_eq!(h.state().session.documents().len(), docs + 1);
    }
    // Stepped opens take a PSD only: other files open at once, as before.
    let mut h = app_harness();
    (h.state_mut().background_jobs, h.state_mut().stepped_opens) = (false, true);
    let doc = (*h.state().session.active().unwrap().doc).clone();
    let png = photocraft_io::export(&doc, "a.png", &Default::default()).unwrap().bytes;
    assert!(!matches!(h.state_mut().open_hidden_owned("a.png", png), Ok(crate::Opening::Job(_))), "not a job");
}

/// A shown open's tracked outcome is the document once it is in place (the embed API settles
/// `openDocument` with it), after the opening tab gave way to it.
#[test]
fn tracked_opens_report_the_document_once_in_place() {
    let mut h = app_harness();
    h.state_mut().open_owned("page.psd", psd_bytes()).unwrap();
    let job = h.state().jobs.opens[0].job;
    h.state_mut().jobs.track_open(job);
    let (id, warnings) = outcome_of(&mut h, job).expect("opened");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(h.state().session.active().map(|st| st.doc.id), Some(id));
    assert!(h.state().jobs.opens.is_empty());
    // A cancelled one says so.
    h.state_mut().open_owned("page2.psd", psd_bytes()).unwrap();
    let job = h.state().jobs.opens[0].job;
    h.state_mut().jobs.track_open(job);
    crate::jobs_ui::cancel(h.state_mut(), job);
    let r = outcome_of(&mut h, job);
    assert!(r.is_err() || h.state().session.documents().iter().any(|st| Ok(st.doc.id) == r.as_ref().map(|(id, _)| *id).map_err(|_| ())), "{r:?}");
    // Untracked opens keep nothing.
    h.state_mut().open_owned("page3.psd", psd_bytes()).unwrap();
    h.run_steps(8);
    let t = Instant::now();
    while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    assert!(h.state_mut().jobs.take_open_outcomes().is_empty());
}

/// A document from the decoded-document cache opens at once, shown or hidden, sharing its
/// pixels with the cache and with every other copy (copy-on-write); `document.cache` reports,
/// removes and budgets the entries.
#[test]
fn cached_documents_reopen_sharing_their_pixels() {
    let mut h = app_harness();
    let ctx = h.ctx.clone();
    let st = h.state().session.active().unwrap();
    let (first, doc) = (st.doc.id, Arc::clone(&st.doc));
    h.state_mut().session.doc_cache.put("p13", "v1", Arc::clone(&doc), vec!["note".into()]);
    let (raw, warnings) = h.state_mut().session.doc_cache.get("p13", "v1").unwrap();
    assert_eq!(warnings, ["note"]);
    let i = h.state_mut().open_decoded(raw, true).unwrap();
    let raw_id = h.state().session.documents()[i].doc.id;
    assert_eq!(h.state().session.active().map(|st| st.doc.id), Some(first), "a hidden copy leaves the active document");
    let (page, _) = h.state_mut().session.doc_cache.get("p13", "v1").unwrap();
    let shown = h.state_mut().open_decoded(page, false).unwrap();
    assert_eq!(h.state().session.active_index(), Some(shown), "a shown copy is made active");
    let hidden = h.state().session.documents().iter().position(|st| st.doc.id == raw_id).unwrap();
    let tiles = |i: usize| -> Vec<usize> {
        let st = &h.state().session.documents()[i];
        let mut out = Vec::new();
        for l in st.doc.layers.iter() {
            if let photocraft_doc::LayerContent::Raster(s) = &l.content {
                out.extend(s.tiles().map(|(_, t)| Arc::as_ptr(t) as usize));
            }
        }
        out
    };
    let ids: Vec<_> = h.state().session.documents().iter().map(|st| st.doc.id).collect();
    assert!(ids[hidden] != first && ids[shown] != first && ids[hidden] != ids[shown], "fresh ids");
    assert!(!tiles(hidden).is_empty() && tiles(hidden) == tiles(shown), "the raw and the page share their tiles");
    assert!(h.state().session.documents()[hidden].hidden);
    let control = |h: &mut Harness<'static, PhotocraftApp>, params: Value| match crate::control::handle(
        h.state_mut(),
        &ctx,
        &crate::ControlRequest::new("document.cache", params).0,
    ) {
        crate::control::Outcome::Done(v) => v,
        _ => panic!("a reply"),
    };
    let info = control(&mut h, json!({}));
    assert_eq!(info["result"]["entries"][0]["key"], "p13", "{info}");
    assert!(info["result"]["usedMb"].as_f64().unwrap() > 0.0);
    let info = control(&mut h, json!({"remove": "p13", "budgetMb": 64}));
    assert_eq!((info["result"]["entries"].as_array().map(Vec::len), info["result"]["budgetMb"].as_f64()), (Some(0), Some(64.0)), "{info}");
    for bad in [json!({"remove": 3}), json!({"clear": "yes"}), json!({"budgetMb": -1}), json!({"nope": 1}), json!(4)] {
        assert_eq!(control(&mut h, bad.clone())["ok"], false, "{bad}");
    }
}

#[test]
fn templates_open_untitled_in_the_background() {
    let mut h = app_harness();
    h.state_mut().open_file("/tmp/photocraft-test/card.psdt", &psd_bytes()).unwrap();
    assert_eq!(h.state().jobs.opens[0].name, "Untitled-1");
    let t = Instant::now();
    while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(2);
    let st = h.state().session.active().unwrap();
    assert_eq!((st.doc.name.as_str(), st.path.as_deref()), ("Untitled-1", None));
    assert_eq!(h.state().ui.recent_files.first().map(String::as_str), Some("/tmp/photocraft-test/card.psdt"));
}

/// Gates for a fake save: it encodes until `encode` opens and writes until `write` opens
/// (polling for cancellation only while encoding, as the real one does).
#[derive(Clone, Default)]
struct Gates {
    encode: Arc<AtomicBool>,
    write: Arc<AtomicBool>,
    written: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Gates {
    fn open() -> Self {
        let g = Gates::default();
        g.encode.store(true, Ordering::Relaxed);
        g.write.store(true, Ordering::Relaxed);
        g
    }

    /// A `save_file` service held by these gates that records the path written, or fails with
    /// `fail` once encoded.
    fn save(&self, fail: Option<&'static str>) -> crate::SaveFileFn {
        let g = self.clone();
        Arc::new(move |_, path, _, ctl| {
            ctl.progress(0.0, "Encoding");
            while !g.encode.load(Ordering::Relaxed) {
                ctl.check()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            if let Some(e) = fail {
                return Err(e.into());
            }
            ctl.commit()?;
            ctl.progress(0.0, "Writing");
            while !g.write.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            g.written.lock().unwrap().push(path.to_string());
            Ok(vec!["a warning".into()])
        })
    }

    fn written(&self) -> Vec<String> {
        self.written.lock().unwrap().clone()
    }
}

fn noticed(app: &PhotocraftApp, text: &str) -> bool {
    app.ui.notices.iter().any(|n| n.title.contains(text) || n.lines.iter().any(|l| l.contains(text)))
}

fn step_until(h: &mut Harness<'_, PhotocraftApp>, done: impl Fn(&PhotocraftApp) -> bool) {
    let t = Instant::now();
    while !done(h.state()) && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(2);
}

#[test]
fn saves_run_in_the_background_and_are_recorded_when_they_end() {
    let mut h = app_harness();
    let gates = Gates { write: Arc::new(AtomicBool::new(true)), ..Gates::default() };
    h.state_mut().services.save_file = Some(gates.save(None));
    let r = h.state_mut().save_as(Some("/tmp/photocraft-test/big.psd".into())).unwrap();
    assert_eq!(r["pending"], true);
    assert_eq!(r["job"].as_u64(), h.state().jobs.saves.first().map(|s| s.job.0), "control clients get the job to wait on");
    assert!(h.state().saving());

    // The window keeps drawing; the document stays unsaved and can't be edited meanwhile.
    let (frames, _) = frames_for_at_least(&mut h, 400, 4);
    assert!(frames >= 4, "only {frames} frames while saving");
    let st = h.state().session.active().unwrap();
    assert!(st.is_dirty() && st.path.is_none());
    assert!(h.state_mut().run("layer.new.layer", json!({})).is_err(), "the saving document is locked");
    // Save in Background is on by default: no modal dialog, so the rest of the app stays usable
    // (#6207); the status bar reads "Saving 0%" until the encoder reports progress (#3389).
    assert!(h.query_by_label("Esc to cancel").is_none() && h.query_by_label("Encoding").is_none(), "no progress dialog");
    h.get_by_label("Saving 0%");
    assert!(crate::jobs_ui::foreground_save(h.state()).is_none());
    h.state_mut().ui.tool = crate::state::Tool::Move;
    h.key_press(egui::Key::B);
    h.run_steps(2);
    assert_eq!(h.state().ui.tool, crate::state::Tool::Brush, "shortcuts still work while it saves");

    gates.encode.store(true, Ordering::Relaxed);
    step_until(&mut h, |a| !a.saving());
    assert_eq!(gates.written(), ["/tmp/photocraft-test/big.psd"]);
    let st = h.state().session.active().unwrap();
    assert!(!st.is_dirty());
    assert_eq!(st.path.as_deref(), Some("/tmp/photocraft-test/big.psd"));
    assert_eq!(st.doc.name, "big.psd", "the document takes the saved file's name, as a synchronous Save As does");
    assert_eq!(h.state().ui.status, "Saved big.psd: a warning");
    assert!(noticed(h.state(), "a warning"), "the export warnings are shown");
    assert!(h.state_mut().run("layer.new.layer", json!({})).is_ok(), "unlocked again");
}

/// Save in Background off (#6207): the same worker writes the file, but the app waits for it
/// like Photoshop's foreground save. The progress dialog shows at once, shortcuts don't reach the
/// app, and a close asked for meanwhile happens only once the file is complete.
#[test]
fn save_in_background_off_blocks_the_app_until_the_file_is_written() {
    let mut h = app_harness();
    h.state_mut().run("prefs.set", json!({"path": "fileHandling.saveInBackground", "value": false})).unwrap();
    let gates = Gates { write: Arc::new(AtomicBool::new(true)), ..Gates::default() };
    h.state_mut().services.save_file = Some(gates.save(None));
    h.state_mut().ui.tool = crate::state::Tool::Move;
    let r = h.state_mut().save_as(Some("/tmp/photocraft-test/fg.psd".into())).unwrap();
    assert_eq!(r["pending"], true, "the same background job does the work");
    h.run_steps(1);
    // The dialog is up from the first frame (no delay), titled with the save, then its stage.
    // A new modal's accessibility tree can take a second pass to appear under kittest.
    let t = Instant::now();
    while h.query_by_label("Saving fg.psd").is_none() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.get_by_label("Saving fg.psd");
    h.get_by_label("Esc to cancel");
    while h.query_by_label("Encoding").is_none() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.get_by_label("Encoding");
    assert!(crate::jobs_ui::foreground_save(h.state()).is_some());
    h.key_press(egui::Key::B);
    h.run_steps(2);
    assert_eq!(h.state().ui.tool, crate::state::Tool::Move, "no shortcut reaches the app while it saves");
    assert!(h.state_mut().run("layer.new.layer", json!({})).is_err(), "the saving document is locked");
    // Quitting now waits for the file.
    let st = h.state_mut().session.active_mut().unwrap();
    st.saved_revision = st.revision;
    assert!(press_window_close(h.state_mut()), "the close waits for the save");
    assert!(h.state().jobs.close_after_saves);

    gates.encode.store(true, Ordering::Relaxed);
    let t = Instant::now();
    while h.state().jobs.close_after_saves && t.elapsed() < Duration::from_secs(30) {
        h.step();
        if h.state().jobs.close_after_saves {
            assert!(h.query_by_label("Esc to cancel").is_some(), "the dialog stays up until the save ends");
        }
    }
    assert_eq!(gates.written(), ["/tmp/photocraft-test/fg.psd"], "the file is complete before the close goes on");
    assert!(!h.state().jobs.close_after_saves, "then the held close is repeated");
    h.run_steps(2);
    assert!(h.query_by_label("Esc to cancel").is_none(), "the dialog is gone");
    let st = h.state().session.active().unwrap();
    assert_eq!(st.path.as_deref(), Some("/tmp/photocraft-test/fg.psd"));
    assert!(!st.is_dirty());
    h.key_press(egui::Key::B);
    h.run_steps(2);
    assert_eq!(h.state().ui.tool, crate::state::Tool::Brush, "shortcuts work again");
}

/// A failing foreground save reports its error, closes its dialog and leaves the document
/// unsaved, as a background one does; Esc still cancels one that is encoding.
#[test]
fn a_failed_or_cancelled_foreground_save_leaves_the_document_unsaved() {
    let mut h = app_harness();
    h.state_mut().run("prefs.set", json!({"path": "fileHandling.saveInBackground", "value": false})).unwrap();
    let gates = Gates::open();
    h.state_mut().services.save_file = Some(gates.save(Some("disk full")));
    h.state_mut().save_as(Some("/tmp/photocraft-test/f.psd".into())).unwrap();
    step_until(&mut h, |a| !a.saving());
    let st = h.state().session.active().unwrap();
    assert!(st.is_dirty() && st.path.is_none());
    assert!(noticed(h.state(), "disk full"), "the failure is reported");
    assert!(h.query_by_label("Esc to cancel").is_none(), "the dialog is gone");

    gates.encode.store(false, Ordering::Relaxed);
    h.state_mut().services.save_file = Some(gates.save(None));
    h.state_mut().save_as(Some("/tmp/photocraft-test/g.psd".into())).unwrap();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    step_until(&mut h, |a| !a.saving());
    assert!(gates.written().is_empty(), "nothing written");
    let st = h.state().session.active().unwrap();
    assert!(st.is_dirty() && st.path.is_none());
    assert_eq!(h.state().ui.status, "Cancelled Saving g.psd");
    assert!(crate::jobs_ui::foreground_save(h.state()).is_none());
}

#[test]
fn a_failed_save_or_one_cancelled_while_encoding_leaves_the_document_unsaved() {
    let mut h = app_harness();
    let gates = Gates::open();
    h.state_mut().services.save_file = Some(gates.save(Some("disk full")));
    h.state_mut().save_as(Some("/tmp/photocraft-test/a.psd".into())).unwrap();
    step_until(&mut h, |a| !a.saving());
    let st = h.state().session.active().unwrap();
    assert!(st.is_dirty() && st.path.is_none());
    assert!(noticed(h.state(), "disk full"), "the failure is reported");

    gates.encode.store(false, Ordering::Relaxed);
    h.state_mut().services.save_file = Some(gates.save(None));
    h.state_mut().save_as(Some("/tmp/photocraft-test/b.psd".into())).unwrap();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    step_until(&mut h, |a| !a.saving());
    assert!(gates.written().is_empty(), "nothing written");
    let st = h.state().session.active().unwrap();
    assert!(st.is_dirty() && st.path.is_none());
    assert_eq!(h.state().ui.status, "Cancelled Saving b.psd");
}

/// Step until the save in `h` has started writing.
fn until_writing(h: &mut Harness<'_, PhotocraftApp>) {
    let t = Instant::now();
    while h.state().session.jobs().first().is_none_or(|j| j.message != "Writing") && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
}

#[test]
fn a_save_that_has_started_writing_cannot_be_cancelled() {
    let mut h = app_harness();
    let gates = Gates { encode: Arc::new(AtomicBool::new(true)), ..Gates::default() };
    h.state_mut().services.save_file = Some(gates.save(None));
    h.state_mut().save_as(Some("/tmp/photocraft-test/w.psd".into())).unwrap();
    until_writing(&mut h);
    let job = h.state().jobs.saves[0].job;
    crate::jobs_ui::cancel(h.state_mut(), job);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().ui.status, "Saving w.psd is writing the file and can no longer be cancelled");
    assert!(h.state().session.job(job).is_some(), "still running, the document still locked");

    gates.write.store(true, Ordering::Relaxed);
    step_until(&mut h, |a| !a.saving());
    assert_eq!(gates.written(), ["/tmp/photocraft-test/w.psd"]);
    assert!(!h.state().session.active().unwrap().is_dirty(), "the written save is recorded");
}

/// #3389: like Photoshop, a running save reads "Saving 45%" in the status bar and appends
/// " - Saving 45%" to its document's tab title, in both tab strips; both go when it ends.
#[test]
fn a_running_save_shows_its_percentage_in_the_status_bar_and_tab_title() {
    for theme in crate::theme::ThemeKind::ALL {
        let mut h = app_harness();
        h.state_mut().run("prefs.set", json!({"values": {"interface.theme": theme.id()}})).unwrap();
        h.run_steps(2);
        let gate = Arc::new(AtomicBool::new(false));
        let g = gate.clone();
        let save: crate::SaveFileFn = Arc::new(move |_, _, _, ctl| {
            ctl.progress(0.4567, "Encoding");
            while !g.load(Ordering::Relaxed) {
                ctl.check()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            ctl.commit()?;
            ctl.progress(1.0, "Writing");
            Ok(Vec::new())
        });
        h.state_mut().services.save_file = Some(save);
        h.state_mut().save_as(Some("/tmp/photocraft-test/pct.psd".into())).unwrap();
        let t = Instant::now();
        while h.query_by_label("Saving 45%").is_none() && t.elapsed() < Duration::from_secs(30) {
            h.step();
        }
        assert!(h.query_by_label("Saving 45%").is_some(), "{theme:?}: the status bar reads Saving 45%");
        assert!(h.query_by_label_contains(" - Saving 45%").is_some(), "{theme:?}: the tab title carries the save's progress");
        gate.store(true, Ordering::Relaxed);
        step_until(&mut h, |a| !a.saving());
        assert!(h.query_by_label_contains("Saving").is_none(), "{theme:?}: the readout and the tab suffix are gone once saved");
        assert_eq!(h.state().session.active().unwrap().path.as_deref(), Some("/tmp/photocraft-test/pct.psd"));
    }
}

/// One frame with the window's close button pressed; whether the guard cancelled the close.
fn press_window_close(app: &mut PhotocraftApp) -> bool {
    let mut info = egui::ViewportInfo::default();
    info.events.push(egui::ViewportEvent::Close);
    let mut input = egui::RawInput::default();
    input.viewports.insert(egui::ViewportId::ROOT, info);
    let mut out = egui::Context::default().run_ui(input, |ui| crate::discard_ui::guard_window_close(app, ui.ctx()));
    out.textures_delta.clear();
    out.viewport_output.remove(&egui::ViewportId::ROOT).is_some_and(|o| o.commands.iter().any(|c| matches!(c, egui::ViewportCommand::CancelClose)))
}

/// A clean document (so the close prompt has nothing to ask) saved as a copy in the background.
fn save_a_copy(h: &mut Harness<'static, PhotocraftApp>, gates: &Gates, path: &str) {
    h.state_mut().services.save_file = Some(gates.save(None));
    let st = h.state_mut().session.active_mut().unwrap();
    st.saved_revision = st.revision;
    assert_eq!(h.state_mut().write_document(path.into(), &crate::ExportSettings::default(), true), Ok(None));
    h.run_steps(2);
}

/// The close prompt says only a flat copy exists: not when that copy never got written, and still
/// when an earlier one did.
#[test]
fn a_failed_background_save_records_no_flat_copy() {
    let mut h = app_harness();
    h.state_mut().run("layer.new.layer", json!({})).unwrap();
    let gates = Gates::default();
    h.state_mut().services.save_file = Some(gates.save(Some("disk full")));
    let doc = h.state().active_doc_id().unwrap();
    let write = |h: &mut Harness<'static, PhotocraftApp>| {
        assert_eq!(h.state_mut().write_flat_aware("/tmp/photocraft-test/flat.png".into(), &crate::ExportSettings::default(), true), Ok(None));
        h.run_steps(2);
        gates.encode.store(true, Ordering::Relaxed);
        step_until(h, |a| !a.saving());
    };
    write(&mut h);
    assert!(!h.state().only_saved_flat(doc));
    assert!(h.state().flat_copy_saves.is_empty());
    h.state_mut().flat_copy_docs.insert(doc);
    gates.encode.store(false, Ordering::Relaxed);
    write(&mut h);
    assert!(h.state().only_saved_flat(doc), "the earlier copy still exists");
}

#[test]
fn a_background_flat_save_counts_once_written() {
    let mut h = app_harness();
    h.state_mut().run("layer.new.layer", json!({})).unwrap();
    let gates = Gates { write: Arc::new(AtomicBool::new(true)), ..Gates::default() };
    h.state_mut().services.save_file = Some(gates.save(None));
    let doc = h.state().active_doc_id().unwrap();
    assert_eq!(h.state_mut().write_flat_aware("/tmp/photocraft-test/flat.png".into(), &crate::ExportSettings::default(), true), Ok(None));
    h.run_steps(2);
    assert!(!h.state().only_saved_flat(doc), "not before the file is written");
    gates.encode.store(true, Ordering::Relaxed);
    step_until(&mut h, |a| !a.saving());
    assert!(h.state().only_saved_flat(doc));
}

#[test]
fn quitting_waits_for_a_background_save_to_finish() {
    let mut h = app_harness();
    let gates = Gates { write: Arc::new(AtomicBool::new(true)), ..Gates::default() };
    save_a_copy(&mut h, &gates, "/tmp/photocraft-test/copy.psd");
    assert!(press_window_close(h.state_mut()), "the close waits for the save");
    assert!(h.state().jobs.close_after_saves);

    gates.encode.store(true, Ordering::Relaxed);
    step_until(&mut h, |a| !a.saving());
    assert_eq!(gates.written(), ["/tmp/photocraft-test/copy.psd"]);
    assert!(!h.state().jobs.close_after_saves, "the close is repeated once the save ends");
}

#[test]
fn a_failed_save_does_not_close_the_window_over_its_error() {
    let mut h = app_harness();
    let gates = Gates::default();
    h.state_mut().services.save_file = Some(gates.save(Some("disk full")));
    let st = h.state_mut().session.active_mut().unwrap();
    st.saved_revision = st.revision;
    h.state_mut().write_document("/tmp/photocraft-test/copy.psd".into(), &crate::ExportSettings::default(), true).unwrap();
    assert!(press_window_close(h.state_mut()));
    gates.encode.store(true, Ordering::Relaxed);
    step_until(&mut h, |a| !a.saving());
    assert!(noticed(h.state(), "disk full"));
    assert!(!h.state().jobs.close_after_saves, "the window stays open to show the error");
}

#[test]
fn a_cancelled_save_still_counts_until_its_worker_returns() {
    let mut h = app_harness();
    // A worker that keeps encoding after the cancel, as the real encoder does (it checks only
    // before writing).
    let (encode, release) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (e, r) = (encode.clone(), release.clone());
    h.state_mut().services.save_file = Some(Arc::new(move |_, _, _, ctl| {
        while !r.load(Ordering::Relaxed) {
            e.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        }
        ctl.commit()?;
        Ok(Vec::new())
    }));
    let st = h.state_mut().session.active_mut().unwrap();
    st.saved_revision = st.revision;
    h.state_mut().write_document("/tmp/photocraft-test/c.psd".into(), &crate::ExportSettings::default(), true).unwrap();
    while !encode.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(1));
    }
    let job = h.state().jobs.saves[0].job;
    crate::jobs_ui::cancel(h.state_mut(), job);
    h.run_steps(3);
    assert!(h.state().session.job(job).is_none(), "the job ended");
    assert!(h.state().saving(), "but its worker is still running");
    assert!(press_window_close(h.state_mut()), "quitting waits for the worker");
    assert!(
        h.state_mut().save_as(Some("/tmp/photocraft-test/c.psd".into())).unwrap_err().contains("still running"),
        "a second save to the same file waits for it too"
    );

    release.store(true, Ordering::Relaxed);
    step_until(&mut h, |a| !a.saving());
    assert!(!h.state().jobs.close_after_saves, "the held close is repeated once the worker returns");
}

/// Preferences ▸ General ▸ Beep When Done (off by default): a finished background job beeps
/// once through the services hook; with the preference off it stays silent.
#[test]
fn a_finished_job_beeps_when_the_preference_is_on() {
    static BEEPS: AtomicUsize = AtomicUsize::new(0);
    fn count_beep() {
        BEEPS.fetch_add(1, Ordering::Relaxed);
    }
    BEEPS.store(0, Ordering::Relaxed);
    let mut h = app_harness_with(crate::Services { beep: Some(count_beep), ..Default::default() });
    // Off by default: the first job finishes without a beep.
    let gate = Arc::new(AtomicBool::new(true));
    let _ = slow_job(h.state_mut(), &gate);
    frames_for(&mut h, 800);
    assert_eq!(BEEPS.load(Ordering::Relaxed), 0, "silent by default");
    // On: the next finished job beeps once.
    h.state_mut().run("prefs.set", json!({"path": "general.beepWhenDone", "value": true})).unwrap();
    let gate = Arc::new(AtomicBool::new(true));
    let _ = slow_job(h.state_mut(), &gate);
    frames_for(&mut h, 800);
    assert_eq!(BEEPS.load(Ordering::Relaxed), 1, "one beep for the finished job");
}

#[test]
fn printing_keeps_drawing_and_reports_only_the_final_spooler_result() {
    for success in [true, false] {
        let mut h = app_harness();
        let gate = Arc::new(AtomicBool::new(false));
        let worker_gate = gate.clone();
        let Started::Job(id) = h
            .state_mut()
            .session
            .start_job(
                "file.print",
                json!({}),
                "Print",
                false,
                move |_| {
                    while !worker_gate.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    if success {
                        Ok(json!({"sent": true, "spooler": "request id is Office-42", "pdf": null}))
                    } else {
                        Err(photocraft_engine::EngineError::Other("queue refused".into()))
                    }
                },
                |_, result| Ok(result),
            )
            .unwrap()
        else {
            panic!("must run on a worker")
        };
        let frame = h.state().frame;
        h.run_steps(4);
        assert!(h.state().frame > frame);
        assert!(!h.state().ui.status.contains("Sent to the printer"));
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().session.job(id).is_some(), "Esc cannot retract a submitted job");
        let (request, _) = crate::control::ControlRequest::new("jobs.cancel", json!({"job": id.0}));
        let ctx = h.ctx.clone();
        let crate::control::Outcome::Done(reply) = crate::control::handle(h.state_mut(), &ctx, &request) else { panic!("cancel replies immediately") };
        assert_eq!(reply["result"]["cancelled"], 0, "control must not falsely report cancellation: {reply}");
        let st = h.state_mut().session.active_mut().unwrap();
        st.saved_revision = st.revision;
        assert!(press_window_close(h.state_mut()), "quit waits for print submission");
        assert!(h.state().jobs.close_after_saves);
        gate.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(10);
        while h.state().session.has_jobs() && Instant::now() < deadline {
            h.step();
        }
        assert!(!h.state().session.has_jobs());
        assert!(!h.state().jobs.close_after_saves, "close repeats on success or is cleared for a visible error");
        if success {
            assert!(h.state().ui.status.contains("Office-42"), "{}", h.state().ui.status);
            assert!(!h.state().ui.status_error);
        } else {
            assert!(h.state().ui.status.contains("queue refused"), "{}", h.state().ui.status);
            assert!(h.state().ui.status_error);
        }
    }
    // The job and its status do not ask the system spooler for its printers.
    assert_eq!(photocraft_engine::print_cmds::system_printer_listings(), 0, "a test asked the system spooler for its printers");
}

fn layer_count(app: &PhotocraftApp) -> usize {
    app.session.active().unwrap().doc.layers.len()
}

/// Free Transform the active layer to the right half of the canvas, then ↩.
fn transform_and_commit(h: &mut Harness<'static, PhotocraftApp>, copy: bool) {
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    if copy { crate::transform_tool::begin_copy(app, &ctx) } else { crate::transform_tool::begin(app, &ctx) }.unwrap();
    app.ui.transform.as_mut().unwrap().quad = [[160.0, 0.0], [320.0, 0.0], [320.0, 120.0], [160.0, 120.0]];
    crate::transform_tool::commit(app);
}

#[test]
fn a_free_transform_commit_runs_in_the_background_and_keeps_its_preview() {
    let mut h = app_harness();
    let (layers, steps) = (layer_count(h.state()), h.state().session.active().unwrap().history.past_len());
    transform_and_commit(&mut h, true);
    let app = h.state();
    // The warp runs as a job; the moving pixels stay on screen until it lands, and the box's
    // history is closed meanwhile.
    let job = crate::transform_tool::committing(app).expect("the commit runs as a job");
    assert!(app.session.job(job).is_some_and(|j| j.label == "Free Transform"));
    assert!(app.ui.transform.is_some() && app.transform_preview.is_some());
    assert_eq!(crate::transform_tool::is_enabled(app, "edit.undo"), Some(false));
    assert!(h.state_mut().run("edit.undo", json!({})).is_err());
    crate::transform_tool::commit(h.state_mut());
    let t = Instant::now();
    while h.state().session.has_jobs() && t.elapsed() < Duration::from_secs(10) {
        h.step();
    }
    h.run_steps(2);
    let app = h.state();
    assert!(app.ui.transform.is_none() && app.transform_preview.is_none(), "the session ends with the job");
    // The copy and its transform are one step, as when the commit ran inline.
    let st = app.session.active().unwrap();
    assert_eq!((layer_count(app), st.history.past_len()), (layers + 1, steps + 1));
    assert_eq!(st.history.undo_label(), Some("Free Transform"));
    let moved = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
    assert_eq!((moved.x0, moved.y0, moved.x1, moved.y1), (160, 0, 320, 120));
}

#[test]
fn esc_cancels_a_background_free_transform_commit() {
    let mut h = app_harness();
    let (layers, steps) = (layer_count(h.state()), h.state().session.active().unwrap().history.past_len());
    let before = h.state().session.active().unwrap().doc.clone();
    transform_and_commit(&mut h, true);
    let job = crate::transform_tool::committing(h.state()).expect("the commit runs as a job");
    // Esc reaches the transform tool first (shortcuts), which cancels the job.
    crate::transform_tool::cancel(h.state_mut());
    h.run_steps(3);
    let app = h.state();
    assert!(app.session.job(job).is_none());
    assert!(app.ui.transform.is_none() && app.transform_preview.is_none());
    // The copy is taken back with the transform, leaving nothing to redo.
    let st = app.session.active().unwrap();
    assert_eq!((layer_count(app), st.history.past_len()), (layers, steps));
    assert!(!st.history.can_redo());
    assert_eq!(st.doc.layers.len(), before.layers.len());
}

/// Warp the active layer into an arc, then ↩.
fn warp_and_commit(h: &mut Harness<'static, PhotocraftApp>) {
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    crate::transform_tool::begin(app, &ctx).unwrap();
    let t = app.ui.transform.as_mut().unwrap();
    t.warp = Some(photocraft_geom::warp::Warp::preset(photocraft_geom::warp::WarpStyle::Arc, 40.0, t.rect));
    crate::transform_tool::commit(app);
}

#[test]
fn a_warp_commit_runs_in_the_background_and_keeps_its_preview() {
    let mut h = app_harness();
    let steps = h.state().session.active().unwrap().history.past_len();
    warp_and_commit(&mut h);
    let app = h.state();
    let job = crate::transform_tool::committing(app).expect("the commit runs as a job");
    assert!(app.session.job(job).is_some_and(|j| j.label == "Warp"));
    assert!(app.ui.transform.is_some() && app.transform_preview.is_some());
    assert_eq!(h.state_mut().run("edit.undo", json!({})).err().as_deref(), Some("Warp is still being applied"));
    let t = Instant::now();
    while h.state().session.has_jobs() && t.elapsed() < Duration::from_secs(10) {
        h.step();
    }
    h.run_steps(2);
    let app = h.state();
    assert!(app.ui.transform.is_none() && app.transform_preview.is_none(), "the session ends with the job");
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), steps + 1);
    assert_eq!(st.history.undo_label(), Some("Warp"));
    // The arc bulges the layer past its old edges.
    let b = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
    assert_ne!(b, photocraft_geom::Rect::new(0, 0, 320, 240), "the layer was warped");
}

#[test]
fn esc_cancels_a_background_warp_commit() {
    let mut h = app_harness();
    let steps = h.state().session.active().unwrap().history.past_len();
    let before = h.state().session.active().unwrap().doc.clone();
    warp_and_commit(&mut h);
    let job = crate::transform_tool::committing(h.state()).expect("the commit runs as a job");
    crate::transform_tool::cancel(h.state_mut());
    h.run_steps(3);
    let app = h.state();
    assert!(app.session.job(job).is_none());
    assert!(app.ui.transform.is_none() && app.transform_preview.is_none());
    let st = app.session.active().unwrap();
    assert_eq!(st.history.past_len(), steps);
    let id = st.active_layer.unwrap();
    let px = |d: &photocraft_doc::Document| d.layer(id).unwrap().surface().unwrap().read_region(photocraft_geom::Rect::new(0, 0, 320, 240));
    assert_eq!(px(&st.doc), px(&before), "the layer is untouched");
}
fn identity_app() -> PhotocraftApp {
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 6})).unwrap();
    session.execute("layer.new.layer", json!({})).unwrap();
    session.execute("edit.fill", json!({"color": "#336699"})).unwrap();
    session.active_mut().unwrap().path = Some("accepted-control.pcraft".into());
    assert!(session.active().unwrap().is_dirty());
    PhotocraftApp::new(session, crate::Services::default())
}

fn identity_png(profile: Option<photocraft_cms::Builtin>) -> Vec<u8> {
    let app = identity_app();
    let mut doc = (*app.session.active().unwrap().doc).clone();
    doc.icc_profile = profile.map(|profile| profile.profile().to_bytes());
    photocraft_io::export(&doc, "synthetic.png", &Default::default()).unwrap().bytes
}

/// Complete the real decoder/apply path without delivering its result to the UI yet. This is
/// the same boundary as a poll that applies all ready jobs before handling their UI events.
fn completed_identity_open(app: &mut PhotocraftApp, name: &str, bytes: Vec<u8>) -> Value {
    match app.session.start_open(name, photocraft_engine::jobs::OpenSource::Bytes(Arc::new(bytes))).unwrap() {
        Started::Done(result) => result,
        Started::Job(job) => app.session.wait_job(job).unwrap(),
    }
}

struct IdentityControl {
    doc: Arc<photocraft_doc::Document>,
    path: Option<String>,
    revision: u64,
    saved_revision: u64,
    history: usize,
    redo: bool,
}

impl IdentityControl {
    fn capture(app: &PhotocraftApp) -> Self {
        let state = app.session.active().unwrap();
        Self {
            doc: state.doc.clone(),
            path: state.path.clone(),
            revision: state.revision,
            saved_revision: state.saved_revision,
            history: state.history.past_len(),
            redo: state.history.can_redo(),
        }
    }

    fn assert_untouched(&self, app: &PhotocraftApp) {
        let state = app.session.documents().iter().find(|state| state.doc.id == self.doc.id).expect("the accepted control remains open");
        assert!(Arc::ptr_eq(&state.doc, &self.doc), "the accepted control's pixels and document data must not change");
        assert_eq!(state.path, self.path, "an import must not rebind another document's save path");
        assert_eq!(
            (state.revision, state.saved_revision, state.history.past_len(), state.history.can_redo()),
            (self.revision, self.saved_revision, self.history, self.redo)
        );
        assert!(state.is_dirty(), "the unrelated document must still need saving");
    }
}

/// Exercise the consumer separately from the engine result-schema regression: the identity is
/// observed from the real imported document, then supplied even on the old producer.
fn identified_result(app: &PhotocraftApp, mut result: Value) -> (Value, photocraft_doc::DocId) {
    let id = app.session.documents()[result["document"].as_u64().unwrap() as usize].doc.id;
    result["docId"] = json!(id.0);
    (result, id)
}

#[test]
fn open_identity_survives_tab_reordering() {
    let mut app = identity_app();
    let control = IdentityControl::capture(&app);
    let result = completed_identity_open(&mut app, "imported.png", identity_png(None));
    let (result, imported) = identified_result(&app, result);
    let old_index = result["document"].as_u64().unwrap() as usize;
    app.session.move_document(old_index, 0).unwrap();
    assert_eq!(app.session.documents()[old_index].doc.id, control.doc.id);
    super::finish_open(&mut app, "imported.png", Some("imported.png"), &result, None).unwrap();
    control.assert_untouched(&app);
    assert_eq!(app.session.active().unwrap().doc.id, imported);
    assert_eq!(app.session.active().unwrap().path.as_deref(), Some("imported.png"));
}

#[test]
fn open_identity_survives_an_earlier_tab_closing() {
    let mut app = identity_app();
    let control = IdentityControl::capture(&app);
    app.session.execute("file.new", json!({"width": 2, "height": 2})).unwrap();
    let result = completed_identity_open(&mut app, "imported.png", identity_png(None));
    let (result, imported) = identified_result(&app, result);
    app.session.close(1).unwrap();
    app.session.set_active(0);
    super::finish_open(&mut app, "imported.png", Some("imported.png"), &result, Some(0)).unwrap();
    control.assert_untouched(&app);
    assert_eq!(app.session.active().unwrap().doc.id, imported);
    assert_eq!(app.session.documents()[0].doc.id, imported, "the requested destination slot applies to the correct document");
}

#[test]
fn open_identity_rejects_closed_missing_and_invalid_identities() {
    for identity in [None, Some(Value::Null), Some(json!("not-an-id")), Some(json!(-1)), Some(json!(u64::MAX)), Some(json!(1.5))] {
        let mut app = identity_app();
        let control = IdentityControl::capture(&app);
        let result = completed_identity_open(&mut app, "closed.png", identity_png(None));
        let (mut result, imported) = identified_result(&app, result);
        app.session.close(result["document"].as_u64().unwrap() as usize).unwrap();
        // A reused/stale index must never become permission to finish another document.
        result["document"] = json!(0);
        match identity {
            None => {
                result.as_object_mut().unwrap().remove("docId");
            }
            Some(value) => result["docId"] = value,
        }
        assert!(super::finish_open(&mut app, "closed.png", Some("closed.png"), &result, Some(0)).is_err(), "invalid identity was accepted: {result}");
        control.assert_untouched(&app);
        assert_eq!(app.session.active().unwrap().doc.id, control.doc.id);
        assert_eq!(app.session.documents().len(), 1);
        result["docId"] = json!(imported.0);
        assert!(
            super::finish_open(&mut app, "closed.png", Some("closed.png"), &result, None).is_err(),
            "a closed identity must not fall back to the old tab index"
        );
        control.assert_untouched(&app);
    }
}

#[test]
fn open_identity_keeps_same_poll_results_in_the_profile_queue() {
    let mut app = identity_app();
    let control = IdentityControl::capture(&app);
    app.session.execute("edit.colorSettings", json!({"askOnMismatch": true, "askOnMissing": false, "policyRgb": "preserve"})).unwrap();
    let mismatched = identity_png(photocraft_cms::Builtin::from_id("adobe-rgb-compat"));
    let prompt = completed_identity_open(&mut app, "profile.png", mismatched);
    assert!(prompt["held"].as_u64().is_some());
    let first = completed_identity_open(&mut app, "first.png", identity_png(None));
    let (first, first_id) = identified_result(&app, first);
    let second = completed_identity_open(&mut app, "second.png", identity_png(None));
    let (second, second_id) = identified_result(&app, second);
    // All engine applies have finished. Handling the first ordinary result removes its tab,
    // shifting the second result before its UI callback runs.
    super::finish_open(&mut app, "profile.png", Some("profile.png"), &prompt, None).unwrap();
    super::finish_open(&mut app, "first.png", Some("first.png"), &first, None).unwrap();
    super::finish_open(&mut app, "second.png", Some("second.png"), &second, None).unwrap();
    assert_eq!(app.held_opens.iter().map(|held| held.name.as_str()).collect::<Vec<_>>(), ["profile.png", "first.png", "second.png"]);
    assert_eq!(app.held_opens.iter().skip(1).map(|held| held.doc.id).collect::<Vec<_>>(), [first_id, second_id]);
    assert_eq!(app.session.documents().len(), 1, "both completed imports wait behind the profile prompt");
    control.assert_untouched(&app);
    assert_eq!(crate::open_prompt::answer(&mut app, None).unwrap()["cancelled"], 3);
    assert!(app.held_opens.is_empty());
    control.assert_untouched(&app);
}

/// Opening a PSD (text, mask, group, hidden layer) and drawing frames leaves it unmodified, so
/// quitting (and Restart and Update) asks nothing.
#[test]
fn an_opened_psd_is_not_modified_and_quits_without_asking() {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 200, "height": 150})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("edit.fill", json!({"color": "#3366cc"})).unwrap();
    s.execute("layer.mask.revealAll", json!({})).ok();
    s.execute("type.create", json!({"text": "Hola", "size": 24, "x": 10, "y": 40})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("layer.group", json!({})).ok();
    let doc = (*s.active().unwrap().doc).clone();
    let bytes = photocraft_io::export(&doc, "x.psd", &Default::default()).unwrap().bytes;

    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(8).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.background_jobs = true;
        app
    });
    h.run_steps(2);
    h.state_mut().open_file("/tmp/photocraft-test/clean.psd", &bytes).unwrap();
    let t = Instant::now();
    while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(8);
    assert_eq!(h.state().session.documents().len(), 1, "opened");
    for st in h.state().session.documents() {
        assert!(!st.is_dirty(), "{} is modified after opening: revision {} saved {}", st.doc.name, st.revision, st.saved_revision);
    }
    assert!(!h.state().has_unsaved_work());
    // View, selection and layer targeting are not edits either.
    let ctx = h.ctx.clone();
    let mut dirtied = Vec::new();
    for id in [
        "view.zoomIn", "view.zoomOut", "view.fitOnScreen", "view.actualPixels", "view.rulers", "view.extras", "view.grid",
        "select.all", "select.inverse", "select.deselect", "select.reselect", "select.deselect", "select.allLayers",
        "layer.selectBackward", "layer.selectForward", "window.history", "window.layers", "window.channels",
    ] {
        let _ = crate::menus::invoke(h.state_mut(), &ctx, id, json!({}));
        h.run_steps(2);
        if h.state().session.active().is_some_and(|st| st.is_dirty()) {
            dirtied.push(id);
            let st = h.state_mut().session.active_mut().unwrap();
            st.saved_revision = st.revision;
        }
    }
    assert!(dirtied.is_empty(), "{dirtied:?} marked the opened file modified");
    // File › Exit (what Restart and Update runs) quits without the unsaved-changes prompt.
    crate::menus::invoke(h.state_mut(), &ctx, "file.exit", json!({})).unwrap();
    assert!(h.state().discard.is_none(), "no unsaved-changes prompt");
    assert!(h.state().allow_close);
}

/// [`an_opened_psd_is_not_modified_and_quits_without_asking`] over real files: the paths in
/// `PHOTOCRAFT_CLEAN_OPEN_PSDS` (separated by `;`).
#[test]
#[ignore = "needs PHOTOCRAFT_CLEAN_OPEN_PSDS"]
fn real_psds_open_unmodified() {
    let list = std::env::var("PHOTOCRAFT_CLEAN_OPEN_PSDS").unwrap_or_default();
    let mut bad = Vec::new();
    for path in list.split(';').filter(|p| !p.is_empty()) {
        let bytes = std::fs::read(path).unwrap();
        let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(8).build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            app.background_jobs = true;
            app
        });
        h.run_steps(2);
        h.state_mut().open_file(path, &bytes).unwrap();
        let t = Instant::now();
        while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(120) {
            h.step();
        }
        h.run_steps(8);
        assert_eq!(h.state().session.documents().len(), 1, "{path} opened");
        for st in h.state().session.documents() {
            if st.is_dirty() {
                bad.push(format!("{path}: revision {} saved {} history {}", st.revision, st.saved_revision, st.history.past_len()));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
