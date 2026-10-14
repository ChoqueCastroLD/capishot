use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use serde_json::{Value, json};

use super::*;

/// A library with two pixel styles (the SFX one with a white stroke).
fn library() -> Value {
    json!({
        "folders": [{"id": "f1", "name": "Series", "parentId": null, "order": 0}],
        "styles": [
            {"id": "reg", "name": "Regular", "folder": "f1",
             "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"fontName": "Komika Display", "fontPostScriptName": "KomikaDisplay", "size": 20}}]}, "typeUnit": "pixelsUnit"}},
            {"id": "sfx", "name": "SFX", "folder": "f1",
             "textProps": {"layerText": {"textStyleRange": [{"textStyle": {"fontName": "Badaboom", "fontPostScriptName": "BadaBoomBB", "size": 40, "syntheticItalic": true}}]}, "typeUnit": "pixelsUnit"},
             "stroke": {"enabled": true, "size": 4, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}}}
        ],
        "includesSettings": true
    })
}

/// A white page with text layers pasted in the given styles.
fn page_with_text(styles: &[&str]) -> Session {
    let mut s = Session::new();
    s.execute("typer.library.import", json!({"json": library(), "now": 1})).unwrap();
    s.add_document(Document::with_background("p1.psd", Size::new(600, 400), ColorMode::Rgb, SampleType::U8, Color::WHITE), None);
    for (i, st) in styles.iter().enumerate() {
        let y = 20 + 60 * i as i64;
        s.execute("typer.paste", json!({"text": format!("Line {i}"), "bounds": [20, y, 300, 50], "style": {"id": st}, "advance": false})).unwrap();
    }
    s
}

