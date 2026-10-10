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
