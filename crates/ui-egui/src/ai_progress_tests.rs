use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use egui::vec2;
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_ai_lama::service::{Done, Hooks, Inpainter, ModelState, Request};
use photocraft_engine::jobs::{JobCtx, JobId, Started};
use serde_json::json;

use super::*;

#[test]
fn stages_are_read_from_the_progress_messages() {
    assert_eq!(stage("Downloading Manga LaMa (100 MB): 45% at 12.3 MB/s", 0.05), Stage::Download { fraction: Some(0.45), speed: Some(12.3e6) });
    assert_eq!(stage("Downloading Manga LaMa (100 MB)", 0.0), Stage::Download { fraction: None, speed: None });
    assert_eq!(stage("Loading Manga LaMa", 0.0), Stage::Preparing);
    assert_eq!(stage("Preparing the model", 0.12), Stage::Preparing);
    assert_eq!(stage("Preparing the model (2 of 3)", 0.4), Stage::Preparing, "a tile's model load is still preparing");
    assert_eq!(stage("Compiling GPU shaders (first run) (1 of 3)", 0.2), Stage::Compiling);
    assert_eq!(stage("Cleaning with Manga LaMa (WebGPU) (3 of 8)", 0.4), Stage::Processing { step: Some((3, 8)) });
    assert_eq!(stage("Remove with AI (2 of 5)", 0.3), Stage::Processing { step: Some((2, 5)) });
    assert_eq!(stage("Generating (WebGPU) (step 4 of 20)", 0.3), Stage::Processing { step: Some((4, 20)) });
    assert_eq!(stage("Upscaling with 2x Manga (CPU)", 0.5), Stage::Processing { step: None });
    assert_eq!(stage("Remove with AI", 0.0), Stage::Preparing, "nothing reported yet");
    assert_eq!(stage("Remove with AI", 0.999), Stage::Applying);
    assert_eq!(stage(photocraft_engine::ai_cmds::SWITCHING_TO_CPU, 0.5), Stage::Preparing);
    assert_eq!(stage("Applying", 1.0), Stage::Applying);
    // In words.
    crate::i18n::set_current(crate::i18n::Lang::EN);
    assert_eq!(stage_text(&Stage::Download { fraction: Some(0.456), speed: Some(12.3e6) }), "Downloading model 45% · 12.3 MB/s");
    assert_eq!(stage_text(&Stage::Processing { step: Some((3, 8)) }), "Processing 3/8");
}

/// An inpainter that holds every request until the test answers it.
#[derive(Default)]
struct Held {
    pending: Mutex<Option<(Arc<dyn Hooks>, Done)>>,
}

impl Inpainter for Held {
    fn model_state(&self) -> ModelState {
        ModelState::Ready
    }
    fn device(&self) -> String {
        "test".into()
    }
    fn run(&self, _req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        *self.pending.lock().unwrap() = Some((hooks, done));
    }
}

