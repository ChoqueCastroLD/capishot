use std::sync::Arc;

use serde_json::json;

use super::*;

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
const MONO: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

/// `font` with its OS/2 `fsType` set to `v`.
fn with_fs_type(font: &[u8], v: u16) -> Vec<u8> {
    let mut f = font.to_vec();
    let n = u16::from_be_bytes([f[4], f[5]]) as usize;
    for i in 0..n {
        let rec = 12 + i * 16;
        if &f[rec..rec + 4] == b"OS/2" {
            let off = u32::from_be_bytes([f[rec + 8], f[rec + 9], f[rec + 10], f[rec + 11]]) as usize;
            f[off + 8..off + 10].copy_from_slice(&v.to_be_bytes());
        }
    }
    f
}

#[test]
fn page_prefixes_tell_a_chapters_pages() {
    assert_eq!(page_prefix("OnePiece_c1050_p03.psd").as_deref(), Some("OnePiece_c1050_p"));
    assert_eq!(page_prefix("/scans/Kagura/ch12/015.png").as_deref(), None, "a bare number has no prefix");
    assert_eq!(page_prefix("kagura-12-015.jpg").as_deref(), Some("kagura-12-"));
    assert_eq!(page_prefix("portada.psd"), None);
    let mut p = Project::new("op", "One Piece");
    p.patterns = vec!["OnePiece_c1050_p".into(), "folder:Kagura".into()];
    assert!(p.matches("OnePiece_c1050_p17.psd", None));
    assert!(!p.matches("OnePiece_c1051_p01.psd", None));
    assert!(p.matches("015.png", Some("Kagura")));
}

#[test]
fn line_tags_pick_roles() {
    let p = Project::new("x", "X");
    assert_eq!(p.role_for_line("GRITO: ¡Corre!"), Some("shout"));
    assert_eq!(p.role_for_line("  sfx: BOOM"), Some("sfx"));
    assert_eq!(p.role_for_line("Hola: ¿qué tal?"), None);
    assert_eq!(p.role_for_line("Sin etiqueta"), None);
}

#[test]
fn the_licence_bits_are_read() {
    assert_eq!(fs_type(INTER), Some(0), "Inter is installable");
    assert!(!restricted(INTER));
    assert!(restricted(&with_fs_type(INTER, 0x0002)));
    assert!(!restricted(&with_fs_type(INTER, 0x0008)), "editable embedding is fine");
    assert_eq!(fs_type(b"not a font"), None);
}

fn file(name: &str, family: &str, bytes: Vec<u8>) -> FontFile {
    FontFile { name: name.into(), families: vec![family.into()], bytes: Arc::new(bytes) }
}

/// Export → import: the fonts come back (each file once), a restricted one stays out with a
/// warning, a font with no file is only named, and nothing but the manifest, the fonts and the
/// style files is in the archive (never pages or images).
#[test]
fn a_capiproj_round_trips_without_pages_and_without_restricted_fonts() {
    let mut p = Project::new("serie", "Serie");
    p.fonts = vec![
        ProjectFont { family: "Inter".into(), role: "dialogue".into(), sha256: None },
        ProjectFont { family: "Inter".into(), role: "dialogue".into(), sha256: None },
        ProjectFont { family: "JetBrains Mono".into(), role: "sfx".into(), sha256: None },
        ProjectFont { family: "Comic Pro".into(), role: "shout".into(), sha256: None },
        ProjectFont { family: "Arial".into(), role: "notes".into(), sha256: None },
    ];
    p.roles[1].settings = RoleSettings {
        size_pt: Some(40.0),
        caps: Some("all".into()),
        stroke: Some(RoleStroke { width: 4.0, color: "#ffffff".into(), position: "outside".into() }),
        ..Default::default()
    };
    let files = vec![
        file("Inter-Regular.ttf", "Inter", INTER.to_vec()),
        file("Mono.ttf", "JetBrains Mono", MONO.to_vec()),
        file("ComicPro.otf", "Comic Pro", with_fs_type(INTER, 0x0002)),
    ];
    let typer = json!({"styles": []});
    let out = export(&p, &files, &[], false, Some(&typer), None).unwrap();
    assert_eq!(out.restricted, ["Comic Pro"], "the restricted font is left out");
    assert_eq!(out.referenced, ["Arial"], "a system font is only named");
    assert!(!out.large);
    // Only the manifest, fonts and the style file.
    let zip = photocraft_format::zip::ZipReader::new(&out.bytes).unwrap();
    let names: Vec<&str> = zip.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.iter().all(|n| *n == "manifest.json" || *n == "typer.json" || (n.starts_with("fonts/") && (n.ends_with(".ttf") || n.ends_with(".otf")))),
        "{names:?}"
    );
    assert_eq!(names.iter().filter(|n| n.starts_with("fonts/")).count(), 2, "Inter once, Mono once: {names:?}");
    assert!(!names.iter().any(|n| [".png", ".jpg", ".psd", ".webp"].iter().any(|e| n.ends_with(e))));
    let back = import(&out.bytes).unwrap();
    assert_eq!(back.project.name, "Serie");
    assert_eq!(back.project.roles[1].settings.size_pt, Some(40.0));
    assert_eq!(back.fonts.len(), 2);
    assert!(back.fonts.iter().any(|(n, b)| n == "Inter-Regular.ttf" && b == INTER));
    assert_eq!(back.referenced, ["Comic Pro", "Arial"]);
    assert_eq!(back.typer, Some(typer));
    assert!(back.project.fonts.iter().find(|f| f.family == "Inter").unwrap().sha256.is_some());
    // Asked for, the restricted font goes in too.
    assert!(export(&p, &files, &[], true, None, None).unwrap().restricted.is_empty());
    // Unticked fonts stay out.
    assert_eq!(export(&p, &files, &["JetBrains Mono".into()], false, None, None).unwrap().referenced, ["JetBrains Mono", "Arial"]);
}

