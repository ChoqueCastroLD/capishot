//! `edit.removeAi`: the Remove with AI tool. What the user painted (`strokes`, as the Remove
//! tool's) or, without strokes, the selection, is erased and filled as Clean Selected Area with AI
//! (`edit.inpaintAi`) fills a selection, with fixed options: Manga LaMa (never the lite model) at
//! best quality (on a device with 2 GB of memory or less, where Manga LaMa's ~500 MB doesn't fit
//! beside the page, MI-GAN, the lite model: [`model`]), f32, the default working side and context margin, at full resolution: a hole
//! larger than the working side is split into overlapping tiles feathered at their seams, never
//! downscaled (`maxSide` > 0 caps and downscales as `edit.inpaintAi` does). The AI Cleaning
//! Options dialog doesn't apply. The GPU runs it when there is one, the CPU otherwise or when the GPU fails
//! mid-run (still Manga LaMa). A stroke's hole grows first so nothing of what it touched is left for the
//! model to rebuild from (`retouch_cmds::ink_mask`). One undo step, "Remove with AI", on the
//! target layer, or with `output` on a layer of its own or one for all the cleans. A background
//! job, cancellable.

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const ID: &str = "edit.removeAi";
/// The undo step and the job.
pub const LABEL: &str = "Remove with AI";
/// What the AI Cleaning Options dialog would set, overridden on every run: Remove with AI is
/// fixed (Manga LaMa at its best, the GPU when there is one, the working side and context
/// margin `edit.inpaintAi` uses by default) and never follows that dialog. `maxSide` is the
/// caller's (the dialog never sends it here): default 0, full resolution.
const FIXED: [&str; 6] = ["model", "quality", "device", "margin", "threads", "keepLoaded"];

/// The model Remove with AI runs on this device: Manga LaMa, or MI-GAN where memory is short
/// (2 GB or less: Manga LaMa adds about 500 MB in the AI worker, and the tab is killed).
pub fn model(s: &Session) -> Option<&'static photocraft_ai::catalog::ModelInfo> {
    use photocraft_ai::catalog;
    catalog::model(if lite(s) { catalog::MIGAN_ID } else { catalog::LAMA_ID })
}

/// Remove with AI uses the lite model on this device ([`model`]).
pub fn lite(s: &Session) -> bool {
    s.ai_profile().low_memory()
}

/// The inpainting params of a run: Clean Selected Area with AI's (`edit.inpaintAi`) with this
/// device's [`model`] pinned (Manga LaMa at best quality, MI-GAN at fast), full resolution unless
/// `maxSide` caps it, and every other option at its default, whatever the caller sent.
pub fn run_params(s: &Session, p: &Value) -> Result<serde_json::Map<String, Value>> {
    let mut q = match p {
        Value::Object(m) => m.clone(),
        Value::Null => serde_json::Map::new(),
        _ => return Err(EngineError::BadParams { cmd: ID.into(), msg: "params must be a JSON object".into() }),
    };
    for k in FIXED.iter().chain(&["ai", "output", "newLayer", "keepActive"]) {
        q.remove(*k);
    }
    let lite = lite(s);
    q.insert("model".into(), json!(if lite { photocraft_ai::catalog::MIGAN_ID } else { photocraft_ai::catalog::LAMA_ID }));
    q.insert("quality".into(), json!(if lite { "fast" } else { "best" }));
    q.entry("maxSide").or_insert(json!(0));
    Ok(q)
}

/// Whether this device's [`model`] can run now (the tool's download prompt).
pub fn model_state(s: &Session) -> Option<photocraft_ai_lama::service::ModelState> {
    let p = Value::Object(run_params(s, &json!({})).ok()?);
    crate::ai_inpaint_cmds::model_state(s, &p).map(|(_, st)| st)
}

