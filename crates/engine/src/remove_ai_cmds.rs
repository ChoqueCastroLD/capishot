//! `edit.removeAi`: the Remove with AI tool. What the user painted (`strokes`, as the Remove
//! tool's) or, without strokes, the selection, is erased and filled as Clean Selected Area with AI
//! (`edit.inpaintAi`) fills a selection, with fixed options: Manga LaMa (never the lite model) at
//! best quality, f32, the default working side and context margin. The AI Cleaning Options dialog
//! doesn't apply. The GPU runs it when there is one, the CPU otherwise or when the GPU fails
//! mid-run (still Manga LaMa). A stroke's hole grows first so nothing of what it touched is left for the
//! model to rebuild from (`retouch_cmds::ink_mask`). One undo step, "Remove with AI", on the
//! target layer. A background job, cancellable.

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const ID: &str = "edit.removeAi";
/// The undo step and the job.
pub const LABEL: &str = "Remove with AI";
/// What the AI Cleaning Options dialog would set, overridden on every run: Remove with AI is
/// fixed (Manga LaMa at its best, the GPU when there is one, the working side and context
/// margin `edit.inpaintAi` uses by default) and never follows that dialog.
const FIXED: [&str; 7] = ["model", "quality", "device", "maxSide", "margin", "threads", "keepLoaded"];

/// The inpainting params of a run: Clean Selected Area with AI's (`edit.inpaintAi`) with Manga LaMa
/// pinned at best quality and every other option at its default, whatever the caller sent.
pub fn lama_params(p: &Value) -> Result<serde_json::Map<String, Value>> {
    let mut q = match p {
        Value::Object(m) => m.clone(),
        Value::Null => serde_json::Map::new(),
        _ => return Err(EngineError::BadParams { cmd: ID.into(), msg: "params must be a JSON object".into() }),
    };
    for k in FIXED.iter().chain(&["ai", "output", "newLayer"]) {
        q.remove(*k);
    }
    q.insert("model".into(), json!(photocraft_ai::catalog::LAMA_ID));
    q.insert("quality".into(), json!("best"));
    Ok(q)
}

/// The model state the run needs (the UI's first-use download).
pub fn lama_state(s: &Session) -> Option<photocraft_ai_lama::service::ModelState> {
    let p = Value::Object(lama_params(&json!({})).ok()?);
    crate::ai_inpaint_cmds::model_state(s, &p).map(|(_, st)| st)
}

fn remove_ai(s: &mut Session, p: &Value) -> Result<Value> {
    let mut q = lama_params(p)?;
    if q.contains_key("strokes") || q.contains_key("points") {
        q.insert("ai".into(), json!(true));
        q.insert("newLayer".into(), json!(false));
        q.entry("closeLoops").or_insert(json!(true));
        return crate::retouch_cmds::remove::remove_ink_labelled(s, &Value::Object(q), LABEL);
    }
    q.insert("output".into(), json!("current"));
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
        params: r#"{"strokes"?: [[[x,y,pressure?],…],…] or "points"?: [[x,y],…] (what was painted, with "size": px, as paint.remove; a stroke around an object removes what it encloses), else the selection, "sampleAllLayers"?: bool, "layer"?: id, "allowDownload"?: bool (fetch Manga LaMa, about 100 MB, when it isn't on this device)} -> {"damage"|"region", "device", "model", "lite", "cpu", "fellBack", "ms", …}. Erases it and fills it as edit.inpaintAi fills a selection, with fixed options (Manga LaMa at best quality, its default working side and margin; model, quality, device, maxSide, margin, threads and keepLoaded params are ignored), on the target layer, as one undo step ("Remove with AI"). A background job; the GPU's failure retries on the CPU."#,
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
        let q = lama_params(&dialog).unwrap();
        assert_eq!((q["model"].as_str(), q["quality"].as_str()), (Some("manga-lama"), Some("best")));
        for k in ["device", "maxSide", "margin", "threads", "keepLoaded", "ai"] {
            assert!(!q.contains_key(k), "{k} is fixed");
        }
        assert!(q.contains_key("strokes"), "the painted strokes go through");
        assert_eq!(lama_params(&json!({})).unwrap(), q.into_iter().filter(|(k, _)| k != "strokes").collect());
        assert!(lama_params(&json!([1])).is_err());
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
}
