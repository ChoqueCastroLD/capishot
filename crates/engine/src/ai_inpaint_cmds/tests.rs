use std::sync::Arc;

use photocraft_ai_lama::service::{FakeInpainter, ModelState};
use photocraft_geom::Rect;
use serde_json::json;

use super::*;

/// White page, black stripes, a red "text" blob at (28..36, 18..28), selected (26..38, 16..30).
fn page(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": depth, "background": "white"})).unwrap();
    s.edit("stripes", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        for x in (0..64).step_by(8) {
            surf.fill_rect(Rect::new(x, 0, x + 3, 48), &[0.0, 0.0, 0.0, 1.0]);
        }
        surf.fill_rect(Rect::new(28, 18, 36, 28), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
    s
}

fn with_fake(mut s: Session, fake: FakeInpainter) -> Session {
    s.set_inpainter(Some(Arc::new(fake)));
    s
}

fn layer_px(s: &Session, id: LayerId, x: i32, y: i32) -> Vec<f32> {
    s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().pixel(x, y)
}

fn active_id(s: &Session) -> LayerId {
    s.active().unwrap().active_layer.unwrap()
}

fn is_red(v: &[f32]) -> bool {
    v[0] > 0.9 && v[1] < 0.1 && v[2] < 0.1
}

#[test]
fn unavailable_without_a_service_and_nothing_changes() {
    let mut s = page(8);
    assert!(!s.is_enabled(ID), "greyed out where no service runs the model");
    let before = s.active().unwrap().doc.clone();
    let past = s.active().unwrap().history.past_len();
    let err = s.execute(ID, json!({})).unwrap_err().to_string();
    assert!(err.contains(UNAVAILABLE), "{err}");
    let st = s.active().unwrap();
    assert!(Arc::ptr_eq(&before, &st.doc));
    assert_eq!(st.history.past_len(), past);
}

#[test]
fn disabled_without_a_selection() {
    let mut s = with_fake(page(8), FakeInpainter::default());
    s.execute("select.deselect", json!({})).unwrap();
    assert!(!s.is_enabled(ID));
    assert!(s.execute(ID, json!({})).is_err());
}

#[test]
fn new_layer_holds_only_the_selection_at_all_depths() {
    for depth in [8, 16, 32] {
        let mut s = with_fake(page(depth), FakeInpainter::default());
        let base = active_id(&s);
        let past = s.active().unwrap().history.past_len();
        let r = s.execute(ID, json!({})).unwrap();
        assert_eq!(r["pixels"].as_u64(), Some(12 * 14), "depth {depth}: {r}");
        assert_eq!(s.active().unwrap().history.past_len(), past + 1, "one undo step");
        let new = active_id(&s);
        assert_ne!(new, base, "a new layer, made active");
        assert_eq!(r["layer"].as_u64(), Some(new.0));
        let inside = layer_px(&s, new, 30, 20);
        assert!((inside[3] - 1.0).abs() < 1e-3 && !is_red(&inside), "depth {depth}: {inside:?}");
        assert!(layer_px(&s, new, 5, 5)[3] < 1e-6, "depth {depth}: outside the selection stays empty");
        assert!(is_red(&layer_px(&s, base, 30, 20)), "the original layer is untouched");
        s.undo();
        assert_eq!(active_id(&s), base);
        assert!(s.active().unwrap().doc.layer(new).is_none());
    }
}

#[test]
fn current_layer_output_replaces_only_selected_pixels() {
    for depth in [8, 16, 32] {
        let mut s = with_fake(page(depth), FakeInpainter::default());
        let id = active_id(&s);
        let outside = layer_px(&s, id, 1, 1);
        s.execute(ID, json!({"output": "current"})).unwrap();
        assert_eq!(active_id(&s), id);
        for (x, y) in [(30, 20), (33, 25)] {
            assert!(!is_red(&layer_px(&s, id, x, y)), "depth {depth}: red left at ({x},{y})");
        }
        assert_eq!(layer_px(&s, id, 1, 1), outside);
        assert_eq!(layer_px(&s, id, 40, 20), vec![0.0, 0.0, 0.0, 1.0][..layer_px(&s, id, 40, 20).len()].to_vec());
        s.undo();
        assert!(is_red(&layer_px(&s, id, 30, 20)), "undo restores the blob");
    }
}

#[test]
fn runs_as_a_background_job_like_inline() {
    let mut inline = with_fake(page(8), FakeInpainter::default());
    inline.execute(ID, json!({"output": "current"})).unwrap();
    let mut bg = with_fake(page(8), FakeInpainter::default());
    let id = match bg.start(ID, json!({"output": "current"})).unwrap() {
        crate::jobs::Started::Job(id) => id,
        crate::jobs::Started::Done(v) => panic!("ran inline: {v}"),
    };
    let t = std::time::Instant::now();
    let ev = loop {
        if let Some(e) = bg.poll_jobs().into_iter().find(|e| e.id == id) {
            break e;
        }
        assert!(t.elapsed().as_secs() < 30, "job did not finish");
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    assert!(matches!(ev.outcome, crate::jobs::JobOutcome::Done(_)), "{:?}", ev.outcome);
    let (a, b) = (active_id(&inline), active_id(&bg));
    assert_eq!(layer_px(&inline, a, 30, 20), layer_px(&bg, b, 30, 20));
}

#[test]
fn bad_params_are_errors() {
    let mut s = with_fake(page(8), FakeInpainter::default());
    let past = s.active().unwrap().history.past_len();
    for p in [
        json!({"output": "elsewhere"}),
        json!({"output": 3}),
        json!({"maxSide": 100}),
        json!({"maxSide": 32}),
        json!({"maxSide": 4096}),
        json!({"maxSide": -8}),
        json!({"margin": 5000}),
        json!({"margin": "wide"}),
        json!({"allowDownload": "yes"}),
        json!({"layer": "x"}),
        json!({"layer": 999_999}),
        json!([1, 2]),
    ] {
        assert!(s.execute(ID, p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.active().unwrap().history.past_len(), past);
}

#[test]
fn model_states_and_failures_are_reported() {
    let needs = || FakeInpainter { state: Some(ModelState::NeedsDownload { bytes: 101_904_726 }), ..Default::default() };
    let mut s = with_fake(page(8), needs());
    let err = s.execute(ID, json!({})).unwrap_err().to_string();
    assert!(err.contains("isn't downloaded") && err.contains("101.9 MB") && err.contains("allowDownload"), "{err}");
    // Still checking (the browser's cache): it runs, and a missing model fails the run instead.
    let mut s = with_fake(page(8), FakeInpainter { state: Some(ModelState::Checking), ..Default::default() });
    s.execute(ID, json!({})).unwrap();
    let mut s = with_fake(page(8), needs());
    s.execute(ID, json!({"allowDownload": true})).unwrap();

    let mut s = with_fake(page(8), FakeInpainter { state: Some(ModelState::Unavailable("no GPU and no CPU backend".into())), ..Default::default() });
    assert!(s.execute(ID, json!({})).unwrap_err().to_string().contains("no GPU"));

    let mut s = with_fake(page(8), FakeInpainter { fail: Some("device lost".into()), ..Default::default() });
    let past = s.active().unwrap().history.past_len();
    let err = s.execute(ID, json!({})).unwrap_err().to_string();
    assert!(err.contains("device lost"), "{err}");
    assert_eq!(s.active().unwrap().history.past_len(), past);
}

#[test]
fn locked_layer_is_refused() {
    let mut s = with_fake(page(8), FakeInpainter::default());
    s.execute("layer.lockLayers", json!({"pixels": true})).unwrap();
    assert!(!s.is_enabled(ID));
    assert!(s.execute(ID, json!({"output": "current"})).is_err());
}

#[test]
fn remove_tool_ai_mode_uses_the_service() {
    let stroke = json!({"points": [[28, 22], [36, 22]], "size": 10, "ai": true});
    let mut s = page(8);
    s.execute("select.deselect", json!({})).unwrap();
    let err = s.execute("paint.remove", stroke.clone()).unwrap_err().to_string();
    assert!(err.contains(UNAVAILABLE), "{err}");
    let mut s = with_fake(s, FakeInpainter::default());
    let id = active_id(&s);
    s.execute("paint.remove", stroke).unwrap();
    assert!(!is_red(&layer_px(&s, id, 32, 22)));
    assert!(s.execute("paint.remove", json!({"points": [[28, 22]], "size": 10, "ai": true, "maxSide": 7})).is_err());
}

/// An inpainter that answers later, from another thread (as the browser's AI worker answers
/// from a later event): `done` is called after `run` returned.
struct LaterInpainter(FakeInpainter);

impl Inpainter for LaterInpainter {
    fn model_state(&self) -> ModelState {
        ModelState::Ready
    }
    fn model_state_of(&self, _: &str) -> ModelState {
        ModelState::Ready
    }
    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: photocraft_ai_lama::service::Done) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.0.run(req, hooks, Box::new(move |r| drop(tx.send(r))));
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if let Ok(r) = rx.recv() {
                done(r);
            }
        });
    }
}

/// The Remove tool's AI mode never waits for the inpainter on the calling thread (the browser
/// can't: the threads build panicked in `recv_timeout`, "time not implemented on this
/// platform"): with an inpainter that answers later the stroke runs as a job and applies.
#[test]
fn remove_tool_ai_mode_works_with_an_inpainter_that_answers_later() {
    let mut s = page(8);
    s.execute("select.deselect", json!({})).unwrap();
    s.set_inpainter(Some(Arc::new(LaterInpainter(FakeInpainter::default()))));
    let id = active_id(&s);
    let past = s.active().unwrap().history.past_len();
    let started = s.start("paint.remove", json!({"points": [[28, 22], [36, 22]], "size": 10, "ai": true})).unwrap();
    let crate::jobs::Started::Job(job) = started else { panic!("expected a background job") };
    let t = std::time::Instant::now();
    let event = loop {
        if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
            break e;
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(30), "the job never finished");
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    assert!(matches!(event.outcome, crate::jobs::JobOutcome::Done(_)), "{event:?}");
    assert!(!is_red(&layer_px(&s, id, 32, 22)));
    assert_eq!(s.active().unwrap().history.past_len(), past + 1, "one undo step");
}

/// Quality / Device pick Manga LaMa or the lite MI-GAN from the shell's device profile, and the
/// result says which ran.
#[test]
fn quality_and_device_choose_lama_or_migan() {
    use photocraft_ai::profile::{DeviceProfile, GpuClass};
    use photocraft_ai::service::FakeUpscaler;
    let run = |profile: DeviceProfile, params: Value| {
        let fake = Arc::new(FakeInpainter::default());
        let mut s = page(8);
        s.set_inpainter(Some(fake.clone()));
        let up = FakeUpscaler { profile: std::sync::Mutex::new(profile), ..Default::default() };
        s.set_ai_upscaler(Some(Arc::new(up)));
        let r = s.execute(ID, params).unwrap();
        let last = *fake.last.lock().unwrap();
        (r, last.unwrap())
    };
    let gpu = DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(8.0), cores: Some(8), ..Default::default() };
    let none = DeviceProfile { gpu: GpuClass::None, cores: Some(8), ..Default::default() };
    let (r, last) = run(gpu.clone(), json!({}));
    assert_eq!((r["model"].as_str(), r["lite"].as_bool(), last), (Some("manga-lama"), Some(false), ("manga-lama", false)));
    let (r, last) = run(none.clone(), json!({}));
    assert_eq!((r["model"].as_str(), last), (Some("migan-512"), ("migan-512", true)), "no GPU: lite on the CPU");
    let (_, last) = run(none, json!({"quality": "best"}));
    assert_eq!(last, ("manga-lama", true), "Best on the CPU when the user asks");
    let (_, last) = run(gpu.clone(), json!({"quality": "fast"}));
    assert_eq!(last, ("migan-512", false));
    let (_, last) = run(gpu.clone(), json!({"device": "cpu"}));
    assert_eq!(last, ("migan-512", true));
    let (_, last) = run(gpu, json!({"model": "manga-lama", "device": "cpu"}));
    assert_eq!(last, ("manga-lama", true));
    let mut s = with_fake(page(8), FakeInpainter::default());
    for bad in
        [json!({"quality": "max"}), json!({"device": "npu"}), json!({"model": "mangajanai-v1-2x-1200p"}), json!({"threads": 99}), json!({"keepLoaded": 1})]
    {
        assert!(s.execute(ID, bad.clone()).is_err(), "{bad}");
    }
}