fn remove_ai(s: &mut Session, p: &Value) -> Result<Value> {
    use crate::ai_inpaint_cmds::Output;
    let output = Output::parse(p, Output::Current)?;
    let mut q = run_params(s, p)?;
    // The page stays the active layer: the next clean reads it.
    q.insert("keepActive".into(), json!(true));
    if q.contains_key("strokes") || q.contains_key("points") {
        q.insert("ai".into(), json!(true));
        q.insert("newLayer".into(), json!(output != Output::Current));
        q.insert("output".into(), p.get("output").cloned().unwrap_or(Value::Null));
        if output != Output::Current
            && !q.contains_key("layer")
            && let Some((doc, id)) = s.active().and_then(|st| Some((st.doc.clone(), st.active_layer?)))
        {
            q.insert("layer".into(), json!(crate::ai_inpaint_cmds::source_layer(&doc, id, &output).0));
        }
        q.entry("closeLoops").or_insert(json!(true));
        return crate::retouch_cmds::remove::remove_ink_labelled(s, &Value::Object(q), LABEL);
    }
    q.entry("output").or_insert(json!("current"));
    if let Some(o) = p.get("output") {
        q.insert("output".into(), o.clone());
    }
    crate::ai_inpaint_cmds::inpaint_ai_labelled(s, &Value::Object(q), LABEL)
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    if s.inpainter().is_none() {
        return Err(crate::ai_inpaint_cmds::UNAVAILABLE.into());
    }
    crate::edit_menu_cmds::pixel_layer(s).map(|_| ())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: LABEL,
        menu: &[],
        shortcut: None,
        params: r#"{"strokes"?: [[[x,y,pressure?],…] or {"points", "size"?, "hardness"?, "spacing"?, "erase"?: bool},…] or "points"?: [[x,y],…] (what was painted, with "size": px, as paint.remove; a stroke around an object removes what it encloses; a whole mask at once: its strokes in order, "erase" ones taking theirs back out), else the selection, "sampleAllLayers"?: bool, "layer"?: id, "maxSide"?: 0 (default: full resolution, a hole larger than 512 px split into overlapping tiles feathered at their seams) or 64..2048, multiple of 8 (larger crops downscaled to it, as before), "allowDownload"?: bool (fetch Manga LaMa, about 100 MB, when it isn't on this device), "output"?: "current" (default, into the layer) | "new" (each clean on a new layer above, only what was generated) | "single" (one layer above for all the cleans), "layerName"?: str (what those layers are called), "byBlocks"?: bool (a run for each block of the selection)} -> {"damage"|"region", "device", "model", "lite", "cpu", "fellBack", "ms", …}. Erases it and fills it as edit.inpaintAi fills a selection, with fixed options (Manga LaMa at best quality, or MI-GAN on a device with 2 GB of memory or less, its default working side and margin, at full resolution; model, quality, device, margin, threads and keepLoaded params are ignored), on the target layer, as one undo step ("Remove with AI"). A background job; the GPU's failure retries on the CPU."#,
        enabled,
        run: remove_ai,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_run_is_manga_lama_at_best_whatever_the_options_say() {
        let dialog = json!({"model": "migan-512", "quality": "fast", "device": "cpu", "maxSide": 64, "margin": 8, "threads": 2, "keepLoaded": false, "strokes": [[[1, 2]]], "ai": false});
        let mut q = run_params(&Session::new(), &dialog).unwrap();
        assert_eq!((q["model"].as_str(), q["quality"].as_str()), (Some("manga-lama"), Some("best")));
        for k in ["device", "margin", "threads", "keepLoaded", "ai"] {
            assert!(!q.contains_key(k), "{k} is fixed");
        }
        assert_eq!(q["maxSide"], 64, "the caller's maxSide goes through");
        assert_eq!(run_params(&Session::new(), &json!({})).unwrap()["maxSide"], 0, "full resolution by default");
        assert!(q.contains_key("strokes"), "the painted strokes go through");
        q.insert("maxSide".into(), json!(0));
        assert_eq!(run_params(&Session::new(), &json!({})).unwrap(), q.into_iter().filter(|(k, _)| k != "strokes").collect());
        assert!(run_params(&Session::new(), &json!([1])).is_err());
    }

    fn layers(s: &Session) -> Vec<String> {
        s.active().unwrap().doc.layers.iter().map(|l| l.name.clone()).collect()
    }

    #[test]
    fn a_clean_can_go_into_the_layer_on_one_of_its_own_or_on_one_for_all() {
        let select = |s: &mut Session, x: i32| s.execute("select.rect", json!({"x": x, "y": 10, "width": 10, "height": 10})).unwrap();
        // Into the layer, by default.
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        select(&mut s, 5);
        s.execute(ID, json!({})).unwrap();
        assert_eq!(layers(&s).len(), 1);
        // A layer each.
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        let page_layer = s.active().unwrap().active_layer;
        for x in [5, 30] {
            select(&mut s, x);
            s.execute(ID, json!({"output": "new", "layerName": "Limpieza"})).unwrap();
            assert_eq!(s.active().unwrap().active_layer, page_layer, "the page stays active: the next clean reads it");
        }
        assert_eq!(layers(&s).len(), 3, "{:?}", layers(&s));
        // One for all.
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        for x in [5, 30] {
            select(&mut s, x);
            s.execute(ID, json!({"output": "single", "layerName": "Limpieza"})).unwrap();
        }
        assert_eq!(layers(&s).len(), 2, "{:?}", layers(&s));
        assert_eq!(layers(&s)[1], "Limpieza");
        let st = s.active().unwrap();
        let surf = st.doc.layers[1].surface().unwrap();
        assert!(surf.pixel(8, 12)[3] > 0.5 && surf.pixel(33, 12)[3] > 0.5, "both cleans on it");
        assert!(surf.pixel(60, 40)[3] < 0.5, "and nothing else");
        // With that layer active, the next clean still reads the page.
        let clean = st.doc.layers[1].id;
        s.active_mut().unwrap().active_layer = Some(clean);
        select(&mut s, 45);
        s.execute(ID, json!({"output": "single", "layerName": "Limpieza"})).unwrap();
        assert_eq!(layers(&s).len(), 2);
        assert!(s.active().unwrap().doc.layers[1].surface().unwrap().pixel(48, 12)[3] > 0.5);
        assert!(s.execute(ID, json!({"output": "elsewhere"})).is_err());
    }

    use std::sync::Arc;

    use photocraft_ai::profile::{DeviceProfile, GpuClass};
    use photocraft_ai::service::FakeUpscaler;
    use photocraft_ai_lama::service::FakeInpainter;
    use photocraft_geom::Rect;

    /// A white page with a red blob at (28..36, 18..28); a GPU, or none.
    fn page(fake: Arc<FakeInpainter>, gpu: bool) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
        s.edit("blob", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            surf.fill_rect(Rect::new(28, 18, 36, 28), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
        s.set_inpainter(Some(fake));
        let profile = if gpu {
            DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(8.0), cores: Some(8), ..Default::default() }
        } else {
            DeviceProfile { gpu: GpuClass::None, cores: Some(8), ..Default::default() }
        };
        s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: std::sync::Mutex::new(profile), ..Default::default() })));
        s
    }

    fn red_at(s: &Session, x: i32, y: i32) -> bool {
        let id = s.active().unwrap().active_layer.unwrap();
        let v = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().pixel(x, y);
        v[0] > 0.9 && v[1] < 0.1
    }

    fn grey_at(s: &Session, x: i32, y: i32) -> f32 {
        let id = s.active().unwrap().active_layer.unwrap();
        s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().pixel(x, y)[0]
    }

    /// A white page with a word of two letters (60..72 and 76..88 × 40..70), a letter of the next
    /// word (120..132) and a long line under them (y 73..75).
    fn lettered(fake: Arc<FakeInpainter>) -> Session {
        let mut s = page(fake, true);
        s.execute("file.new", json!({"width": 240, "height": 120, "background": "white"})).unwrap();
        s.edit("letters", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            for r in [Rect::new(60, 40, 72, 70), Rect::new(76, 40, 88, 70), Rect::new(120, 40, 132, 70), Rect::new(0, 73, 240, 75)] {
                surf.fill_rect(r, &[0.0, 0.0, 0.0, 1.0]);
            }
            Ok(())
        })
        .unwrap();
        s
    }

    #[test]
    fn a_rough_stroke_over_a_word_leaves_nothing_of_it_and_nothing_else_changes() {
        // The stroke covers the word's middle only (y 47..63, x 50..86): its tops and bottoms and
        // the second letter's right edge are outside it.
        let stroke = json!({"strokes": [[[58, 55], [78, 55]]], "size": 16});
        let mut s = lettered(Arc::new(FakeInpainter::default()));
        s.execute(ID, stroke.clone()).unwrap();
        for (x, y) in [(61, 41), (66, 69), (87, 41), (87, 55), (80, 69)] {
            assert!(grey_at(&s, x, y) > 0.9, "the word is gone, at {x},{y}");
        }
        assert_eq!(grey_at(&s, 126, 55), 0.0, "the next word stays");
        assert_eq!(grey_at(&s, 30, 74), 0.0, "the line stays");
        assert_eq!(grey_at(&s, 74, 74), 0.0, "also under the word");
        // The Remove tool's AI mode with the same stroke leaves the word's edges.
        let mut s = lettered(Arc::new(FakeInpainter::default()));
        let mut old = stroke;
        old["ai"] = json!(true);
        s.execute("paint.remove", old).unwrap();
        assert!(grey_at(&s, 61, 41) < 0.5 && grey_at(&s, 87, 41) < 0.5, "paint.remove keeps its own hole");
    }

    #[test]
    fn strokes_are_removed_with_manga_lama_in_one_named_step() {
        let fake = Arc::new(FakeInpainter::default());
        let mut s = page(fake.clone(), true);
        let (layers, past) = (s.active().unwrap().doc.walk().len(), s.active().unwrap().history.past_len());
        s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap();
        assert!(!red_at(&s, 31, 22), "the blob is gone");
        assert_eq!(*fake.last.lock().unwrap(), Some(("manga-lama", false)), "Manga LaMa on the GPU");
        let st = s.active().unwrap();
        assert_eq!((st.doc.walk().len(), st.history.past_len()), (layers, past + 1), "on the layer, one step");
        assert_eq!(st.history.entries().last().map(String::as_str), Some(LABEL));
        s.undo();
        assert!(red_at(&s, 31, 22));
    }

    #[test]
    fn the_selection_without_strokes_and_the_cpu_without_a_gpu() {
        let fake = Arc::new(FakeInpainter::default());
        let mut s = page(fake.clone(), false);
        s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
        let layers = s.active().unwrap().doc.walk().len();
        s.execute(ID, json!({})).unwrap();
        assert!(!red_at(&s, 31, 22));
        assert_eq!(*fake.last.lock().unwrap(), Some(("manga-lama", true)), "no GPU: still Manga LaMa, on the CPU");
        assert_eq!(s.active().unwrap().doc.walk().len(), layers, "no new layer");
    }

    /// The layer's pixels.
    fn pixels(s: &Session) -> Vec<f32> {
        let st = s.active().unwrap();
        let surf = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        surf.read_region(st.doc.bounds())
    }

    /// A stroke's hole is filled exactly as Clean Selected Area with AI fills a selection of that
    /// shape: the same context reaches the model (the fake fills with the mean of what it sees),
    /// and nothing around the hole (an SFX's outline, hair) is taken or blended. Before, the stroke
    /// read another window, grew over the light around it and feathered and flattened the fill.
    #[test]
    fn a_stroke_fills_as_clean_selected_area_with_ai_fills_its_hole() {
        // Dark hair strands (every 4th column dark, 2 px wide) with a white-outlined black word.
        let draw = |s: &mut Session| {
            s.execute("file.new", json!({"width": 400, "height": 300, "background": "white"})).unwrap();
            s.edit("art", |doc, active| {
                let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
                for x in (0..400).step_by(4) {
                    surf.fill_rect(Rect::new(x, 0, x + 2, 300), &[0.1, 0.1, 0.1, 1.0]);
                }
                surf.fill_rect(Rect::new(161, 141, 241, 181), &[1.0, 1.0, 1.0, 1.0]);
                surf.fill_rect(Rect::new(167, 147, 235, 175), &[0.0, 0.0, 0.0, 1.0]);
                Ok(())
            })
            .unwrap();
        };
        let mut a = page(Arc::new(FakeInpainter::default()), true);
        draw(&mut a);
        let before = pixels(&a);
        a.execute(ID, json!({"strokes": [[[171, 161], [228, 163]]], "size": 23})).unwrap();
        let after = pixels(&a);
        // The hole: what changed (the fake's mean is grey, unlike any pixel of the page).
        let hole: Vec<bool> = before.as_chunks::<4>().0.iter().zip(after.as_chunks::<4>().0).map(|(b, a)| b != a).collect();
        let n = hole.iter().filter(|m| **m).count();
        assert!(n > 60 * 24 && n < 120 * 50, "the stroke's hole, grown a little: {n} px");
        assert!(!hole[131 * 400 + 201] && !hole[191 * 400 + 201], "the hair beyond the outline stays");
        let mut b = page(Arc::new(FakeInpainter::default()), true);
        draw(&mut b);
        b.edit("sel", |doc, _| {
            let mut sel = photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8);
            for (i, _) in hole.iter().enumerate().filter(|(_, m)| **m) {
                let (x, y) = ((i % 400) as i32, (i / 400) as i32);
                sel.fill_rect(Rect::new(x, y, x + 1, y + 1), &[1.0]);
            }
            doc.selection = Some(sel);
            Ok(())
        })
        .unwrap();
        b.execute(crate::ai_inpaint_cmds::ID, json!({"output": "current", "model": "manga-lama", "quality": "best"})).unwrap();
        assert!(pixels(&b) == after, "the same fill, pixel for pixel");
    }

    #[test]
    fn a_gpu_failure_retries_manga_lama_on_the_cpu() {
        let fake = Arc::new(FakeInpainter { gpu_fail: Some("device lost".into()), ..Default::default() });
        let mut s = page(fake.clone(), true);
        let r = s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap();
        assert_eq!(*fake.last.lock().unwrap(), Some(("manga-lama", true)), "never the lite model");
        assert!(!red_at(&s, 31, 22));
        assert_eq!((r["cpu"].clone(), r["fellBack"].clone(), r["model"].clone()), (json!(true), json!(true), json!("manga-lama")), "the reply says so: {r}");
    }

    /// Visoria C.18: Remove with AI (strokes, `paint.remove` with `ai`, and the selection) says
    /// where it ran, with which model and how long it took, as `edit.inpaintAi` does.
    #[test]
    fn a_device_with_2_gb_or_less_removes_with_mi_gan() {
        let low = |gb: f32| {
            let mut s = page(Arc::new(FakeInpainter::default()), true);
            let profile = DeviceProfile { gpu: GpuClass::Integrated, mobile: true, memory_gb: Some(gb), cores: Some(8), ..Default::default() };
            s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: std::sync::Mutex::new(profile), ..Default::default() })));
            s
        };
        let mut s = low(2.0);
        assert!(lite(&s) && model(&s).map(|m| m.id) == Some("migan-512"));
        let q = run_params(&s, &json!({"model": "manga-lama", "quality": "best"})).unwrap();
        assert_eq!((q["model"].as_str(), q["quality"].as_str()), (Some("migan-512"), Some("fast")), "the caller can't pin Manga LaMa here");
        let r = s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap();
        assert!(!red_at(&s, 31, 22), "removed");
        assert_eq!((r["model"].clone(), r["lite"].clone()), (json!("migan-512"), json!(true)), "{r}");
        for gb in [4.0, 8.0] {
            let mut s = low(gb);
            assert!(!lite(&s), "{gb} GB");
            let r = s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap();
            assert_eq!(r["model"], "manga-lama", "{gb} GB keeps Manga LaMa: {r}");
        }
        assert_eq!(model(&Session::new()).map(|m| m.id), Some("manga-lama"), "unknown memory: Manga LaMa");
    }

    #[test]
    fn every_ai_removal_reports_device_model_and_time() {
        let strokes = json!({"strokes": [[[29, 22], [35, 22]]], "size": 14});
        let mut ai = strokes.clone();
        ai["ai"] = json!(true);
        for (gpu, cmd, p) in [(true, ID, strokes.clone()), (false, ID, strokes), (true, "paint.remove", ai)] {
            let mut s = page(Arc::new(FakeInpainter::default()), gpu);
            let r = s.execute(cmd, p).unwrap();
            assert!(r["damage"].is_array() || r["damage"].is_object(), "{cmd}: {r}");
            assert_eq!(r["device"], "test", "{cmd}: {r}");
            assert_eq!((r["model"].clone(), r["lite"].clone()), (json!("manga-lama"), json!(false)), "{cmd}: {r}");
            assert_eq!((r["cpu"].clone(), r["fellBack"].clone()), (json!(!gpu), json!(false)), "{cmd}: {r}");
            assert!(r["ms"].is_u64(), "{cmd}: {r}");
        }
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
        let r = s.execute(ID, json!({})).unwrap();
        for k in ["device", "model", "lite", "cpu", "fellBack", "ms", "region"] {
            assert!(r.get(k).is_some(), "selection: {k} in {r}");
        }
        // The patch-based Remove (no AI) is unchanged.
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        let r = s.execute("paint.remove", json!({"points": [[29, 22], [35, 22]], "size": 14})).unwrap();
        assert!(r.get("device").is_none() && r.get("damage").is_some(), "{r}");
    }

    /// The fake, on a device named `CPU` whatever the request asked for.
    struct ForcedCpu(FakeInpainter);

    impl photocraft_ai_lama::service::Inpainter for ForcedCpu {
        fn model_state(&self) -> photocraft_ai_lama::service::ModelState {
            self.0.model_state()
        }
        fn model_state_of(&self, id: &str) -> photocraft_ai_lama::service::ModelState {
            self.0.model_state_of(id)
        }
        fn device(&self) -> String {
            "CPU".into()
        }
        fn run(&self, req: photocraft_ai_lama::service::Request, hooks: Arc<dyn photocraft_ai_lama::service::Hooks>, done: photocraft_ai_lama::service::Done) {
            self.0.run(req, hooks, done);
        }
    }

    /// `CAPISHOT_AI_DEVICE=cpu` forces the CPU inside the service: the reply said `"device": "CPU"`,
    /// `"cpu": false`.
    #[test]
    fn a_removal_forced_onto_the_cpu_says_so() {
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        s.set_inpainter(Some(Arc::new(ForcedCpu(FakeInpainter::default()))));
        let r = s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap();
        assert_eq!((r["device"].clone(), r["cpu"].clone(), r["fellBack"].clone()), (json!("CPU"), json!(true), json!(false)), "{r}");
    }

    /// The browser threads build reads the stroke's window and hole on the pool (seconds on a
    /// phone: the page froze 2 s in `cmd.run`), then hands them to the page's thread between
    /// frames, where the AI worker is: here the same path natively.
    #[test]
    fn the_window_is_read_off_the_page_thread_and_handed_back_between_frames() {
        use crate::jobs::{JobOutcome, Started};
        crate::jobs::TEST_HAND_OFF.with(|h| h.set(true));
        for (cmd, p) in [(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})), (crate::ai_inpaint_cmds::ID, json!({"output": "current"}))] {
            let mut s = page(Arc::new(FakeInpainter::default()), true);
            if cmd != ID {
                s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
            }
            let past = s.active().unwrap().history.past_len();
            let Started::Job(job) = s.start(cmd, p).unwrap() else { panic!("{cmd}: a background job") };
            // Without a frame's step the job waits: the AI worker is only called on the page's thread.
            for _ in 0..20 {
                assert!(s.poll_jobs().is_empty(), "{cmd}: no result before the hand-off");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(s.has_stepped_jobs());
            let mut done = None;
            for _ in 0..2000 {
                s.step_jobs(&|| true);
                if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                    done = Some(e);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let e = done.unwrap_or_else(|| panic!("{cmd}: the job ended"));
            assert!(matches!(e.outcome, JobOutcome::Done(_)), "{cmd}: {:?}", e.outcome);
            assert!(!red_at(&s, 31, 22), "{cmd}: the blob is gone");
            assert_eq!(s.active().unwrap().history.past_len(), past + 1, "{cmd}: one undo step");
        }
        // A stroke over nothing that can change fails the job, as it failed the command before.
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4})).unwrap();
        let Started::Job(job) = s.start(ID, json!({"strokes": [[[40, 40], [44, 40]]], "size": 4})).unwrap() else { panic!("a job") };
        let mut failed = false;
        for _ in 0..2000 {
            s.step_jobs(&|| true);
            if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                failed = matches!(e.outcome, JobOutcome::Failed(_));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        crate::jobs::TEST_HAND_OFF.with(|h| h.set(false));
        assert!(failed, "the prep's error fails the job");
    }

    /// Capikoma #34: Remove with AI's result is one undo step whatever it paints into (the
    /// layer's pixels, its layer mask, an alpha channel, the Quick Mask) and however the job
    /// runs (on a worker natively, handed back between frames as in the browser): one undo takes
    /// the whole result back and leaves everything else as it was.
    #[test]
    fn remove_ai_is_one_undo_step_on_every_target() {
        use crate::jobs::{JobOutcome, Started};
        // Every surface a target can be: the layer, its mask, the alpha channels, the Quick Mask.
        let surfaces = |s: &Session| {
            let st = s.active().unwrap();
            let (doc, b) = (&st.doc, st.doc.bounds());
            let layer = doc.layer(st.active_layer.unwrap()).unwrap();
            let mut v = vec![layer.surface().unwrap().read_region(b)];
            v.extend(layer.mask.as_ref().map(|m| m.surface.read_region(b)));
            v.extend(doc.channels.iter().map(|c| c.surface.read_region(b)));
            v.extend(doc.quick_mask.as_ref().map(|c| c.surface.read_region(b)));
            v
        };
        for target in [json!("pixels"), json!("mask"), json!({"channel": 0}), json!("quickMask")] {
            for hand_off in [false, true] {
                let mut s = page(Arc::new(FakeInpainter::default()), true);
                // A blob in the target for the fill to change.
                match target.as_str() {
                    Some("mask") => s.execute("layer.layerMask.revealAll", json!({})).map(|_| ()).unwrap(),
                    Some("quickMask") => s.execute("select.editInQuickMaskMode", json!({"on": true})).map(|_| ()).unwrap(),
                    Some(_) => {}
                    None => s.execute("channel.new", json!({"fill": "black"})).map(|_| ()).unwrap(),
                }
                let t = target.clone();
                s.edit("blob", |doc, active| {
                    let (r, px): (Rect, &[f32]) = (Rect::new(28, 18, 36, 28), &[1.0]);
                    match t.as_str() {
                        Some("mask") => doc.layer_mut(active.unwrap()).unwrap().mask.as_mut().unwrap().surface.fill_rect(r, &[0.0]),
                        // A Quick Mask without a selection is all white: the blob is black.
                        Some("quickMask") => doc.quick_mask.as_mut().unwrap().surface.fill_rect(r, &[0.0]),
                        Some(_) => {}
                        None => doc.channels.first_mut().unwrap().surface.fill_rect(r, px),
                    }
                    Ok(())
                })
                .unwrap();
                let (before, past) = (surfaces(&s), s.active().unwrap().history.past_len());
                crate::jobs::TEST_HAND_OFF.with(|h| h.set(hand_off));
                let p = json!({"strokes": [[[29, 22], [35, 22]]], "size": 14, "target": target});
                let outcome = match s.start(ID, p).unwrap() {
                    Started::Done(v) => JobOutcome::Done(v),
                    Started::Job(job) => {
                        let mut end = None;
                        for _ in 0..5000 {
                            s.step_jobs(&|| true);
                            if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                                end = Some(e.outcome);
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        end.unwrap_or_else(|| panic!("{target} hand-off {hand_off}: the job ended"))
                    }
                };
                crate::jobs::TEST_HAND_OFF.with(|h| h.set(false));
                let what = format!("{target}, hand-off {hand_off}");
                assert!(matches!(outcome, JobOutcome::Done(_)), "{what}: {outcome:?}");
                let after = surfaces(&s);
                assert_ne!(after, before, "{what}: the fill landed");
                let st = s.active().unwrap();
                assert_eq!(st.history.past_len(), past + 1, "{what}: one step");
                assert_eq!(st.history.entries().last().map(String::as_str), Some(LABEL), "{what}");
                assert!(s.undo());
                assert_eq!(surfaces(&s), before, "{what}: one undo takes it all back");
                assert!(s.redo());
                assert_eq!(surfaces(&s), after, "{what}: one redo puts it all back");
            }
        }
    }

    /// The single-threaded browser build reads the stroke's window and hole a part per step
    /// between frames (one go froze the page 4-5 s on a phone): with no time left, each step
    /// does one part, and the result is the same as in one go.
    #[test]
    fn the_window_is_read_a_part_per_step_between_frames() {
        use crate::jobs::{JobOutcome, Started};
        let stroke = json!({"strokes": [[[29, 22], [35, 22]]], "size": 14});
        let mut one_go = page(Arc::new(FakeInpainter::default()), true);
        one_go.execute(ID, stroke.clone()).unwrap();
        crate::jobs::TEST_STEPPED_PREP.with(|h| h.set(true));
        for (cmd, p) in [(ID, stroke), (crate::ai_inpaint_cmds::ID, json!({"output": "current"}))] {
            let mut s = page(Arc::new(FakeInpainter::default()), true);
            if cmd != ID {
                s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
            }
            let past = s.active().unwrap().history.past_len();
            let Started::Job(job) = s.start(cmd, p).unwrap() else { panic!("{cmd}: a background job") };
            assert!(s.has_stepped_jobs(), "{cmd}: stepped");
            assert!(red_at(&s, 31, 22), "{cmd}: nothing read in the command");
            let (mut steps, mut done) = (0, None);
            for _ in 0..2000 {
                s.step_jobs(&|| false);
                steps += 1;
                if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                    done = Some(e);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let e = done.unwrap_or_else(|| panic!("{cmd}: the job ended"));
            assert!(matches!(e.outcome, JobOutcome::Done(_)), "{cmd}: {:?}", e.outcome);
            assert!(steps >= 2, "{cmd}: in parts ({steps} steps)");
            assert!(!red_at(&s, 31, 22), "{cmd}: the blob is gone");
            assert_eq!(s.active().unwrap().history.past_len(), past + 1, "{cmd}: one undo step");
            if cmd == ID {
                assert!(pixels(&s) == pixels(&one_go), "the same fill as in one go");
            }
        }
        // An error in a part fails the job; Esc stops it between parts.
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4})).unwrap();
        let Started::Job(job) = s.start(ID, json!({"strokes": [[[40, 40], [44, 40]]], "size": 4})).unwrap() else { panic!("a job") };
        let mut failed = false;
        for _ in 0..2000 {
            s.step_jobs(&|| false);
            if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                failed = matches!(e.outcome, JobOutcome::Failed(_));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(failed, "the part's error fails the job");
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        let Started::Job(job) = s.start(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap() else { panic!("a job") };
        s.step_jobs(&|| false);
        s.cancel_job(job);
        for _ in 0..50 {
            s.step_jobs(&|| false);
            let _ = s.poll_jobs();
        }
        crate::jobs::TEST_STEPPED_PREP.with(|h| h.set(false));
        assert!(!s.has_stepped_jobs(), "cancelled: no step left");
        assert!(red_at(&s, 31, 22), "cancelled: unchanged");
    }

    /// Paint mask, then run: a mask with an erasing stroke keeps what it erased out of the hole
    /// when read a part per step, with Manga LaMa and with MI-GAN on 2 GB: the same fill as in
    /// one go, one undo step.
    #[test]
    fn a_mask_with_an_eraser_is_read_in_parts_on_every_model() {
        use crate::jobs::{JobOutcome, Started};
        let mask = json!({"strokes": [
            {"points": [[29, 22], [35, 22]], "size": 14},
            {"points": [[34, 12], [34, 32]], "size": 2, "erase": true},
        ]});
        for (gb, name) in [(8.0, "manga-lama"), (2.0, "migan-512")] {
            let on = |gb: f32| {
                let mut s = page(Arc::new(FakeInpainter::default()), true);
                let profile = DeviceProfile { gpu: GpuClass::Integrated, mobile: true, memory_gb: Some(gb), cores: Some(8), ..Default::default() };
                s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: std::sync::Mutex::new(profile), ..Default::default() })));
                s
            };
            let mut one_go = on(gb);
            assert_eq!(one_go.execute(ID, mask.clone()).unwrap()["model"], name);
            assert!(red_at(&one_go, 34, 22) && !red_at(&one_go, 29, 22), "{name}: erased stays, painted goes");
            crate::jobs::TEST_STEPPED_PREP.with(|h| h.set(true));
            let mut s = on(gb);
            let past = s.active().unwrap().history.past_len();
            let Started::Job(job) = s.start(ID, mask.clone()).unwrap() else { panic!("a background job") };
            let mut done = None;
            for _ in 0..2000 {
                s.step_jobs(&|| false);
                if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == job) {
                    done = Some(e);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            crate::jobs::TEST_STEPPED_PREP.with(|h| h.set(false));
            let e = done.unwrap_or_else(|| panic!("{name}: the job ended"));
            assert!(matches!(e.outcome, JobOutcome::Done(_)), "{name}: {:?}", e.outcome);
            assert_eq!(s.active().unwrap().history.past_len(), past + 1, "{name}: one undo step");
            assert!(pixels(&s) == pixels(&one_go), "{name}: the same fill as in one go");
        }
    }

    #[test]
    fn a_missing_model_is_an_error_unless_downloads_are_allowed() {
        let fake = Arc::new(FakeInpainter { state: Some(photocraft_ai_lama::service::ModelState::NeedsDownload { bytes: 100 << 20 }), ..Default::default() });
        let mut s = page(fake, true);
        let past = s.active().unwrap().history.past_len();
        let e = s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap_err().to_string();
        assert!(e.contains(crate::ai_models_cmds::NOT_DOWNLOADED), "{e}");
        assert_eq!(s.active().unwrap().history.past_len(), past);
        assert!(s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14, "allowDownload": true})).is_ok());
    }

    /// #21: a hole larger than the 512 px working side is filled at full resolution, in
    /// overlapping tiles of at most 512 a side, by Remove with AI (strokes and selection) and
    /// `paint.remove` with `ai`; `maxSide` > 0 downscales as before, and `edit.inpaintAi` keeps
    /// downscaling unless asked for `maxSide: 0`.
    #[test]
    fn a_large_hole_is_filled_at_full_resolution_in_overlapping_tiles() {
        let runs = |cmd: &str, p: Value, select: bool| {
            let fake = Arc::new(FakeInpainter::default());
            let mut s = page(fake.clone(), true);
            s.execute("file.new", json!({"width": 1400, "height": 1000, "background": "white"})).unwrap();
            s.edit("tone", |doc, active| {
                let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
                for x in (0..1400).step_by(8) {
                    surf.fill_rect(Rect::new(x, 0, x + 3, 1000), &[0.2, 0.2, 0.2, 1.0]);
                }
                Ok(())
            })
            .unwrap();
            if select {
                s.execute("select.rect", json!({"x": 200, "y": 150, "width": 1000, "height": 700})).unwrap();
            }
            let r = s.execute(cmd, p).unwrap();
            assert_eq!(r["model"], "manga-lama", "{cmd}: {r}");
            let sizes = fake.sizes.lock().unwrap().clone();
            assert!(grey_at(&s, 700, 500) > 0.2 && grey_at(&s, 700, 500) < 1.0, "{cmd}: filled");
            sizes
        };
        let full = |sizes: &[(usize, usize)]| sizes.len() > 4 && sizes.iter().all(|(w, h)| *w <= 512 && *h <= 512);
        let stroke = json!({"strokes": [[[300, 500], [1100, 500]]], "size": 300});
        let mut capped = stroke.clone();
        capped["maxSide"] = json!(512);
        let mut ai = stroke.clone();
        ai["ai"] = json!(true);
        assert!(full(&runs(ID, stroke, false)), "strokes");
        assert!(full(&runs(ID, json!({}), true)), "the selection");
        assert!(full(&runs("paint.remove", ai, false)), "paint.remove with ai");
        assert_eq!(runs(ID, capped, false).len(), 1, "maxSide 512: one downscaled tile");
        assert_eq!(runs(ID, json!({"maxSide": 256}), true), vec![(256, 184)], "downscaled to maxSide");
        assert_eq!(runs(crate::ai_inpaint_cmds::ID, json!({"output": "current"}), true).len(), 1, "Clean Selected Area with AI: unchanged");
        assert!(full(&runs(crate::ai_inpaint_cmds::ID, json!({"output": "current", "maxSide": 0}), true)), "unless maxSide 0");
        let mut s = page(Arc::new(FakeInpainter::default()), true);
        for bad in [json!(32), json!(-1), json!(100), json!("full")] {
            assert!(s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14, "maxSide": bad})).is_err(), "{bad}");
        }
    }
}
