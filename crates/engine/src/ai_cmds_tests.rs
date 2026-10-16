use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use photocraft_ai::service::FakeUpscaler;
use serde_json::json;

use super::*;
use crate::jobs::{JobOutcome, Started};

/// A `w`×`h` document painted with `f(x, y) -> rgb`.
fn session(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                px.extend_from_slice(&[r, g, b, 1.0]);
            }
        }
        surf.write_region(photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &px);
        Ok(())
    })
    .unwrap();
    s
}

fn gray(x: u32, y: u32) -> [f32; 3] {
    let v = ((x * 3 + y * 5) % 17) as f32 / 16.0;
    [v, v, v]
}

fn colour(x: u32, _: u32) -> [f32; 3] {
    [x as f32 / 10.0, 0.2, 0.9]
}

fn with_fake(mut s: Session) -> (Session, Arc<FakeUpscaler>) {
    let fake = Arc::new(FakeUpscaler::default());
    s.set_ai_upscaler(Some(fake.clone()));
    (s, fake)
}

#[test]
fn disabled_without_an_upscaler() {
    let mut s = session(4, 4, gray);
    let e = s.execute(COMMAND, json!({})).unwrap_err().to_string();
    assert!(e.contains(UNAVAILABLE), "{e}");
}

#[test]
fn picks_mangajanai_for_gray_pages_and_illustrationjanai_for_colour() {
    let (s, _) = with_fake(session(8, 6, gray));
    let p = plan(&s, &json!({"scale": 4})).unwrap();
    assert_eq!(p.model.id, "mangajanai-v1-4x-1200p");
    assert!(p.grayscale && p.detected && p.auto_levels);
    assert_eq!(p.output, Output::NewDocument, "two layers: a new document");
    let (s, _) = with_fake(session(8, 6, colour));
    let p = plan(&s, &json!({})).unwrap();
    assert_eq!((p.model.id, p.grayscale, p.scale()), ("illustrationjanai-v1-2x", false, 2));
    let p = plan(&s, &json!({"content": "grayscale"})).unwrap();
    assert_eq!(p.model.id, "mangajanai-v1-2x-1200p");
    let p = plan(&s, &json!({"model": "mangajanai-v1-4x-2048p"})).unwrap();
    assert_eq!((p.model.scale, p.grayscale), (4, true));
}

#[test]
fn replace_is_one_undo_step() {
    let (mut s, _) = with_fake(session(8, 6, gray));
    let r = s.execute(COMMAND, json!({"scale": 2, "output": "replace", "autoLevels": false})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["content"].as_str()), (Some(16), Some(12), Some("grayscale")));
    let d = s.active().unwrap();
    assert_eq!((d.doc.size.width, d.doc.size.height, d.doc.layers.len()), (16, 12, 1));
    // Nearest-neighbour fake: each source pixel became a 2×2 block of the same gray.
    let px = photocraft_compose::flatten(&d.doc).px;
    let src = gray(3, 2)[0];
    assert!((px[(2 * 2) * 16 + 3 * 2][0] - src).abs() < 1e-3);
    assert!((px[(2 * 2 + 1) * 16 + 3 * 2 + 1][1] - src).abs() < 1e-3);
    s.execute("edit.undo", json!({})).unwrap();
    let d = s.active().unwrap();
    assert_eq!((d.doc.size.width, d.doc.size.height, d.doc.layers.len()), (8, 6, 2));
}

#[test]
fn new_document_output_keeps_the_original() {
    let (mut s, _) = with_fake(session(5, 3, colour));
    let r = s.execute(COMMAND, json!({"scale": 4, "output": "newDocument"})).unwrap();
    assert_eq!(r["model"], "illustrationjanai-v1-4x");
    assert_eq!(s.documents().len(), 2);
    let i = r["document"].as_u64().unwrap() as usize;
    assert_eq!((s.documents()[i].doc.size.width, s.documents()[i].doc.size.height), (20, 12));
    assert!(s.documents()[i].doc.name.contains("4x AI"));
    assert_eq!(s.documents()[0].doc.size.width, 5);
}

