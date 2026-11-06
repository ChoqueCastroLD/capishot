use std::sync::{Arc, Mutex};

use photocraft_ai::models::FakeStore;
use photocraft_ai::profile::{DeviceProfile, GpuClass};
use photocraft_ai::service::FakeUpscaler;
use photocraft_ai_lama::service::{FakeInpainter, Inpainter};
use photocraft_engine::Session;
use photocraft_engine::doc::Rect;
use photocraft_engine::jobs::{JobOutcome, Started};
use serde_json::{Value, json};

use super::*;

/// A white 64×48 page with a red blob at (28..36, 18..28) on its one layer, `fake` as the
/// inpainter and a machine with a GPU (or none).
fn page(fake: Arc<dyn Inpainter>, gpu: bool) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    s.edit("blob", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        surf.fill_rect(Rect::new(28, 18, 36, 28), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.set_inpainter(Some(fake));
    let gpu = if gpu { GpuClass::Discrete } else { GpuClass::None };
    let profile = DeviceProfile { gpu, memory_gb: Some(8.0), cores: Some(8), ..Default::default() };
    s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: Mutex::new(profile), ..Default::default() })));
    s
}

fn past(s: &Session) -> usize {
    s.active().unwrap().history.past_len()
}

fn last_step(s: &Session) -> Option<String> {
    s.active().unwrap().history.entries().last().cloned()
}

/// The active layer's pixels.
fn pixels(s: &Session) -> Vec<f32> {
    let st = s.active().unwrap();
    st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().read_region(st.doc.bounds())
}

/// The active layer's layer mask.
fn mask(s: &Session) -> Vec<f32> {
    let st = s.active().unwrap();
    st.doc.layer(st.active_layer.unwrap()).unwrap().mask.as_ref().unwrap().surface.read_region(st.doc.bounds())
}

fn over_the_blob() -> RemoveAi {
    RemoveAi::strokes(vec![vec![[29.0, 22.0, 1.0], [35.0, 22.0, 1.0]]], 14.0)
}