impl Held {
    /// Waits (a little) for the job's worker to send its request.
    fn arrived(&self) {
        for _ in 0..400 {
            if self.waiting() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("no request arrived");
    }
    fn progress(&self, f: f32, s: &str) {
        self.arrived();
        let g = self.pending.lock().unwrap();
        g.as_ref().expect("a request").0.progress(f, s);
    }
    fn fail(&self, why: &str) {
        self.arrived();
        let (_, done) = self.pending.lock().unwrap().take().expect("a request");
        done(Err(photocraft_ai_lama::LamaError::Device(why.into())));
    }
    fn waiting(&self) -> bool {
        self.pending.lock().unwrap().is_some()
    }
}

static VIBRATED: AtomicU32 = AtomicU32::new(0);

/// A phone (the simple interface, in Spanish) with a page, background jobs and `held` as the
/// inpainter.
fn phone(held: Arc<Held>) -> Harness<'static, PhotocraftApp> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 600, "height": 800, "background": "white"})).unwrap();
    s.set_inpainter(Some(held));
    let services = crate::Services { vibrate: Some(|ms| VIBRATED.store(ms, Ordering::SeqCst)), ..Default::default() };
    let mut app = PhotocraftApp::new(s, services);
    app.run("prefs.set", json!({"values": {"interface.language": "es"}})).unwrap();
    app.background_jobs = true;
    app.sync_views();
    let mut h = Harness::builder().with_size(vec2(412.0, 915.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    h
}

/// Remove with AI through its command: the card shows its stages, the area pulses, the Capi
/// button pulses, the modal progress dialog stays away, and Cancel stops it.
#[test]
fn remove_with_ai_shows_its_stages_and_cancel_stops_it() {
    let held = Arc::new(Held::default());
    let mut h = phone(held.clone());
    crate::remove_ai_ui::request(h.state_mut(), json!({"strokes": [[[100, 100, 1], [180, 120, 1]]], "size": 20}));
    h.run_steps(3);
    held.arrived();
    assert!(busy(h.state()), "the Capi button pulses");
    h.get_by_label("Quitar con IA");
    h.get_by_label("Preparando IA…");
    held.progress(0.0, "Compiling GPU shaders (first run)");
    h.run_steps(2);
    h.get_by_label("Preparando la IA para esta GPU (solo la primera vez)…");
    held.progress(0.5, "Cleaning with Manga LaMa (test)");
    h.run_steps(2);
    h.get_by_label("Procesando…");
    assert!(h.query_by_label("Esc para cancelar").is_none(), "no modal progress dialog");
    // The pulse covers the strokes on screen.
    let (_, r) = h.state().ai_progress.tracked.as_ref().and_then(|t| t.region).expect("a region");
    assert!(r.x0 <= 90 && r.y0 <= 90 && r.x1 >= 190 && r.y1 >= 130, "{r:?}");
    // Cancel.
    h.get_by_label("Cancelar").click();
    h.run_steps(3);
    assert!(h.state().session.jobs().is_empty(), "cancelled");
    assert!(h.query_by_label("Procesando…").is_none(), "the card is gone");
    assert!(!busy(h.state()));
}

/// A failed run keeps its error on the card with Retry, which runs it again.
#[test]
fn a_failure_says_why_and_retry_runs_it_again() {
    let held = Arc::new(Held::default());
    let mut h = phone(held.clone());
    crate::remove_ai_ui::request(h.state_mut(), json!({"strokes": [[[100, 100, 1], [180, 120, 1]]], "size": 20}));
    h.run_steps(3);
    held.fail("out of memory");
    h.run_steps(3);
    h.get_by_label_contains("out of memory");
    assert!(h.state().ui.notices.is_empty(), "said once, on the card");
    h.get_by_label("Reintentar").click();
    h.run_steps(3);
    held.arrived();
    h.get_by_label("Quitar con IA");
}

/// A job of each AI command shows the card with the tool's name; a finished one says "Done"
/// and taps the vibration.
#[test]
fn every_ai_command_gets_the_card_and_done_taps_the_phone() {
    let held = Arc::new(Held::default());
    let mut h = phone(held);
    for (id, title) in [
        (photocraft_engine::ai_inpaint_cmds::ID, "Limpiar con IA"),
        (photocraft_engine::ai_cmds::COMMAND, "Escalar con IA"),
        (photocraft_engine::genfill_cmds::ID, "Relleno generativo"),
        (photocraft_engine::remove_ai_cmds::ID, "Quitar con IA"),
    ] {
        let (tx, rx) = mpsc::channel::<()>();
        let (ctx_tx, ctx_rx) = mpsc::channel::<JobCtx>();
        let rx = Mutex::new(rx);
        let started = h
            .state_mut()
            .session
            .start_job(
                id,
                json!({}),
                title,
                false,
                move |ctx| {
                    ctx_tx.send(ctx.clone()).unwrap();
                    rx.lock().unwrap().recv().ok();
                    Ok(())
                },
                |_, ()| Ok(json!({})),
            )
            .unwrap();
        let Started::Job(job) = started else { panic!("a job") };
        note_start(h.state_mut(), id, &json!({}));
        started_job(h.state_mut(), id, job);
        let ctx = ctx_rx.recv().unwrap();
        ctx.progress(0.3, "Generating (test) (step 3 of 8)");
        h.run_steps(2);
        h.get_by_label(title);
        h.get_by_label("Procesando 3/8");
        VIBRATED.store(0, Ordering::SeqCst);
        tx.send(()).unwrap();
        for _ in 0..50 {
            h.run_steps(1);
            if h.state().session.jobs().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        h.run_steps(2);
        h.get_by_label(&format!("{title}: listo"));
        assert_eq!(VIBRATED.load(Ordering::SeqCst), 40, "{id}: the phone is tapped");
    }
}

fn started_job(app: &mut PhotocraftApp, id: &str, job: JobId) {
    started(app, id, &json!({}), job);
}

/// The GPU gave way to the CPU: the card says it may take longer, until the end.
#[test]
fn the_cpu_fallback_is_said() {
    let held = Arc::new(Held::default());
    let mut h = phone(held.clone());
    crate::remove_ai_ui::request(h.state_mut(), json!({"strokes": [[[100, 100, 1], [180, 120, 1]]], "size": 20}));
    h.run_steps(3);
    held.progress(0.2, photocraft_engine::ai_cmds::SWITCHING_TO_CPU);
    h.run_steps(2);
    held.progress(0.6, "Cleaning with Manga LaMa (CPU)");
    h.run_steps(2);
    h.get_by_label("Usando la CPU, puede tardar más");
}