#[test]
fn a_damaged_or_foreign_archive_is_refused() {
    assert!(import(b"PK not really").is_err());
    let mut z = photocraft_format::zip::ZipWriter::new();
    z.add("manifest.json", br#"{"format":"other"}"#).unwrap();
    assert!(import(&z.finish().unwrap()).is_err());
    let p = Project::new("a", "A");
    let mut out = export(
        &Project { fonts: vec![ProjectFont { family: "Inter".into(), role: "dialogue".into(), sha256: None }], ..p },
        &[file("Inter.ttf", "Inter", INTER.to_vec())],
        &[],
        false,
        None,
        None,
    )
    .unwrap()
    .bytes;
    // Flip a byte of the font: its checksum no longer matches.
    let at = out.windows(4).position(|w| w == &INTER[..4]).unwrap() + 200;
    out[at] ^= 0xff;
    assert!(import(&out).is_err(), "a damaged font is refused");
}

fn session_with_type() -> (Session, u64) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 400, "height": 300, "background": "white"})).unwrap();
    let r = s.execute("type.create", json!({"x": 40, "y": 100, "text": "HOLA", "size": 20})).unwrap();
    (s, r["layer"].as_u64().unwrap())
}

#[test]
fn projects_are_kept_and_a_role_applies_in_one_step() {
    let (mut s, layer) = session_with_type();
    let id = s.execute("fontProjects.create", json!({"name": "Mi serie"})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("fontProjects.addFont", json!({"family": "JetBrains Mono", "role": "shout"})).unwrap();
    s.execute("fontProjects.setRole", json!({"role": "shout", "settings": {"sizePt": 36, "caps": "all", "color": "#cc0000", "stroke": {"width": 3, "color": "#ffffff", "position": "outside"}}})).unwrap();
    s.execute("fontProjects.setRole", json!({"role": "whisper", "name": "Susurro"})).unwrap();
    let list = s.execute("fontProjects.list", json!({})).unwrap();
    assert_eq!(list["active"], id.as_str());
    assert_eq!(list["projects"][0]["roles"].as_array().unwrap().len(), 7, "six built-in roles and the custom one");
    let past = s.active().unwrap().history.past_len();
    s.execute("fontProjects.applyRole", json!({"role": "shout", "layer": layer})).unwrap();
    let st = s.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1, "one undo step");
    let l = st.doc.layer(photocraft_doc::LayerId(layer)).unwrap();
    let photocraft_doc::LayerContent::Text(t) = &l.content else { panic!() };
    assert_eq!(t.char_runs()[0].style.size_pt, 36.0);
    assert_eq!(t.font_family, "JetBrains Mono");
    assert!(l.effects.items.iter().any(|e| matches!(e, photocraft_doc::Effect::Stroke(_))), "the role's stroke");
    // Removing a custom role is fine, a built-in one stays.
    assert!(s.execute("fontProjects.removeRole", json!({"role": "whisper"})).is_ok());
    assert!(s.execute("fontProjects.removeRole", json!({"role": "shout"})).is_err());
    // Duplicate, rename, delete.
    let dup = s.execute("fontProjects.duplicate", json!({})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("fontProjects.set", json!({"id": dup, "name": "Copia"})).unwrap();
    s.execute("fontProjects.delete", json!({"id": dup})).unwrap();
    assert_eq!(s.execute("fontProjects.list", json!({})).unwrap()["projects"].as_array().unwrap().len(), 1);
}

/// A page carries its project in its PSD, and opening it (or a page named like a remembered
/// one) activates the project.
#[test]
fn a_page_remembers_its_project_through_a_psd_and_by_name() {
    let (mut s, _) = session_with_type();
    let id = s.execute("fontProjects.create", json!({"name": "Kagura"})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("fontProjects.activate", json!({"id": id})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    assert_eq!(doc_project(&doc).as_deref(), Some(id.as_str()));
    let psd = photocraft_io::export(&doc, "p.psd", &photocraft_io::ExportOptions::default()).unwrap().bytes;
    let back = photocraft_io::import("p.psd", &psd).unwrap().document;
    assert_eq!(doc_project(&back).as_deref(), Some(id.as_str()), "the PSD keeps it");
    // Another project active; opening the page brings its own back.
    s.execute("fontProjects.create", json!({"name": "Otra"})).unwrap();
    assert_ne!(Projects::of(&s).active.as_deref(), Some(id.as_str()));
    crate::file_cmds::open_bytes_as(&mut s, "p.psd", &psd, None, Some("/pages/p.psd".into())).unwrap();
    assert_eq!(activate_for_open(&mut s).as_deref(), Some(id.as_str()), "opening the page activates its project");
    assert_eq!(Projects::of(&s).active.as_deref(), Some(id.as_str()));
    // By name.
    s.execute("fontProjects.rememberPattern", json!({"id": id, "file": "kagura_c12_p01.psd"})).unwrap();
    assert_eq!(s.execute("fontProjects.forFile", json!({"file": "kagura_c12_p07.psd"})).unwrap()["id"], id.as_str());
    assert_eq!(s.execute("fontProjects.forFile", json!({"file": "otra_p07.psd"})).unwrap()["id"], serde_json::Value::Null);
}

/// A TypeR line tagged with a role's tag (`GRITO: …`) is placed in the role's font and settings,
/// in the paste's one undo step.
#[test]
fn typer_line_tags_use_the_projects_roles() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 400, "height": 300, "background": "white"})).unwrap();
    s.execute("fontProjects.create", json!({"name": "Serie"})).unwrap();
    s.execute("fontProjects.addFont", json!({"family": "JetBrains Mono", "role": "shout"})).unwrap();
    s.execute("fontProjects.setRole", json!({"role": "shout", "settings": {"sizePt": 33}})).unwrap();
    s.execute("select.rect", json!({"x": 50, "y": 50, "width": 200, "height": 100})).unwrap();
    let past = s.active().unwrap().history.past_len();
    let r = s.execute("typer.paste", json!({"text": "GRITO: ¡Corre!"})).unwrap();
    let st = s.active().unwrap();
    assert_eq!(st.history.past_len(), past + 1, "one undo step");
    let l = st.doc.layer(photocraft_doc::LayerId(r["layer"].as_u64().unwrap())).unwrap();
    let photocraft_doc::LayerContent::Text(t) = &l.content else { panic!() };
    assert_eq!(t.font_family, "JetBrains Mono");
    assert_eq!(t.char_runs()[0].style.size_pt, 33.0);
    // An untagged line keeps TypeR's own style.
    let r = s.execute("typer.paste", json!({"text": "Hola"})).unwrap();
    let l = s.active().unwrap().doc.layer(photocraft_doc::LayerId(r["layer"].as_u64().unwrap())).unwrap().clone();
    let photocraft_doc::LayerContent::Text(t) = &l.content else { panic!() };
    assert_ne!(t.font_family, "JetBrains Mono");
}