/// The GPU couldn't run the model (the live report: burn's matmul over WebGPU's workgroup
/// memory): the job runs on the CPU with the model the Quality picks there, says so in
/// `notices`, and the document gets the result.
#[test]
fn a_gpu_failure_runs_again_on_the_cpu() {
    use photocraft_ai::profile::{DeviceProfile, GpuClass};
    use photocraft_ai::service::FakeUpscaler;
    let run = |params: Value| {
        let fake = Arc::new(FakeInpainter { gpu_fail: Some("Shared memory budget exceeded".into()), ..Default::default() });
        let mut s = page(8);
        s.set_inpainter(Some(fake.clone()));
        let gpu = DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(8.0), cores: Some(8), ..Default::default() };
        s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: std::sync::Mutex::new(gpu), ..Default::default() })));
        let r = s.execute(ID, params).unwrap();
        let last = fake.last.lock().unwrap().unwrap();
        (s, r, last)
    };
    let (s, r, last) = run(json!({}));
    assert_eq!(last, ("migan-512", true), "Auto: MI-GAN on the CPU");
    assert_eq!((r["model"].as_str(), r["lite"].as_bool()), (Some("migan-512"), Some(true)));
    let n = &r["notices"][0];
    assert_eq!(n["class"].as_str(), Some(crate::ai_cmds::GPU_FALLBACK_CLASS));
    assert!(n["cause"].as_str().unwrap().contains("Shared memory"));
    let layer = LayerId(r["layer"].as_u64().unwrap());
    assert!(!is_red(&layer_px(&s, layer, 31, 22)), "the text is cleaned");
    let (_, r, last) = run(json!({"quality": "best"}));
    assert_eq!((last, r["model"].as_str()), (("manga-lama", true), Some("manga-lama")), "Best keeps LaMa");
    let (_, _, last) = run(json!({"model": "manga-lama"}));
    assert_eq!(last, ("manga-lama", true), "a named model stays");
}