#[test]
fn downloads_need_consent() {
    let (mut s, fake) = with_fake(session(4, 4, gray));
    fake.download_needed.store(true, Ordering::Relaxed);
    let e = s.execute(COMMAND, json!({})).unwrap_err().to_string();
    assert!(e.contains("isn't downloaded") && e.contains("MB") && e.contains("AI Models"), "{e}");
    s.execute(COMMAND, json!({"download": true, "output": "replace"})).unwrap();
    assert_eq!(s.active().unwrap().doc.size.width, 8);
}

#[test]
fn bad_params_are_errors_not_panics() {
    let (mut s, _) = with_fake(session(4, 4, gray));
    for p in [
        json!({"scale": 3}),
        json!({"scale": "big"}),
        json!({"scale": -1}),
        json!({"model": "nope"}),
        json!({"model": "manga-lama"}),
        json!({"model": "mangajanai-v1-4x-1200p", "scale": 2}),
        json!({"content": "sepia"}),
        json!({"output": "printer"}),
    ] {
        assert!(s.execute(COMMAND, p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.active().unwrap().doc.size.width, 4);
    // A failing model leaves the document alone.
    let mut s = session(4, 4, gray);
    s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { fail: Some("GPU lost".into()), ..Default::default() })));
    assert!(s.execute(COMMAND, json!({"output": "replace"})).unwrap_err().to_string().contains("GPU lost"));
    assert_eq!(s.active().unwrap().doc.size.width, 4);
}

#[test]
fn rejects_cmyk_and_oversized_results() {
    let (mut s, _) = with_fake(session(4, 4, gray));
    s.execute("image.mode.cmyk", json!({})).unwrap();
    assert!(s.execute(COMMAND, json!({})).is_err());
    let (mut s, _) = with_fake(session(17_000, 2, gray));
    assert!(s.execute(COMMAND, json!({"scale": 4})).unwrap_err().to_string().contains("too large"));
}

#[test]
fn transparency_is_kept() {
    let (mut s, _) = with_fake(session(4, 4, gray));
    // Hide the white background: the painted layer is opaque, so delete it and paint half alpha.
    s.edit("alpha", |doc, active| {
        doc.layers.retain(|l| Some(l.id) == *active);
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        surf.write_region(photocraft_geom::Rect::new(0, 0, 4, 4), &[0.2, 0.2, 0.2, 0.5].repeat(16));
        Ok(())
    })
    .unwrap();
    s.execute(COMMAND, json!({"output": "replace", "autoLevels": false})).unwrap();
    let px = photocraft_compose::flatten(&s.active().unwrap().doc).px;
    assert!(px.iter().all(|q| (q[3] - 0.5).abs() < 0.01 && (q[0] - 0.2).abs() < 0.01), "{:?}", px[0]);
}

