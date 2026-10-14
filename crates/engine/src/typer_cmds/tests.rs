use super::*;
use crate::EngineError;

const SCRIPT: &str = "## Chapter 12\nPage 1\nREG: Where are we?\nSFX: BOOM\n// It's huge!\n\nPage 2\nHello again.\nRead page 5 now.";

fn library() -> Value {
    json!({
        "folders": [{"id": "f1", "name": "Series", "parentId": null, "order": 0}],
        "styles": [
            {"id": "reg", "name": "Regular", "folder": "f1", "prefixes": ["REG:"], "textProps": photocraft_typer::library::default_text_props()},
            {"id": "sfx", "name": "SFX", "folder": "f1", "prefixes": ["SFX:"], "sizePresets": [30, 40],
             "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"fontName": "Komika", "size": 30}}]}, "typeUnit": "pixelsUnit"},
             "stroke": {"enabled": true, "size": 4, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}}}
        ],
        "includesSettings": true,
        "defaultStyleId": "reg"
    })
}

fn loaded() -> Session {
    let mut s = Session::new();
    s.execute("typer.library.import", json!({"json": library(), "now": 1})).unwrap();
    s.execute("typer.script.set", json!({"text": SCRIPT})).unwrap();
    s
}

#[test]
fn script_and_cursor_drive_the_loop() {
    let mut s = loaded();
    let st = s.execute("typer.state", json!({})).unwrap();
    assert_eq!(st["text"], "Where are we?");
    assert_eq!(st["number"], 1);
    assert_eq!(st["page"], 1);
    assert_eq!(st["usableLines"], 5);
    let r = s.execute("typer.line.next", json!({})).unwrap();
    assert_eq!((r["moved"].as_bool(), r["text"].as_str(), r["style"].as_str()), (Some(true), Some("BOOM"), Some("sfx")));
    let r = s.execute("typer.line.next", json!({"add": true})).unwrap();
    assert_eq!((r["text"].as_str(), r["style"].as_str()), (Some("It's huge!"), Some("sfx")), "// continues SFX");
    let r = s.execute("typer.page.next", json!({})).unwrap();
    assert_eq!((r["text"].as_str(), r["page"].as_u64(), r["number"].as_u64()), (Some("Hello again."), Some(2), Some(1)));
    assert_eq!(r["style"], "reg", "untagged line takes the default style");
    let r = s.execute("typer.line.next", json!({})).unwrap();
    assert_eq!(r["text"], "Read page 5 now.", "dialogue mentioning a page is not a marker");
    assert_eq!(s.execute("typer.line.next", json!({})).unwrap()["moved"], false);
    let r = s.execute("typer.page.previous", json!({})).unwrap();
    assert_eq!(r["text"], "Where are we?");
    let r = s.execute("typer.line.set", json!({"index": 3})).unwrap();
    assert_eq!(r["text"], "BOOM");
    assert!(matches!(s.execute("typer.line.set", json!({"index": 99})), Err(EngineError::BadParams { .. })));
    let lines = s.execute("typer.parseScript", json!({"text": "Page 3\nx"})).unwrap();
    assert_eq!(lines["lines"][0]["page"], 3);
    assert_eq!(s.execute("typer.state", json!({})).unwrap()["text"], "BOOM", "parseScript doesn't change the script");
    // Not document data: no history, works without a document.
    assert!(s.active().is_none());
}

