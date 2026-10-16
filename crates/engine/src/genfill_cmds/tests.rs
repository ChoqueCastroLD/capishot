use std::sync::Arc;

use photocraft_ai_sd::service::{FakeGenFill, ModelState};
use photocraft_geom::Rect;
use serde_json::json;

use super::*;

/// A colour page with a red blob, selected (20..40, 10..30).
fn page() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": 8, "background": "white"})).unwrap();
    s.edit("paint", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        surf.fill_rect(Rect::new(0, 0, 64, 24), &[0.2, 0.4, 0.9, 1.0]);
        surf.fill_rect(Rect::new(24, 14, 36, 26), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 20, "height": 20})).unwrap();
    s
}

fn with_fake(mut s: Session, fake: FakeGenFill) -> (Session, Arc<FakeGenFill>) {
    let f = Arc::new(fake);
    s.set_genfill(Some(f.clone()));
    (s, f)
}

#[test]
fn unavailable_without_a_service() {
    let mut s = page();
    assert!(!s.is_enabled(ID));
    let err = s.execute(ID, json!({"prompt": "a cat"})).unwrap_err().to_string();
    assert!(err.contains(UNAVAILABLE), "{err}");
}

#[test]
fn fills_on_a_new_masked_layer_and_swaps_variants() {
    let (mut s, fake) = with_fake(page(), FakeGenFill::default());
    assert!(s.is_enabled(ID));
    let past = s.active().unwrap().history.past_len();
    let r = s.execute(ID, json!({"prompt": "un gato negro", "variants": 2, "seed": 5})).unwrap();
    assert_eq!(r["variants"], 2);
    assert_eq!(r["english"], "black cat");
    let req = fake.last.lock().unwrap().clone().unwrap();
    assert_eq!((req.variants, req.seed, req.prompt.as_str()), (2, 5, "black cat"));
    assert_eq!(req.width % 64, 0);
    let st = s.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1, "one undo step");
    let layer = LayerId(r["layer"].as_u64().unwrap());
    let l = st.doc.layer(layer).unwrap();
    assert!(l.mask.is_some(), "masked by the selection");
    // Variant 0 is grey 0.2 inside the selection.
    let px = l.surface().unwrap().pixel(30, 20);
    assert!((px[0] - 0.2).abs() < 0.02, "{px:?}");
    s.execute(VARIANT_ID, json!({"index": 1})).unwrap();
    let px = s.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().pixel(30, 20);
    assert!((px[0] - 0.4).abs() < 0.02, "{px:?}");
    assert_eq!(s.genfill_variants(layer).unwrap().selected, 1);
    assert!(s.execute(VARIANT_ID, json!({"index": 7})).is_err());
}

#[test]
fn refused_prompts_and_bad_params_change_nothing() {
    let (mut s, fake) = with_fake(page(), FakeGenFill::default());
    let past = s.active().unwrap().history.past_len();
    for p in [
        json!({"prompt": "n1ña d3snuda"}),
        json!({"prompt": 5}),
        json!({"variants": 0}),
        json!({"variants": 9}),
        json!({"strength": -1}),
        json!({"quality": "ultra-max"}),
        json!({"side": 500}),
        json!({"seed": -3}),
        json!([1, 2]),
    ] {
        assert!(s.execute(ID, p.clone()).is_err(), "{p}");
    }
    assert!(fake.last.lock().unwrap().is_none());
    assert_eq!(s.active().unwrap().history.past_len(), past);
}

#[test]
fn missing_model_names_the_download() {
    let (mut s, _) = with_fake(page(), FakeGenFill { state: Some(ModelState::NeedsDownload { bytes: 2_000_000_000 }), ..Default::default() });
    let err = s.execute(ID, json!({})).unwrap_err().to_string();
    assert!(err.contains(crate::ai_models_cmds::NOT_DOWNLOADED), "{err}");
}

#[test]
fn grayscale_pages_get_a_monochrome_prompt() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": 8, "background": "white"})).unwrap();
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
    let (mut s, fake) = with_fake(s, FakeGenFill::default());
    let r = s.execute(ID, json!({"prompt": ""})).unwrap();
    assert_eq!(r["english"], "monochrome, greyscale");
    assert!(fake.last.lock().unwrap().as_ref().unwrap().prompt.contains("monochrome"));
}
