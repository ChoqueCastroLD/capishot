use super::*;
use crate::package::tests::{png, raw_zip};

const CSS: &str = ":root {\n  --base: studio;\n  --accent: #ff8800;\n}\n";

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

#[test]
fn themes_are_created_duplicated_renamed_deleted_and_remembered() {
    let mut s = Session::new();
    let a = run(&mut s, "theme.custom.save", json!({"name": "Tinta", "css": CSS}))["id"].as_str().unwrap().to_string();
    assert_eq!(s.prefs().interface.custom_theme, a, "a new theme is used");
    let b = run(&mut s, "theme.custom.duplicate", json!({"id": a}))["id"].as_str().unwrap().to_string();
    assert_ne!(a, b);
    let names: Vec<String> = s.prefs().interface.custom_themes.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, ["Tinta", "Tinta 2"]);
    assert_eq!(run(&mut s, "theme.custom.rename", json!({"id": b, "name": "tinta"}))["name"], "tinta 2", "names stay distinct");
    run(&mut s, "theme.custom.save", json!({"id": b, "css": ":root { --accent: #00ff00; }", "use": true}));
    assert_eq!(s.prefs().interface.custom_theme, b);
    // Persisted with the preferences and restored at the next start.
    let saved = s.prefs_to_json();
    let mut next = Session::new();
    next.load_prefs_json(&saved).unwrap();
    assert_eq!(next.prefs().interface.custom_theme, b);
    assert_eq!(next.prefs().interface.active_custom_theme().map(|t| t.css.as_str()), Some(":root { --accent: #00ff00; }"));
    // Choosing a built-in theme leaves the custom one.
    run(&mut next, "prefs.set", json!({"path": "interface.theme", "value": "classic"}));
    assert_eq!(next.prefs().interface.custom_theme, "");
    run(&mut s, "theme.custom.delete", json!({"id": b}));
    assert_eq!(s.prefs().interface.custom_theme, "", "deleting the theme in use goes back to the built-in");
    assert_eq!(run(&mut s, "theme.custom.list", json!({}))["themes"].as_array().unwrap().len(), 1);
}

#[test]
fn a_theme_package_round_trips_with_its_icon_and_metadata() {
    let mut s = Session::new();
    let icon = photocraft_paint::tile::b64_encode(&png(256, 256));
    let meta = json!({"title": "Tinta", "author": "Equipo", "tags": ["dark"], "license": "CC0-1.0", "homepage": "https://example.org"});
    let id = run(&mut s, "theme.custom.save", json!({"name": "Tinta", "css": CSS, "meta": meta, "icon": icon}))["id"].as_str().unwrap().to_string();
    let out = run(&mut s, "theme.package.export", json!({"id": id}));
    assert!(out["file"].as_str().unwrap().ends_with(".theme.capishot"));
    let mut other = Session::new();
    let r = run(&mut other, "theme.package.import", json!({"data": out["data"]}));
    let t = other.prefs().interface.active_custom_theme().cloned().unwrap();
    assert_eq!(r["id"], t.id.as_str());
    assert_eq!((t.name.as_str(), t.css.as_str()), ("Tinta", CSS));
    assert_eq!(t.meta.author, "Equipo");
    assert_eq!(t.meta.homepage, "https://example.org");
    assert_eq!(t.icon, icon);
}

#[test]
fn bad_theme_packages_and_params_are_errors() {
    let mut s = Session::new();
    for (id, p) in [
        ("theme.custom.save", json!({})),
        ("theme.custom.save", json!({"css": CSS})),
        ("theme.custom.save", json!({"name": "x", "css": "a".repeat(MAX_CSS + 1)})),
        ("theme.custom.save", json!({"name": "x", "css": CSS, "icon": "%%"})),
        ("theme.custom.save", json!({"name": "x", "css": CSS, "icon": photocraft_paint::tile::b64_encode(&png(300, 300))})),
        ("theme.custom.save", json!({"id": "nope", "css": CSS})),
        ("theme.custom.delete", json!({"id": "nope"})),
        ("theme.custom.use", json!({"id": "nope"})),
        ("theme.custom.rename", json!({"id": "nope", "name": "x"})),
        ("theme.package.import", json!({"data": "not base64!"})),
        ("theme.package.import", json!({"data": photocraft_paint::tile::b64_encode(b"PK\x05\x06")})),
    ] {
        assert!(s.execute(id, p.clone()).is_err(), "{id} {p}");
    }
    // No theme.css; an icon that isn't a PNG; a too-large icon.
    let m = serde_json::to_vec(&json!({"format": FORMAT, "version": 1})).unwrap();
    assert!(import(&raw_zip(&[("manifest.json", &m, 0)])).is_err());
    assert!(import(&raw_zip(&[("manifest.json", &m, 0), ("theme.css", CSS.as_bytes(), 0), ("icon.png", b"GIF89a", 0)])).is_err());
    let big = png(512, 512);
    assert!(import(&raw_zip(&[("manifest.json", &m, 0), ("theme.css", CSS.as_bytes(), 0), ("icon.png", &big, 0)])).is_err());
    // Extra files are left out with a warning.
    let p = import(&raw_zip(&[("manifest.json", &m, 0), ("theme.css", CSS.as_bytes(), 0), ("evil.js", b"alert(1)", 0)])).unwrap();
    assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
}