/// Tiles are filled in place: a later tile whose context reaches an earlier tile's hole must get
/// exactly what it got from a separate copy (the hole is masked out of its input), so the
/// result equals filling every tile from the untouched region.
#[test]
fn in_place_tiles_match_tiles_filled_from_the_original() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 160, "height": 64, "depth": 8, "background": "white"})).unwrap();
    s.edit("noise", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        for x in 0..160 {
            let v = ((x * 37) % 23) as f32 / 23.0;
            surf.fill_rect(Rect::new(x, 0, x + 1, 64), &[v, 1.0 - v, (x % 7) as f32 / 7.0, 1.0]);
        }
        Ok(())
    })
    .unwrap();
    // Two blobs 20 px apart: with a 24 px margin each tile's context covers the other's hole.
    s.execute("select.rect", json!({"x": 40, "y": 20, "width": 30, "height": 20})).unwrap();
    s.execute("select.rect", json!({"x": 90, "y": 22, "width": 30, "height": 20, "mode": "add"})).unwrap();
    let id = active_id(&s);
    let opts = json!({"output": "current", "maxSide": 64, "margin": 24});
    let p = plan(&s, &opts).unwrap().read().unwrap().finish(&crate::jobs::JobCtx::default()).unwrap();
    let (w, h) = (p.region.width() as usize, p.region.height() as usize);
    let t = tiles::plan(&p.cover, w, h, &p.opts.tiles).unwrap();
    assert!(t.len() >= 2, "{} tiles", t.len());
    assert!(t.iter().any(|a| t.iter().any(|b| a != b && a.rect.x < b.core.x + b.core.w && b.core.x < a.rect.x + a.rect.w)), "contexts overlap other holes");
    let binary: Vec<f32> = p.cover.iter().map(|c| if *c > 0.0 { 1.0 } else { 0.0 }).collect();
    let mut want = p.rgb.clone();
    for tile in &t {
        let input = tiles::extract(&p.rgb, &p.cover, w, h, tile).unwrap();
        let out = Arc::new(std::sync::Mutex::new(None));
        let sink = out.clone();
        struct NoHooks;
        impl photocraft_ai_lama::service::Hooks for NoHooks {
            fn progress(&self, _: f32, _: &str) {}
            fn cancelled(&self) -> bool {
                false
            }
        }
        FakeInpainter::default().run(photocraft_ai_lama::service::Request::lama(input, false), Arc::new(NoHooks), Box::new(move |r| *sink.lock().unwrap() = Some(r.unwrap())));
        let out = out.lock().unwrap().take().unwrap();
        tiles::paste(&mut want, &binary, w, h, tile, &out).unwrap();
    }
    let mut s = with_fake(s, FakeInpainter::default());
    s.execute(ID, opts).unwrap();
    let surf = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap();
    let got = to_rgb(&surf.format(), &surf.read_region(p.region));
    let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst <= 1.0 / 255.0 + 1e-4, "largest difference {worst}");
}

