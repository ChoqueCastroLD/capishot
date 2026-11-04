use serde_json::json;

use crate::Session;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 400, "height": 300, "background": "transparent"})).unwrap();
    s
}

fn add(s: &mut Session, p: serde_json::Value) -> u64 {
    s.execute("type.create", p).unwrap()["layer"].as_u64().unwrap()
}

/// The layout of [`scene`] with the bundled fonts, bit for bit. The browser e2e
/// (`packaging/web-threads/e2e/layout-digest.mjs`) builds the same scene in the web build and
/// checks it against this value: change both together if layout changes on purpose.
const GOLDEN: &str = "df1cc080d8aad5623de886e71a07fa494bfbf2a18dcc7a51b087f3647d0e739a";

/// Point text, and paragraph text that wraps, justifies, kerns optically (`ln`) and slants
/// (faux italic: `tan`), in both bundled families. The e2e sends these same calls.
pub fn scene() -> Vec<serde_json::Value> {
    vec![
        json!({"x": 10, "y": 60, "text": "AVATAR wave, fi ffi\nSegunda línea — ¿sí?", "size": 24, "font": "Inter"}),
        json!({"box": [20, 90, 180, 200], "text": "¡Qué haces aquí! Esto es un globo de diálogo bastante largo, que debe partirse en varias líneas.", "size": 15, "font": "Inter", "align": "justify", "kerning": "optical", "fauxItalic": true, "tracking": 25}),
        json!({"box": [220, 20, 160, 260], "text": "Monospace 0123 // wrap wrap wrap wrap", "size": 13, "font": "JetBrains Mono", "align": "center", "horizontalScale": 90}),
    ]
}

#[test]
fn golden_scene() {
    let mut s = session();
    for p in scene() {
        add(&mut s, p);
    }
    let all = s.execute("type.layoutDigest", json!({})).unwrap();
    assert_eq!(all["layers"].as_array().unwrap().len(), 3);
    for l in all["layers"].as_array().unwrap() {
        assert!(l["lines"].as_u64().unwrap() >= 2, "{l}");
    }
    eprintln!("golden scene digest: {}", all["digest"]);
    assert_eq!(all["digest"], GOLDEN, "layout changed: update GOLDEN and the browser e2e together");
}

#[test]
fn digest_is_stable_and_follows_the_layout() {
    let mut s = session();
    let a = add(&mut s, json!({"x": 10, "y": 60, "text": "AVATAR wave, fi ffi\nSegunda línea — ¿sí?", "size": 24, "font": "Inter"}));
    let one = s.execute("type.layoutDigest", json!({"layer": a})).unwrap();
    let digest = one["digest"].as_str().unwrap().to_string();
    assert_eq!(digest.len(), 64);
    assert_eq!(one["lines"], json!(2));
    assert!(one["glyphs"].as_u64().unwrap() > 20);
    assert!(!one["fonts"].as_array().unwrap().is_empty());
    assert!(one["fonts"][0].get("sha256").is_none(), "hashes are opt-in");
    // Same again: no state leaks between layouts.
    assert_eq!(s.execute("type.layoutDigest", json!({"layer": a})).unwrap()["digest"], one["digest"]);
    let hashed = s.execute("type.layoutDigest", json!({"layer": a, "fontHashes": true})).unwrap();
    assert_eq!(hashed["fonts"][0]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(hashed["digest"], one["digest"]);
    // Any change to the layout changes the digest.
    s.execute("type.edit", json!({"layer": a, "runs": [{"start": 0, "end": 1, "tracking": 200}]})).unwrap();
    assert_ne!(s.execute("type.layoutDigest", json!({"layer": a})).unwrap()["digest"], one["digest"]);
    // Without `layer`: every type layer, plus one digest of them all.
    let b = add(&mut s, json!({"x": 10, "y": 200, "text": "Box", "size": 12}));
    let all = s.execute("type.layoutDigest", json!({})).unwrap();
    let layers = all["layers"].as_array().unwrap();
    let mut got: Vec<u64> = layers.iter().map(|l| l["layer"].as_u64().unwrap()).collect();
    got.sort_unstable();
    let mut want = vec![a, b];
    want.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(all["digest"].as_str().unwrap().len(), 64);
}

/// Bad params are errors, never panics.
#[test]
fn bad_params_fail_gracefully() {
    let mut s = Session::new();
    assert!(s.execute("type.layoutDigest", json!({})).is_err(), "no document");
    let mut s = session();
    let pixel = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
    for p in [json!([1]), json!("x"), json!({"layer": "a"}), json!({"layer": -1}), json!({"layer": 999_999}), json!({"layer": pixel}), json!({"fontHashes": 3}), json!({"nope": 1})] {
        assert!(s.execute("type.layoutDigest", p.clone()).is_err(), "{p}");
    }
    // An empty document has no type layers.
    assert_eq!(s.execute("type.layoutDigest", json!(null)).unwrap()["layers"], json!([]));
}