#[test]
fn runs_as_a_background_job() {
    let (mut s, _) = with_fake(session(6, 6, gray));
    let id = match s.start(COMMAND, json!({"output": "replace"})).unwrap() {
        Started::Job(id) => id,
        Started::Done(v) => panic!("ran inline: {v}"),
    };
    let t = Instant::now();
    let ev = loop {
        if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == id) {
            break e;
        }
        assert!(t.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(matches!(ev.outcome, JobOutcome::Done(_)), "{:?}", ev.outcome);
    assert_eq!(s.active().unwrap().doc.size.width, 12);
}

#[test]
fn bilinear_alpha_plane() {
    let up = upscale_plane(&[0.0, 1.0], 2, 1, 2);
    assert_eq!(up.len(), 8);
    assert!((up[0]).abs() < 1e-6 && (up[3] - 1.0).abs() < 1e-6 && up[1] > 0.0 && up[1] < up[2]);
    assert!(upscale_plane(&[], 0, 0, 2).is_empty());
}

/// Quality / Device pick MangaJaNai or the lite Real-ESRGAN Compact; Advanced settings reach the
/// upscaler.
#[test]
fn quality_device_and_advanced_settings() {
    use photocraft_ai::profile::{DeviceProfile, GpuClass, Reason};
    let (s, fake) = with_fake(session(8, 6, gray));
    *fake.profile.lock().unwrap() = DeviceProfile { gpu: GpuClass::None, cores: Some(8), ..Default::default() };
    let p = plan(&s, &json!({"scale": 4})).unwrap();
    assert_eq!((p.model.id, p.model.lite, p.run.choice.reason, p.opts.cpu), ("realesr-compact-4x", true, Reason::NoGpu, true));
    assert!(p.grayscale, "the lite model runs one channel on gray pages too");
    let p = plan(&s, &json!({"scale": 2, "quality": "best"})).unwrap();
    assert_eq!((p.model.id, p.opts.cpu), ("mangajanai-v1-2x-1200p", true));
    *fake.profile.lock().unwrap() = DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(8.0), cores: Some(8), ..Default::default() };
    let p = plan(&s, &json!({"scale": 2})).unwrap();
    assert_eq!((p.model.id, p.opts.cpu, p.run.choice.reason), ("mangajanai-v1-2x-1200p", false, Reason::Gpu));
    let p = plan(&s, &json!({"scale": 2, "quality": "fast", "tile": 192, "overlap": 8, "threads": 3, "keepLoaded": false})).unwrap();
    assert_eq!(p.model.id, "realesr-compact-2x");
    assert_eq!(p.opts.tiles, Some(TileOptions { tile: 192, overlap: 8 }));
    assert_eq!((p.opts.threads, p.opts.unload), (3, true));
    let p = plan(&s, &json!({"model": "realesr-compact-4x", "scale": 4})).unwrap();
    assert!(p.model.lite && p.detected);
    for bad in [json!({"quality": "max"}), json!({"device": "tpu"}), json!({"tile": 8}), json!({"tile": 64, "overlap": 32}), json!({"threads": -1})] {
        assert!(plan(&s, &bad).is_err(), "{bad}");
    }
    // The run carries the options.
    let (mut s, fake) = with_fake(session(8, 6, gray));
    let r = s.execute(COMMAND, json!({"quality": "fast", "device": "cpu", "output": "newDocument"})).unwrap();
    assert_eq!((r["model"].as_str(), r["lite"].as_bool()), (Some("realesr-compact-2x"), Some(true)));
    assert_eq!(fake.last_opts.lock().unwrap().map(|o| o.cpu), Some(true));
}

/// The GPU couldn't run the model: the same request runs on the CPU, with the lite model on
/// Quality Auto (the full one on Best, the named one when the params name it), and the result
/// says so in `notices`.
#[test]
fn a_gpu_failure_runs_again_on_the_cpu() {
    use photocraft_ai::profile::{DeviceProfile, GpuClass};
    let run = |params: serde_json::Value| {
        let gpu = DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(8.0), cores: Some(8), ..Default::default() };
        let fake = Arc::new(FakeUpscaler { gpu_fail: Some("Shared memory budget exceeded".into()), profile: std::sync::Mutex::new(gpu), ..Default::default() });
        let mut s = session(4, 4, gray);
        s.set_ai_upscaler(Some(fake.clone()));
        let r = s.execute(COMMAND, params).unwrap();
        let last = (fake.last_model.lock().unwrap().unwrap(), fake.last_opts.lock().unwrap().unwrap().cpu);
        (r, last)
    };
    let (r, last) = run(json!({"output": "replace"}));
    assert_eq!(last, (catalog::COMPACT_2X_ID, true), "Auto: the lite model on the CPU");
    assert_eq!((r["model"].as_str(), r["lite"].as_bool(), r["width"].as_u64()), (Some(catalog::COMPACT_2X_ID), Some(true), Some(8)));
    let n = &r["notices"][0];
    assert_eq!((n["class"].as_str(), n["text"].as_str()), (Some(GPU_FALLBACK_CLASS), Some(GPU_FALLBACK_NOTICE)));
    assert!(n["cause"].as_str().unwrap().contains("Shared memory"));
    let (r, last) = run(json!({"output": "replace", "quality": "best"}));
    assert_eq!((last.0, last.1, r["lite"].as_bool()), ("mangajanai-v1-2x-1200p", true, Some(false)), "Best keeps the full model");
    let (_, last) = run(json!({"output": "replace", "model": "mangajanai-v1-2x-1200p"}));
    assert_eq!(last, ("mangajanai-v1-2x-1200p", true), "a named model stays");
    // Already on the CPU: an error is an error.
    let (mut s, _) = with_fake(session(4, 4, gray));
    s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { fail: Some("out of memory".into()), ..Default::default() })));
    assert!(s.execute(COMMAND, json!({"device": "cpu"})).is_err());
}