#[test]
fn open_documents_scan_into_font_groups() {
    let mut s = page_with_text(&["reg", "reg", "sfx", "reg"]);
    let r = s.execute("typer.fontScan", json!({})).unwrap();
    assert_eq!((r["files"].as_u64(), r["layers"].as_u64()), (Some(1), Some(4)));
    let groups = r["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    let reg = &groups[0];
    assert_eq!(reg["fontPostScriptName"], "KomikaDisplay");
    assert_eq!(reg["usageCount"], 3, "most used first");
    assert_eq!(reg["topSize"], 20.0, "sizes in pixels at the document's resolution");
    assert_eq!(reg["installed"], false, "not a font this test machine has");
    let sfx = &groups[1];
    assert_eq!(sfx["syntheticItalic"], true);
    assert!(sfx["defaultName"].as_str().unwrap().contains("(faux italic)"));
    assert_eq!(sfx["stroke"]["enabled"], true);
    assert_eq!(sfx["stroke"]["size"], 4);
    // Nothing open: no groups, no error.
    let mut empty = Session::new();
    assert_eq!(empty.execute("typer.fontScan", json!({})).unwrap()["groups"], json!([]));
    assert!(empty.execute("typer.fontScan", json!({"documents": [0]})).is_err());
    assert!(empty.execute("typer.fontScan", json!({"documents": "some"})).is_err());
    assert!(empty.execute("typer.fontScan", json!({"scans": {}})).is_err());
    assert!(empty.execute("typer.fontScan", json!({"scans": [{"layers": 3}]})).is_err());
}

#[test]
fn psd_files_scan_without_decoding_pixels() {
    let s = page_with_text(&["sfx", "reg"]);
    let doc = &s.active().unwrap().doc;
    let bytes = photocraft_io::document_to_psd(doc).to_bytes().unwrap();
    let scan = scan_psd("ch1_p01.psd", &bytes).unwrap();
    assert_eq!(scan.layers.len(), 2);
    assert_eq!(scan.file, "ch1_p01.psd");
    let sfx = scan.layers.iter().find(|l| l.runs[0]["fontPostScriptName"] == "BadaBoomBB").expect("the SFX layer");
    assert_eq!(sfx.stroke.as_ref().map(|s| s["enabled"].clone()), Some(json!(true)), "the stroke comes from lfx2");
    assert_eq!(sfx.runs[0]["size"], 40);
    // The shell's scans go through the command.
    let mut fresh = Session::new();
    let r = fresh.execute("typer.fontScan", json!({"scans": [scan]})).unwrap();
    assert_eq!(r["groups"].as_array().map(Vec::len), Some(2));
    assert!(scan_psd("x.psd", b"not a psd").unwrap_err().starts_with("x.psd: "));
}

#[test]
fn proposals_import_into_a_new_or_existing_folder() {
    let mut s = page_with_text(&["reg", "sfx"]);
    let groups = s.execute("typer.fontScan", json!({})).unwrap()["groups"].clone();
    let styles: Vec<Value> =
        groups.as_array().unwrap().iter().map(|g| json!({"name": g["defaultName"], "textProps": g["textProps"], "stroke": g["stroke"]})).collect();
    let before = s.typer.state.library.styles.len();
    let r = s.execute("typer.fontScan.import", json!({"styles": styles, "newFolder": "Capítulo 3", "now": 99})).unwrap();
    let folder = Id::from_value(&r["folder"]).unwrap();
    let lib = &s.typer.state.library;
    assert_eq!(lib.folder(&folder).map(|f| f.name.as_str()), Some("Capítulo 3"));
    assert_eq!(lib.styles.len(), before + 2);
    let ids: Vec<Id> = r["styles"].as_array().unwrap().iter().filter_map(Id::from_value).collect();
    let first = lib.style(&ids[0]).unwrap();
    assert_eq!(first.folder.as_ref(), Some(&folder));
    assert_eq!(first.edited.as_ref().and_then(serde_json::Number::as_f64), Some(99.0));
    assert_eq!(first.size(), Some(20.0));
    // Into an existing folder, and into Unsorted.
    let r = s.execute("typer.fontScan.import", json!({"styles": [styles[0].clone()], "folder": "f1"})).unwrap();
    assert_eq!(r["folder"], "f1");
    let r = s.execute("typer.fontScan.import", json!({"styles": [styles[0].clone()]})).unwrap();
    assert_eq!(r["folder"], Value::Null);
    // Errors change nothing.
    let n = s.typer.state.library.styles.len();
    assert!(s.execute("typer.fontScan.import", json!({"styles": []})).is_err());
    assert!(s.execute("typer.fontScan.import", json!({"styles": [styles[0].clone()], "folder": "nope"})).is_err());
    assert!(s.execute("typer.fontScan.import", json!({"styles": [styles[0].clone()], "folder": "f1", "newFolder": "x"})).is_err());
    assert!(s.execute("typer.fontScan.import", json!({"styles": [styles[0].clone(), {"name": "", "textProps": {}}]})).is_err());
    assert!(s.execute("typer.fontScan.import", json!({"styles": [{"name": "x"}]})).is_err());
    assert_eq!(s.typer.state.library.styles.len(), n);
}

/// A minimal `.tpl`: one Type tool preset (`Komika 30 px`) and one brush preset.
fn tpl_bytes() -> Vec<u8> {
    fn u32b(v: u32) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }
    fn unicode(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = u32b(units.len() as u32 + 1);
        units.iter().for_each(|u| out.extend(u.to_be_bytes()));
        out.extend([0, 0]);
        out
    }
    fn id(s: &str) -> Vec<u8> {
        let mut out = if s.len() == 4 { u32b(0) } else { u32b(s.len() as u32) };
        out.extend(s.as_bytes());
        out
    }
    fn obj(class: &str, props: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut out = b"Objc".to_vec();
        out.extend(unicode(""));
        out.extend(id(class));
        out.extend(u32b(props.len() as u32));
        for (k, v) in props {
            out.extend(id(k));
            out.extend(v);
        }
        out
    }
    let mut text = b"TEXT".to_vec();
    text.extend(unicode("Komika Display"));
    let mut size = b"UntF#Pxl".to_vec();
    size.extend(30.0f64.to_be_bytes());
    let txts = obj("TxtS", vec![("FntN", text), ("Sz  ", size)]);
    let chars = obj("textToolCharacterOptions", vec![("TxtS", txts)]);
    let mut out = b"8BTP".to_vec();
    out.extend([0u8; 8]);
    out.extend(b"8BIM");
    out.extend([0u8; 4]);
    out.extend(b"8BIMtptp");
    out.extend([0u8; 8]);
    for (name, tool, props) in [("$$$/Presets=Globo", "typeCreateOrEditTool", vec![("textToolCharacterOptions", chars)]), ("Brush", "paintbrushTool", vec![])] {
        out.extend(unicode(name));
        out.extend([0u8; 10]);
        out.extend(id(tool));
        out.extend(u32b(props.len() as u32));
        for (k, v) in props {
            out.extend(id(k));
            out.extend(v);
        }
    }
    out
}

#[test]
fn tool_presets_import_as_a_folder_of_styles() {
    let mut s = Session::new();
    let hex: String = tpl_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let r = s.execute("typer.library.importTpl", json!({"hex": hex, "name": "Mis estilos.tpl", "now": 7})).unwrap();
    assert_eq!(r["styles"], 1, "{r}");
    let lib = &s.typer.state.library;
    assert_eq!(lib.folders.len(), 1);
    assert_eq!(lib.folders[0].name, "Mis estilos");
    let st = &lib.styles[0];
    assert_eq!(st.name, "Globo");
    assert_eq!(st.folder.as_ref(), Some(&lib.folders[0].id));
    assert_eq!((st.size(), st.type_unit()), (Some(30.0), Some("pixelsUnit")));
    // Errors carry TypeR's codes and change nothing.
    let e = s.execute("typer.library.importTpl", json!({"hex": "6e6f7420612074706c"})).unwrap_err().to_string();
    assert!(e.contains("TPL_INVALID_FILE"), "{e}");
    assert!(s.execute("typer.library.importTpl", json!({"hex": "zz"})).unwrap_err().to_string().contains("TPL_INVALID_FILE"));
    assert!(s.execute("typer.library.importTpl", json!({})).is_err());
    assert!(s.execute("typer.library.importTpl", json!({"hex": hex, "name": 3})).is_err());
    assert_eq!(s.typer.state.library.styles.len(), 1);
}