#[test]
fn page_files_follow_the_cursor() {
    let mut s = loaded();
    s.execute("typer.images.set", json!({"images": ["ch12_002.psd", "ch12_001.psd"]})).unwrap();
    assert_eq!(s.execute("typer.state", json!({})).unwrap()["image"], "ch12_001.psd");
    s.execute("typer.page.next", json!({})).unwrap();
    assert_eq!(s.execute("typer.state", json!({})).unwrap()["image"], "ch12_002.psd");
    // The last line of page 1 now blocks a paste advance.
    s.execute("typer.line.set", json!({"index": 4})).unwrap();
    assert_eq!(s.execute("typer.line.next", json!({"add": true})).unwrap()["moved"], false);
    let r = s.execute("typer.images.opened", json!({"image": "ch12_002.psd"})).unwrap();
    assert_eq!((r["lastOpenedImage"].as_str(), r["pageFiles"].as_u64()), (Some("ch12_002.psd"), Some(2)));
    assert!(s.execute("typer.images.opened", json!({"image": "other.psd"})).is_err(), "only a page file");
    assert!(s.execute("typer.images.opened", json!({"image": 3})).is_err());
    assert!(s.execute("typer.images.opened", json!({})).is_err());
    assert_eq!(s.execute("typer.images.opened", json!({"image": null})).unwrap()["lastOpenedImage"], Value::Null);
    s.execute("typer.images.opened", json!({"image": "ch12_001.psd"})).unwrap();
    s.execute("typer.images.set", json!({"images": ["a1.psd"]})).unwrap();
    assert_eq!(s.typer.state.last_opened_image, None, "new page files open afresh");
    s.execute("typer.images.clear", json!({})).unwrap();
    assert_eq!(s.execute("typer.state", json!({})).unwrap()["image"], Value::Null);
}

#[test]
fn styles_list_select_and_presets() {
    let mut s = loaded();
    let list = s.execute("typer.style.list", json!({})).unwrap();
    assert_eq!(list["folders"][0]["count"], 2);
    assert_eq!(list["styles"][1]["sizePresets"], json!([30.0, 40.0]));
    assert_eq!(list["default"], "reg");
    s.execute("typer.style.select", json!({"index": 1})).unwrap();
    assert_eq!(s.typer.state.current_style_id, Some(Id::str("sfx")));
    let r = s.execute("typer.style.nextSizePreset", json!({"now": 5})).unwrap();
    assert_eq!((r["changed"].as_bool(), r["size"].as_f64()), (Some(true), Some(40.0)));
    s.execute("typer.style.next", json!({})).unwrap();
    assert_eq!(s.typer.state.current_style_id, Some(Id::str("reg")));
    let r = s.execute("typer.style.setSize", json!({"id": "reg", "size": 16})).unwrap();
    assert_eq!(r["size"], 16.0);
    assert!(s.execute("typer.style.select", json!({"id": "ghost"})).is_err());
}

#[test]
fn style_resolve_uses_the_document_dpi() {
    let mut s = loaded();
    s.execute("file.new", json!({"width": 1200, "height": 1800, "resolution": 300})).unwrap();
    let r = s.execute("typer.style.resolve", json!({"id": "sfx"})).unwrap();
    assert_eq!(r["dpi"], 300.0);
    assert!((r["sizePt"].as_f64().unwrap() - 30.0 * 72.0 / 300.0).abs() < 1e-4);
    assert_eq!(r["charStyle"]["font_family"], "Komika");
    assert_eq!(r["stroke"]["size"], 4.0);
    let r = s.execute("typer.style.resolve", json!({"id": "sfx", "dpi": 72, "textScale": 50})).unwrap();
    assert_eq!(r["sizePt"], 15.0);
}

#[test]
fn style_and_folder_crud() {
    let mut s = loaded();
    let id = s.execute("typer.style.save", json!({"style": {"name": "Thought", "prefixes": "TH:; (", "folder": "f1"}, "now": 9})).unwrap()["id"].clone();
    let lib = &s.typer.state.library;
    let st = lib.style(&Id::from_value(&id).unwrap()).unwrap();
    assert_eq!(st.prefixes, ["TH:", "("]);
    assert!(st.text_props().is_some(), "new styles get TypeR's default text props");
    assert_eq!(st.edited, Some(9.into()));
    // Partial save merges into the existing style.
    s.execute("typer.style.save", json!({"style": {"id": id, "prefixColor": "#C2F2FF"}})).unwrap();
    let st = s.typer.state.library.style(&Id::from_value(&id).unwrap()).unwrap();
    assert_eq!((st.name.as_str(), st.prefix_color()), ("Thought", "#C2F2FF"));
    assert!(s.execute("typer.style.save", json!({"style": {"name": ""}})).is_err());
    assert!(s.execute("typer.style.save", json!({"style": {"name": "x", "folder": "ghost"}})).is_err());
    assert!(s.execute("typer.style.save", json!({"style": {"name": "x", "prefixes": [1]}})).is_err());

    let dup = s.execute("typer.style.duplicate", json!({"id": "sfx", "folder": null})).unwrap()["id"].clone();
    assert_eq!(s.typer.state.current_style_id, Id::from_value(&dup));
    assert_eq!(s.execute("typer.style.togglePrefixes", json!({"id": "sfx"})).unwrap()["prefixesDisabled"], true);
    s.execute("typer.style.reorder", json!({"id": "sfx", "folder": "f1", "order": ["sfx", "reg"]})).unwrap();

    let sub = s.execute("typer.folder.save", json!({"folder": {"name": "Ch 12", "parentId": "f1"}, "styleIds": ["sfx"]})).unwrap()["id"].clone();
    assert!(s.execute("typer.folder.save", json!({"folder": {"id": "f1", "name": "Series", "parentId": sub}})).is_err(), "no cycles");
    let copy = s.execute("typer.folder.duplicate", json!({"id": "f1"})).unwrap()["id"].clone();
    let exported = s.execute("typer.library.exportFolder", json!({"id": copy})).unwrap();
    assert_eq!(exported["name"], "Series copy");
    s.execute("typer.folder.delete", json!({"id": copy, "permanent": true})).unwrap();
    s.execute("typer.folder.reorder", json!({"parentId": null, "order": ["f1"]})).unwrap();
    assert!(s.execute("typer.folder.delete", json!({"id": "ghost"})).is_err());
    assert!(s.execute("typer.style.delete", json!({"id": dup})).is_ok());
}