/// #22: an automatic run that left DirectML or found no usable GPU (the runtime's `fell_back`)
/// says so once per job, however many tiles, and the reply's `cpu` and `fellBack` say where it
/// really ran.
#[test]
fn a_silent_device_fallback_is_reported_once_per_job() {
    use photocraft_ai_lama::service::Fallback;
    let run = |from: &str, to: &str, cpu: bool| {
        let fallback = Some(Fallback { from: from.into(), to: to.into(), cpu, why: "injected".into() });
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 512, "height": 64, "background": "white"})).unwrap();
        // Two blobs far apart: two tiles, two requests.
        s.execute("select.rect", json!({"x": 4, "y": 4, "width": 8, "height": 8})).unwrap();
        s.execute("select.rect", json!({"x": 490, "y": 40, "width": 8, "height": 8, "mode": "add"})).unwrap();
        let mut s = with_fake(s, FakeInpainter { fallback, ..Default::default() });
        s.execute(ID, json!({"margin": 0})).unwrap()
    };
    let r = run("GPU", "CPU", true);
    assert_eq!((r["cpu"].clone(), r["fellBack"].clone()), (json!(true), json!(true)), "{r}");
    let n = r["notices"].as_array().unwrap();
    assert_eq!(n.len(), 1, "said once: {r}");
    assert_eq!((n[0]["class"].as_str(), n[0]["cause"].as_str()), (Some(crate::ai_cmds::CPU_FALLBACK_CLASS), Some("injected")));
    let r = run("DirectML", "Vulkan: test", false);
    assert_eq!((r["cpu"].clone(), r["fellBack"].clone()), (json!(false), json!(true)), "{r}");
    assert_eq!(r["notices"][0]["class"].as_str(), Some(crate::ai_cmds::DIRECTML_FALLBACK_CLASS));
    // Nothing left: as before.
    let mut s = with_fake(page(8), FakeInpainter::default());
    let r = s.execute(ID, json!({})).unwrap();
    assert_eq!((r["cpu"].clone(), r["fellBack"].clone(), r.get("notices")), (json!(false), json!(false), None), "{r}");
}
