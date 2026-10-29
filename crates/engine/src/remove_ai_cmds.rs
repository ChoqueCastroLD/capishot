//! `edit.removeAi`: the Remove with AI tool. What the user painted (`strokes`, as the Remove
//! tool's) or, without strokes, the selection, is erased and filled by Manga LaMa, always, at its
//! best: no lite model, the crops at full resolution up to what the device can hold (1024 px a
//! side on a GPU, 768 on a CPU, 512, LaMa's own working size, on phones and machines of 4 GB or
//! less, one crop at a time, so a run stays within a phone's memory cap), with a wide context
//! margin. The GPU runs it when there is one, the CPU otherwise or
//! when the GPU fails mid-run (still Manga LaMa). A stroke's hole grows first so nothing of what
//! it touched is left for the model to rebuild from, and the model doesn't see the letters around
//! it (`retouch_cmds::ink_mask`). One undo step, "Remove with AI", on the target
//! layer. A background job, cancellable.

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const ID: &str = "edit.removeAi";
/// The undo step and the job.
pub const LABEL: &str = "Remove with AI";
/// Context around each masked blob (px): the largest that keeps the crops at full resolution.
const MARGIN: u64 = 128;

/// The inpainting params every run uses: Manga LaMa pinned, best quality, the largest crops the
/// device can hold, the widest margin.
pub fn lama_params(s: &Session, p: &Value) -> Result<serde_json::Map<String, Value>> {
    let mut q = match p {
        Value::Object(m) => m.clone(),
        Value::Null => serde_json::Map::new(),
        _ => return Err(EngineError::BadParams { cmd: ID.into(), msg: "params must be a JSON object".into() }),
    };
    for k in ["model", "quality", "maxSide", "margin", "ai", "output", "newLayer"] {
        q.remove(k);
    }
    q.insert("model".into(), json!(photocraft_ai::catalog::LAMA_ID));
    q.insert("quality".into(), json!("best"));
    let run = crate::ai_cmds::run_choice(s, &Value::Object(q.clone()), ID)?;
    // Phones and machines with little memory: LaMa's own working size, so a run stays bounded
    // within the page's memory cap (one crop of at most 512 x 512 at a time).
    let bounded = run.choice.small_tiles || run.profile.mobile || run.profile.memory_gb.is_some_and(|m| m <= 4.0);
    let side = if bounded { 512 } else if run.choice.cpu { 768 } else { 1024 };
    q.insert("maxSide".into(), json!(side));
    q.insert("margin".into(), json!(MARGIN));
    Ok(q)
}

/// The model state the run needs (the UI's first-use download).
pub fn lama_state(s: &Session) -> Option<photocraft_ai_lama::service::ModelState> {
    let p = Value::Object(lama_params(s, &json!({})).ok()?);
    crate::ai_inpaint_cmds::model_state(s, &p).map(|(_, st)| st)
}

fn remove_ai(s: &mut Session, p: &Value) -> Result<Value> {
    let mut q = lama_params(s, p)?;
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
        params: r#"{"strokes"?: [[[x,y,pressure?],…],…] or "points"?: [[x,y],…] (what was painted, with "size": px, as paint.remove; a stroke around an object removes what it encloses), else the selection, "sampleAllLayers"?: bool, "layer"?: id, "allowDownload"?: bool (fetch Manga LaMa, about 100 MB, when it isn't on this device), "device"?: "auto"|"gpu"|"cpu"} -> {"damage"|"region", …}. Erases it and fills it with Manga LaMa at its best (a stroke's hole grows over the ink it touches, nearby letters are hidden from the model, a fill meeting flat paper takes its tone, the edge feathers in; never the lite model; crops at full resolution up to 1024 px on a GPU, 768 on a CPU, 512 on phones; 128 px of context), on the target layer, as one undo step ("Remove with AI"). A background job; the GPU's failure retries on the CPU."#,
        enabled,
        run: remove_ai,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_run_is_manga_lama_at_its_best() {
        let s = Session::new();
        let q = lama_params(&s, &json!({"model": "migan-512", "quality": "fast", "maxSide": 64, "strokes": [[[1, 2]]]})).unwrap();
        assert_eq!(q["model"], "manga-lama");
        assert_eq!(q["quality"], "best");
        assert!([512, 768, 1024].contains(&q["maxSide"].as_u64().unwrap()), "{}", q["maxSide"]);
        assert_eq!(q["margin"], 128);
        assert!(q.contains_key("strokes"), "the painted strokes go through");
        assert!(lama_params(&s, &json!([1])).is_err());
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
            assert_eq!(grey_at(&s, x, y), 1.0, "the word is gone, its paper white, at {x},{y}");
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

    #[test]
    fn a_phone_runs_one_512_crop_at_a_time() {
        let mut s = Session::new();
        let phone = DeviceProfile { gpu: GpuClass::Integrated, memory_gb: Some(4.0), cores: Some(8), mobile: true, ..Default::default() };
        s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: std::sync::Mutex::new(phone), ..Default::default() })));
        assert_eq!(lama_params(&s, &json!({})).unwrap()["maxSide"], 512);
        let desk = DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(16.0), cores: Some(8), ..Default::default() };
        s.set_ai_upscaler(Some(Arc::new(FakeUpscaler { profile: std::sync::Mutex::new(desk), ..Default::default() })));
        assert_eq!(lama_params(&s, &json!({})).unwrap()["maxSide"], 1024);
    }

    #[test]
    fn a_gpu_failure_retries_manga_lama_on_the_cpu() {
        let fake = Arc::new(FakeInpainter { gpu_fail: Some("device lost".into()), ..Default::default() });
        let mut s = page(fake.clone(), true);
        s.execute(ID, json!({"strokes": [[[29, 22], [35, 22]]], "size": 14})).unwrap();
        assert_eq!(*fake.last.lock().unwrap(), Some(("manga-lama", true)), "never the lite model");
        assert!(!red_at(&s, 31, 22));
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