#[test]
fn library_round_trips_through_the_commands() {
    let mut s = loaded();
    let out = s.execute("typer.library.export", json!({"withSettings": true})).unwrap();
    assert_eq!(out["defaultStyleId"], "reg");
    assert_eq!(out["schema"], 1);
    let mut t = Session::new();
    let r = t.execute("typer.library.import", json!({"json": out.to_string()})).unwrap();
    assert_eq!(r["kind"], "full");
    let again = t.execute("typer.library.export", json!({"withSettings": true})).unwrap();
    assert_eq!(again["styles"], out["styles"]);
    assert_eq!(again["folders"], out["folders"]);
    // Validator rejections reach the caller as bad params, and change nothing.
    let before = t.typer.clone();
    for bad in [r#"{"__proto__":{}}"#, r#"{"folders":[{"id":"a","name":"A","parentId":"b"},{"id":"b","name":"B","parentId":"a"}]}"#, "[1]", "not json"] {
        assert!(matches!(t.execute("typer.library.import", json!({"json": bad})), Err(EngineError::BadParams { .. })), "{bad}");
    }
    assert_eq!(t.typer, before);
}

#[test]
fn settings_get_and_set() {
    let mut s = loaded();
    assert_eq!(s.execute("typer.settings.get", json!({})).unwrap()["internalPadding"], 10.0);
    let r = s.execute("typer.settings.set", json!({"values": {"ignoreTags": "**", "textScale": 50}})).unwrap();
    assert_eq!((r["ignoreTags"].clone(), r["textScale"].as_f64()), (json!(["**"]), Some(50.0)));
    s.execute("typer.settings.set", json!({"key": "resetLineCounterOnPage", "value": false})).unwrap();
    assert_eq!(s.execute("typer.page.next", json!({})).unwrap()["number"], 4, "numbering no longer restarts per page");
    assert!(s.execute("typer.settings.set", json!({"key": "defaultStyleId", "value": "ghost"})).is_err());
    assert!(s.execute("typer.settings.set", json!({"values": {"textScale": 50, "nope": 1}})).is_err());
    assert_eq!(s.typer.state.settings.text_scale, Some(50.0), "a failed set changes nothing");
}

#[test]
fn work_package_stubs_are_registered_and_refuse_cleanly() {
    let mut s = loaded();
    // Line shaping (WP4) without a document needs text.
    assert!(matches!(s.execute("typer.shapeText", json!({})), Err(EngineError::BadParams { .. })));
    // Placement (WP1) needs a document.
    for id in ["typer.paste", "typer.apply", "typer.align", "typer.detectBubble", "typer.style.fromLayer"] {
        match s.execute(id, json!({})) {
            Err(EngineError::Disabled(_, why)) => assert!(why.contains("no document"), "{id}: {why}"),
            other => panic!("{id}: {other:?}"),
        }
    }
}

