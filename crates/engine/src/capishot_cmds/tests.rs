use super::*;

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("capishot-cmds-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}

fn red_doc() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 10, "background": "#ff0000"})).unwrap();
    s
}

fn png_size(path: &str) -> (u32, u32) {
    let doc = crate::file_cmds::import(path, &std::fs::read(path).unwrap()).unwrap();
    (doc.size.width, doc.size.height)
}

#[test]
fn compile_writes_the_active_document() {
    let dir = tmp("one");
    let mut s = red_doc();
    let path = crate::file_cmds::join(&dir, "page.png");
    let r = s.execute(COMPILE, json!({"path": path})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(20), Some(10)));
    assert_eq!(png_size(&path), (20, 10));
}

#[test]
fn compile_folder_compiles_every_psd() {
    let dir = tmp("folder");
    let input = crate::file_cmds::join(&dir, "in");
    std::fs::create_dir_all(&input).unwrap();
    let mut s = red_doc();
    for n in ["01", "02"] {
        crate::file_cmds::save_doc(&s.active().unwrap().doc, &crate::file_cmds::join(&input, &format!("{n}.psd")), None).unwrap();
    }
    std::fs::write(crate::file_cmds::join(&input, "notes.txt"), "not a page").unwrap();
    let out = crate::file_cmds::join(&dir, "out");
    let r = s.execute(COMPILE_FOLDER, json!({"input": input, "output": out, "format": "jpg", "quality": 90})).unwrap();
    let files: Vec<&str> = r["files"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(files.len(), 2, "{r}");
    assert!(files[0].ends_with("01.jpg") && files[1].ends_with("02.jpg"), "{files:?}");
    assert_eq!(png_size(files[1]), (20, 10));
}

#[test]
fn bad_params_are_errors() {
    let mut s = red_doc();
    for p in [
        json!({}),
        json!({"path": ""}),
        json!({"path": "x.gif"}),
        json!({"path": "x.png", "quality": 0}),
        json!({"path": "x.png", "fonts": 3}),
        json!({"path": "x.png", "fonts": [1]}),
    ] {
        assert!(s.execute(COMPILE, p.clone()).is_err(), "{p}");
    }
    for p in [json!({}), json!({"input": "/nonexistent-dir", "output": "/tmp/x"}), json!({"input": ".", "output": "/tmp/x", "format": "bmp"})] {
        assert!(s.execute(COMPILE_FOLDER, p.clone()).is_err(), "{p}");
    }
    let mut empty = Session::new();
    assert!(empty.execute(COMPILE, json!({"path": "x.png"})).is_err());
}

fn psd_bytes(s: &Session) -> Vec<u8> {
    photocraft_io::export(&s.active().unwrap().doc, "x.psd", &Default::default()).unwrap().bytes
}

#[test]
fn compile_with_a_writer_hands_over_the_encoded_image() {
    let mut s = red_doc();
    let mut got = Vec::new();
    let r = compile_active_with_writer(&mut s, &json!({"path": "page.webp"}), &mut |path, bytes| {
        got.push((path.to_string(), bytes.to_vec()));
        Ok(())
    })
    .unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(20), Some(10)));
    let [(path, bytes)] = got.as_slice() else { panic!("one file expected: {got:?}") };
    assert_eq!(path, "page.webp");
    let doc = crate::file_cmds::import("page.webp", bytes).unwrap();
    assert_eq!((doc.size.width, doc.size.height), (20, 10));
    // A writer error is the command's error; bad params never reach the writer.
    assert!(compile_active_with_writer(&mut s, &json!({"path": "p.png"}), &mut |_, _| Err(EngineError::Other("disk full".into()))).is_err());
    for p in [json!({}), json!({"path": "x.gif"}), json!({"path": "x.png", "quality": 101})] {
        assert!(compile_active_with_writer(&mut s, &p, &mut |_, _| panic!("written for {p}")).is_err());
    }
    assert!(compile_active_with_writer(&mut Session::new(), &json!({"path": "x.png"}), &mut |_, _| Ok(())).is_err());
}

#[test]
fn rerendered_type_uses_the_app_fonts() {
    let mut s = red_doc();
    s.execute("type.create", json!({"x": 1, "y": 8, "text": "Hi", "font": "Inter", "size": 8})).unwrap();
    let mut out = Vec::new();
    let r = compile_active_with_writer(&mut s, &json!({"path": "p.png", "rerenderText": true, "systemFonts": false}), &mut |_, b| {
        out = b.to_vec();
        Ok(())
    })
    .unwrap();
    let warnings: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(!warnings.iter().any(|w| w.contains("Inter")), "{warnings:?}");
    assert!(!out.is_empty());
}

#[test]
fn picked_files_compile_into_one_zip() {
    let s = red_doc();
    let psd = psd_bytes(&s);
    let files = vec![
        ("02.psd".to_string(), psd.clone()),
        ("01.PSD".to_string(), psd.clone()),
        ("sub/01.psb".to_string(), psd.clone()),
        ("notes.txt".to_string(), b"not a page".to_vec()),
        ("broken.psd".to_string(), b"8BPS garbage".to_vec()),
    ];
    let (zip, r) = compile_files_to_zip(&s, &files, &json!({"format": "jpg", "quality": 80})).unwrap();
    let names: Vec<&str> = r["files"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(names, ["01.jpg", "02.jpg", "01-2.jpg"], "{r}");
    let failed: Vec<&str> = r["failed"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(failed.iter().any(|f| f.starts_with("notes.txt")) && failed.iter().any(|f| f.starts_with("broken.psd")));
    let reader = photocraft_format::zip::ZipReader::new(&zip).unwrap();
    let entry = reader.find("02.jpg").unwrap();
    let page = reader.read(entry, 1 << 20).unwrap();
    let doc = crate::file_cmds::import("02.jpg", &page).unwrap();
    assert_eq!((doc.size.width, doc.size.height), (20, 10));
}

#[test]
fn picked_files_without_a_page_are_an_error() {
    let s = red_doc();
    assert!(compile_files_to_zip(&s, &[], &json!({})).is_err());
    assert!(compile_files_to_zip(&s, &[("a.txt".into(), vec![1, 2])], &json!({})).is_err());
    assert!(compile_files_to_zip(&s, &[("a.psd".into(), Vec::new())], &json!({})).is_err());
    let psd = psd_bytes(&s);
    for p in [json!({"format": "bmp"}), json!({"quality": 0}), json!({"fonts": 3})] {
        assert!(compile_files_to_zip(&s, &[("a.psd".into(), psd.clone())], &p).is_err(), "{p}");
    }
}