/// Runs `cmd` as a background job to its end (the browser's stepped hand-off included).
fn run_job(s: &mut Session, cmd: &str, p: Value) -> JobOutcome {
    match s.start(cmd, p).unwrap() {
        Started::Done(v) => JobOutcome::Done(v),
        Started::Job(job) => {
            for _ in 0..5000 {
                s.step_jobs(&|| true);
                if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                    return e.outcome;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("{cmd}: the job never ended");
        }
    }
}

// --- One undo step on every target ---------------------------------------------------------

#[test]
fn remove_ai_strokes_are_one_undo_step_on_the_pixels() {
    let mut s = page(Arc::new(FakeInpainter::default()), true);
    let (before, n) = (pixels(&s), past(&s));
    let r = s.execute(REMOVE_AI, over_the_blob().params().unwrap()).unwrap();
    assert_ne!(pixels(&s), before, "the blob was filled: {r}");
    assert_eq!(past(&s), n + 1, "one step");
    assert_eq!(last_step(&s).as_deref(), Some(photocraft_engine::remove_ai_cmds::LABEL));
    assert!(s.undo());
    assert_eq!(pixels(&s), before, "one Ctrl+Z takes it all back");
    assert_eq!(past(&s), n);
    assert!(s.redo());
    assert_eq!(past(&s), n + 1);
}

#[test]
fn remove_ai_on_the_selection_is_one_undo_step() {
    let mut s = page(Arc::new(FakeInpainter::default()), true);
    s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
    let (before, n) = (pixels(&s), past(&s));
    s.execute(REMOVE_AI, RemoveAi::selection().params().unwrap()).unwrap();
    assert_eq!(past(&s), n + 1);
    assert!(s.undo());
    assert_eq!(pixels(&s), before);
}

#[test]
fn remove_ai_into_the_layer_mask_is_one_undo_step() {
    let mut s = page(Arc::new(FakeInpainter::default()), true);
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    s.edit("hide", |doc, active| {
        let m = doc.layer_mut(active.unwrap()).unwrap().mask.as_mut().unwrap();
        m.surface.fill_rect(Rect::new(28, 18, 36, 28), &[0.0]);
        Ok(())
    })
    .unwrap();
    let (layer, before, n) = (pixels(&s), mask(&s), past(&s));
    let p = RemoveAi { target: PaintTarget::Mask, ..over_the_blob() }.params().unwrap();
    s.execute(REMOVE_AI, p).unwrap();
    assert_ne!(mask(&s), before, "the mask changed");
    assert_eq!(pixels(&s), layer, "the pixels didn't");
    assert_eq!(past(&s), n + 1);
    assert!(s.undo());
    assert_eq!(mask(&s), before);
}

#[test]
fn remove_ai_as_a_job_is_one_undo_step() {
    for selection in [false, true] {
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        if selection {
            s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
        }
        let (before, n) = (pixels(&s), past(&s));
        let p = if selection { RemoveAi::selection() } else { over_the_blob() }.params().unwrap();
        let out = run_job(&mut s, REMOVE_AI, p);
        let JobOutcome::Done(v) = out else { panic!("selection {selection}: {out:?}") };
        let reply = AiReply::from_value(&v);
        assert_eq!(reply.model, "manga-lama", "{v}");
        assert!(reply.rect.is_some(), "{v}");
        assert_eq!(past(&s), n + 1, "selection {selection}: one step");
        assert!(s.undo());
        assert_eq!(pixels(&s), before);
    }
}

#[test]
fn a_failed_remove_ai_leaves_no_step() {
    let mut s = page(Arc::new(FakeInpainter { fail: Some("boom".into()), ..Default::default() }), true);
    let (before, n) = (pixels(&s), past(&s));
    assert!(s.execute(REMOVE_AI, over_the_blob().params().unwrap()).is_err());
    assert!(matches!(run_job(&mut s, REMOVE_AI, over_the_blob().params().unwrap()), JobOutcome::Failed(_)));
    assert_eq!((pixels(&s), past(&s)), (before, n));
}

#[test]
fn clean_selected_area_and_bubble_text_are_one_undo_step() {
    let mut s = page(Arc::new(FakeInpainter::default()), true);
    s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
    let n = past(&s);
    s.execute(INPAINT_AI, InpaintAi { into_layer: true, quality: AiQuality::Best, ..Default::default() }.params().unwrap()).unwrap();
    assert_eq!(past(&s), n + 1);
    // A white bubble with a black outline and a black "letter" in it.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 200, "height": 200, "background": "white"})).unwrap();
    s.edit("bubble", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        for r in [Rect::new(40, 40, 160, 44), Rect::new(40, 156, 160, 160), Rect::new(40, 40, 44, 160), Rect::new(156, 40, 160, 160)] {
            surf.fill_rect(r, &[0.0, 0.0, 0.0, 1.0]);
        }
        surf.fill_rect(Rect::new(90, 90, 110, 96), &[0.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    let (before, n) = (pixels(&s), past(&s));
    let r = s.execute(REMOVE_BUBBLE_TEXT, RemoveBubbleText::at(100.0, 70.0).params().unwrap()).unwrap();
    assert_eq!(past(&s), n + 1, "{r}");
    assert!(s.undo());
    assert_eq!(pixels(&s), before);
}

// --- The device -------------------------------------------------------------------------------

#[test]
fn a_cpu_only_host_runs_every_request_on_the_cpu() {
    let fake = Arc::new(FakeInpainter::default());
    let mut s = page(cpu_inpainter(fake.clone(), true), true);
    s.execute(REMOVE_AI, over_the_blob().params().unwrap()).unwrap();
    assert_eq!(*fake.last.lock().unwrap(), Some(("manga-lama", true)), "on the CPU though there is a GPU");
    // Auto: the GPU.
    let fake = Arc::new(FakeInpainter::default());
    let mut s = page(cpu_inpainter(fake.clone(), false), true);
    s.execute(REMOVE_AI, over_the_blob().params().unwrap()).unwrap();
    assert_eq!(*fake.last.lock().unwrap(), Some(("manga-lama", false)));
}

#[test]
fn a_cpu_only_upscaler_reports_no_gpu() {
    let profile = DeviceProfile { gpu: GpuClass::Discrete, gpu_name: "Test GPU".into(), cores: Some(8), ..Default::default() };
    let up: Arc<dyn photocraft_ai::service::Upscaler> = Arc::new(FakeUpscaler { profile: Mutex::new(profile), ..Default::default() });
    let p = cpu_upscaler(up.clone(), true).profile();
    assert_eq!((p.gpu, p.gpu_name.as_str(), p.cores), (GpuClass::None, "", Some(8)));
    assert_eq!(cpu_upscaler(up, false).profile().gpu, GpuClass::Discrete);
}

#[test]
fn the_gpu_failing_mid_run_says_so_in_the_reply() {
    let fake = Arc::new(FakeInpainter { gpu_fail: Some("device lost".into()), ..Default::default() });
    let mut s = page(fake, true);
    let r = AiReply::from_value(&s.execute(REMOVE_AI, over_the_blob().params().unwrap()).unwrap());
    assert!(r.cpu && r.fell_back, "{:?}", r.raw);
    assert_eq!(r.model, "manga-lama");
}

// --- Setting up --------------------------------------------------------------------------------

#[test]
fn models_url_defaults_and_no_downloads() {
    let c = |u: Option<&str>| AiConfig { models_url: u.map(String::from), ..Default::default() };
    assert_eq!(models_base(&c(None)).as_deref(), Some(DEFAULT_MODELS_URL));
    assert_eq!(models_base(&c(Some(" "))), None);
    assert_eq!(models_base(&c(Some("https://example.com/m"))).as_deref(), Some("https://example.com/m/"));
    assert_eq!(models_base(&c(Some("https://example.com/m/"))).as_deref(), Some("https://example.com/m/"));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn install_without_a_models_folder_or_the_feature_sets_nothing_and_says_why() {
    let mut s = Session::new();
    let a = install(&mut s, &AiConfig::default());
    assert!(!a.inpaint && !a.upscale && !a.models && a.reason.is_some(), "{a:?}");
    assert!(!availability(&s).inpaint);
    // Without services the AI commands fail cleanly; the Bubble Text Remover still works.
    s.execute("file.new", json!({"width": 32, "height": 32, "background": "white"})).unwrap();
    assert!(s.execute(REMOVE_AI, over_the_blob().params().unwrap()).is_err());
    assert!(s.execute(UPSCALE_AI, UpscaleAi::default().params().unwrap()).is_err());
}

#[cfg(all(feature = "ai-native", not(target_arch = "wasm32")))]
#[test]
fn install_natively_sets_every_service_in_the_host_folder() {
    let dir = std::env::temp_dir().join(format!("capishot-host-ai-{}", std::process::id()));
    let mut s = Session::new();
    let a = install(&mut s, &AiConfig { models_dir: Some(dir.clone()), models_url: Some(String::new()), generative_fill: true, ..Default::default() });
    assert!(a.inpaint && a.upscale && a.models && a.generative_fill && a.reason.is_none(), "{a:?}");
    assert!(dir.is_dir(), "the folder was created");
    // No weights in it and no downloads: Remove with AI isn't ready.
    let st = model_status(&s, lama().unwrap()).unwrap();
    assert!(!matches!(st, Status::Downloaded), "{st:?}");
    uninstall(&mut s);
    assert!(!availability(&s).inpaint);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn prepare_remove_ai_starts_the_first_download() {
    let mut s = Session::new();
    assert!(matches!(prepare_remove_ai(&s), Readiness::Unavailable { .. }), "no inpainter");
    s.set_inpainter(Some(Arc::new(FakeInpainter::default())));
    assert_eq!(prepare_remove_ai(&s), Readiness::Ready, "no model service: the inpainter finds its weights");
    let store = Arc::new(FakeStore::default());
    s.set_ai_models(Some(store.clone()));
    assert!(matches!(prepare_remove_ai(&s), Readiness::Downloading { .. }));
    assert!(!matches!(model_status(&s, lama().unwrap()), Some(Status::NotDownloaded)), "queued");
    while store.step() {}
    assert_eq!(prepare_remove_ai(&s), Readiness::Ready);
}

// --- Params and replies ---------------------------------------------------------------------

#[test]
fn typed_params_match_the_control_protocol() {
    let p = RemoveAi { layer: Some(LayerId(7)), sample_all_layers: true, allow_download: true, ..over_the_blob() }.params().unwrap();
    assert_eq!(p["strokes"], json!([[[29.0, 22.0, 1.0], [35.0, 22.0, 1.0]]]));
    assert_eq!((p["size"].clone(), p["layer"].clone(), p["maxSide"].clone()), (json!(14.0), json!(7), json!(0)));
    assert_eq!((p["sampleAllLayers"].clone(), p["allowDownload"].clone(), p["target"].clone()), (json!(true), json!(true), json!("pixels")));
    let p = RemoveAi::selection().params().unwrap();
    assert!(p.get("strokes").is_none() && p.get("size").is_none());
    assert_eq!(RemoveAi { target: PaintTarget::Channel(2), ..RemoveAi::selection() }.params().unwrap()["target"], json!({"channel": 2}));
    for bad in [RemoveAi { size: 0.0, ..over_the_blob() }, RemoveAi { size: f64::NAN, ..over_the_blob() }, RemoveAi { max_side: 100, ..over_the_blob() }, RemoveAi { max_side: 4096, ..RemoveAi::selection() }] {
        assert!(bad.params().is_err(), "{bad:?}");
    }
    assert!(RemoveAi::strokes(vec![vec![[f64::INFINITY, 0.0, 1.0]]], 10.0).params().is_err());
    let p = InpaintAi { max_side: Some(0), ..Default::default() }.params().unwrap();
    assert_eq!((p["output"].clone(), p["quality"].clone(), p["maxSide"].clone()), (json!("new"), json!("auto"), json!(0)));
    assert!(InpaintAi { max_side: Some(63), ..Default::default() }.params().is_err());
    let p = RemoveBubbleText { tolerance: Some(40.0), new_layer: true, ..RemoveBubbleText::at(10.0, 20.0) }.params().unwrap();
    assert_eq!(p, json!({"x": 10.0, "y": 20.0, "tolerance": 40.0, "sampleAll": false, "newLayer": true}));
    assert_eq!(RemoveBubbleText::at(1.0, 2.0).params().unwrap()["tolerance"], "auto");
    assert!(RemoveBubbleText { tolerance: Some(0.0), ..RemoveBubbleText::at(1.0, 2.0) }.params().is_err());
    assert!(RemoveBubbleText::at(f64::NAN, 2.0).params().is_err());
    let p = UpscaleAi { scale: 4, output: UpscaleOutput::NewDocument, ..Default::default() }.params().unwrap();
    assert_eq!((p["scale"].clone(), p["model"].clone(), p["output"].clone()), (json!(4), json!("auto"), json!("newDocument")));
    assert!(UpscaleAi { scale: 3, ..Default::default() }.params().is_err());
    assert!(UpscaleAi { model: Some("nope".into()), ..Default::default() }.params().is_err());
}

#[test]
fn replies_are_read_whatever_their_shape() {
    let v = json!({
        "damage": [1, 2, 3, 4], "device": "Vulkan (Test)", "model": "manga-lama", "lite": false, "cpu": true, "fellBack": true, "ms": 12,
        "notices": [{"class": "ai.gpuFallback", "text": "GPU couldn't run this model; used the CPU instead", "cause": "device lost"}, "junk"],
    });
    let r = AiReply::from_value(&v);
    assert_eq!((r.device.as_str(), r.model.as_str(), r.cpu, r.fell_back, r.ms, r.rect), ("Vulkan (Test)", "manga-lama", true, true, Some(12), Some([1, 2, 3, 4])));
    assert_eq!(r.notices.len(), 1);
    assert_eq!(r.notices[0].class, "ai.gpuFallback");
    let r = AiReply::from_value(&json!({"region": {"x": 5, "y": 6, "width": 7, "height": 8}, "layer": 3}));
    assert_eq!((r.rect, r.layer), (Some([5, 6, 7, 8]), Some(LayerId(3))));
    let r = AiReply::from_value(&json!([1, 2]));
    assert_eq!((r.rect, r.device.as_str(), r.ms), (None, "", None));
}

/// A native host's `allowDownload` reaches the download (it used to fail on the missing file
/// without trying); `models_url: ""` means no downloads.
#[cfg(all(feature = "ai-native", not(target_arch = "wasm32")))]
#[test]
fn native_allow_download_fetches_from_the_models_url() {
    use photocraft_ai_lama::service::{GpuBackend, Hooks, LAMA, Request};
    struct Quiet;
    impl Hooks for Quiet {
        fn progress(&self, _: f32, _: &str) {}
        fn cancelled(&self) -> bool {
            false
        }
    }
    let error_with = |url: &str, name: &str| {
        let dir = std::env::temp_dir().join(format!("capishot-host-fetch-{name}-{}", std::process::id()));
        let mut s = Session::new();
        install(&mut s, &AiConfig { models_dir: Some(dir.clone()), models_url: Some(url.into()), device: AiDevice::Cpu, ..Default::default() });
        let tile = photocraft_ai_lama::tiles::TileInput { rgb: vec![0.0; 3 * 8 * 8], mask: vec![0.0; 8 * 8], w: 8, h: 8 };
        let req = Request { tile, allow_download: true, model: LAMA, cpu: true, threads: 1, unload: true, backend: GpuBackend::Auto };
        let out = Arc::new(Mutex::new(None));
        let sink = out.clone();
        s.inpainter().unwrap().run(req, Arc::new(Quiet), Box::new(move |r| *sink.lock().unwrap() = Some(r.map(|_| ()))));
        let _ = std::fs::remove_dir_all(dir);
        out.lock().unwrap().take().unwrap().unwrap_err().to_string()
    };
    let e = error_with("http://127.0.0.1:1/", "closed");
    assert!(e.contains("127.0.0.1"), "tried the models URL: {e}");
    let e = error_with("", "off");
    assert!(e.contains("not found"), "no downloads: {e}");
}

/// A host warms the model at start-up, before any document is open.
#[test]
fn warm_ai_needs_no_document() {
    let mut host = crate::Host::new();
    host.session_mut().set_inpainter(Some(Arc::new(FakeInpainter::default())));
    assert!(host.active_document().is_none());
    assert!(host.warm_ai("manga-lama").is_ok());
}