#[test]
fn every_typer_command_fails_gracefully_on_bad_params() {
    let bad_params = [
        json!({}),
        json!({"id": [], "index": -1, "text": 5, "json": 3, "key": 1, "values": 2, "images": "x", "size": "big", "folder": 7, "order": "x", "style": "s"}),
        json!({"id": "ghost", "index": 1e18, "text": "", "json": "{", "folders": [5], "size": -1, "kind": "zip", "parentId": {}, "styleIds": [{}]}),
        json!({"style": {"id": {}, "name": 1}, "folder": {"name": "x".repeat(300)}, "images": vec!["a"; 20_000]}),
    ];
    for spec in crate::command_specs().iter().filter(|c| c.id.starts_with("typer.")) {
        for p in &bad_params {
            let mut s = loaded();
            let _ = s.execute(spec.id, p.clone());
            let mut empty = Session::new();
            let _ = empty.execute(spec.id, p.clone());
        }
    }
}

#[test]
fn tabs_hold_separate_chapters() {
    let mut s = loaded();
    s.execute("typer.images.set", json!({"images": ["ch12_001.psd", "ch12_002.psd"]})).unwrap();
    s.execute("typer.line.next", json!({})).unwrap();
    let list = s.execute("typer.tab.list", json!({})).unwrap();
    assert_eq!(list["tabs"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["tabs"][0]["synced"], true);
    let first = list["current"].as_str().unwrap().to_string();
    let r = s.execute("typer.tab.new", json!({"name": "Capítulo 13", "text": "Página 1\nHola."})).unwrap();
    assert_eq!((r["text"].as_str(), r["page"].as_u64(), r["pageFiles"].as_u64()), (Some("Hola."), Some(1), Some(0)));
    let second = r["tab"].as_str().unwrap().to_string();
    let list = s.execute("typer.tab.list", json!({})).unwrap();
    assert_eq!(list["tabs"][1]["name"], "Capítulo 13");
    assert_eq!(list["tabs"][1]["current"], true);
    assert_eq!(list["tabs"][0]["lines"], SCRIPT.split('\n').count());
    // Multi-bubble selections stay with their tab's page: switching drops them.
    s.execute("typer.multiBubble.toggle", json!({"on": true})).unwrap();
    s.typer.stored_selections.push(photocraft_typer::StoredSelection { bounds: Default::default(), opened: None, line_index: None, style_id: None });
    let r = s.execute("typer.tab.switch", json!({"id": first})).unwrap();
    assert_eq!((r["moved"].as_bool(), r["text"].as_str()), (Some(true), Some("BOOM")));
    assert!(s.typer.stored_selections.is_empty());
    assert_eq!(s.execute("typer.tab.next", json!({})).unwrap()["tab"], second.as_str());
    assert_eq!(s.execute("typer.tab.next", json!({})).unwrap()["tab"], first.as_str(), "wraps");
    assert_eq!(s.execute("typer.tab.previous", json!({})).unwrap()["tab"], second.as_str());
    s.execute("typer.tab.rename", json!({"name": "Cap. 13"})).unwrap();
    assert_eq!(s.execute("typer.tab.list", json!({})).unwrap()["tabs"][1]["name"], "Cap. 13");
    let r = s.execute("typer.tab.close", json!({})).unwrap();
    assert_eq!((r["closed"].as_str(), r["tab"].as_str()), (Some(second.as_str()), Some(first.as_str())));
    // Bad params and the last tab.
    assert!(matches!(s.execute("typer.tab.close", json!({})), Err(EngineError::BadParams { .. })));
    assert!(s.execute("typer.tab.switch", json!({})).is_err());
    assert!(s.execute("typer.tab.switch", json!({"id": 4})).is_err());
    assert!(s.execute("typer.tab.rename", json!({"name": ""})).is_err());
    assert!(s.execute("typer.tab.rename", json!({"id": [], "name": "x"})).is_err());
    assert!(s.execute("typer.tab.new", json!({"text": 5})).is_err());
    // The workspace round-trips with its tabs.
    s.execute("typer.tab.new", json!({})).unwrap();
    let back = crate::typer_state::TyperSession::from_json(&s.typer.to_json()).unwrap();
    assert_eq!(back.state, s.typer.state);
}
