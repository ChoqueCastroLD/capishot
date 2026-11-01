use serde_json::json;

use super::*;

/// A session with `n` documents, each a flat colour, named page-a, page-b…
fn session(n: usize) -> Session {
    let mut s = Session::new();
    for i in 0..n {
        s.execute("file.new", json!({"width": 200, "height": 100, "background": "white", "name": format!("page-{}", (b'a' + i as u8) as char)})).unwrap();
        s.execute("select.all", json!({})).unwrap();
        s.execute("edit.fill", json!({"color": format!("#{:02x}3060", 40 * i)})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
    }
    s
}

fn entries(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    let r = photocraft_format::zip::ZipReader::new(zip).unwrap();
    let names: Vec<String> = r.entries.iter().map(|e| e.name.clone()).collect();
    names.iter().map(|n| (n.clone(), r.read_by_name(n, 1 << 30).unwrap())).collect()
}

fn run(s: &Session, p: serde_json::Value) -> (Vec<u8>, Value) {
    let ex = prepare(s, &p).unwrap();
    let mut zip = Vec::new();
    let v = ex
        .run(&mut |_, _| true, &mut |c| {
            zip.extend(c);
            Ok(())
        })
        .unwrap();
    (zip, v)
}

#[test]
fn three_documents_make_three_pages_in_order_with_their_names() {
    let s = session(3);
    let (zip, v) = run(&s, json!({}));
    let e = entries(&zip);
    assert_eq!(e.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["page-a.png", "page-b.png", "page-c.png"]);
    assert!(e.iter().all(|(_, b)| b.starts_with(b"\x89PNG")));
    assert_eq!(v["failed"], json!([]));
    // Numbered with a prefix, in the order asked.
    let ids: Vec<u64> = s.documents().iter().map(|d| d.doc.id.0).collect();
    let (zip, _) = run(&s, json!({"docs": [ids[2], ids[0], ids[1]], "naming": "sequence", "prefix": "cap1-"}));
    let e = entries(&zip);
    assert_eq!(e.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["cap1-001.png", "cap1-002.png", "cap1-003.png"]);
    // The first entry is the third document (its colour).
    let img = photocraft_io::import("x.png", &e[0].1).unwrap().document;
    let px = img.layers[0].surface().unwrap().pixel(10, 10);
    assert!((px[0] - 80.0 / 255.0).abs() < 0.01, "{px:?}");
}

#[test]
fn jpeg_quality_applies() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 400, "height": 300, "background": "white"})).unwrap();
    s.execute("filter.noise.addNoise", json!({"amount": 40, "monochromatic": false, "seed": 1})).unwrap();
    let size = |q: u32| {
        let (zip, _) = run(&s, json!({"format": "jpg", "quality": q}));
        let e = entries(&zip);
        assert!(e[0].0.ends_with(".jpg") && e[0].1.starts_with(&[0xFF, 0xD8]));
        e[0].1.len()
    };
    let (low, high) = (size(20), size(95));
    assert!(low * 2 < high, "quality 20: {low} bytes, 95: {high}");
}

#[test]
fn resize_applies_and_never_enlarges() {
    let s = session(1);
    let dims = |p: serde_json::Value| {
        let (zip, _) = run(&s, p);
        let d = photocraft_io::import("x.png", &entries(&zip)[0].1).unwrap().document;
        (d.size.width, d.size.height)
    };
    assert_eq!(dims(json!({"resize": {"maxWidth": 100}})), (100, 50));
    assert_eq!(dims(json!({"resize": {"percent": 25}})), (50, 25));
    assert_eq!(dims(json!({"resize": {"maxWidth": 1000}})), (200, 100), "never enlarged");
    let (zip, _) = run(&s, json!({"format": "webp", "lossless": true, "srgb": true}));
    assert!(entries(&zip)[0].1.starts_with(b"RIFF"));
}

#[test]
fn a_cancelled_export_stops_and_reports_cancelled() {
    let s = session(3);
    let ex = prepare(&s, &json!({})).unwrap();
    let mut chunks = 0;
    let r = ex.run(&mut |i, _| i < 2, &mut |_| {
        chunks += 1;
        Ok(())
    });
    assert!(matches!(r, Err(EngineError::Cancelled)), "{r:?}");
    assert_eq!(chunks, 2, "two pages went to the sink before the cancel (the caller drops them)");
}

#[test]
fn a_failing_page_is_reported_and_the_others_kept() {
    let s = session(3);
    let mut ex = prepare(&s, &json!({})).unwrap();
    // The second page can't be encoded: a document with no pixels.
    let mut big = (*ex.pages[1].doc).clone();
    big.size = photocraft_geom::Size::new(0, 0);
    ex.pages[1].doc = Arc::new(big);
    let mut zip = Vec::new();
    let v = ex
        .run(&mut |_, _| true, &mut |c| {
            zip.extend(c);
            Ok(())
        })
        .unwrap();
    let e = entries(&zip);
    assert_eq!(e.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["page-a.png", "page-c.png"]);
    assert_eq!(v["failed"].as_array().unwrap().len(), 1);
    assert_eq!(v["failed"][0]["document"], "page-b");
}

#[test]
fn the_command_writes_the_zip_and_no_part_file_is_left() {
    let s = &mut session(2);
    let dir = std::env::temp_dir().join(format!("zip-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cap.zip");
    let v = s.execute(ID, json!({"path": path.to_str().unwrap(), "naming": "sequence"})).unwrap();
    assert_eq!(v["files"], json!(["001.png", "002.png"]));
    assert_eq!(entries(&std::fs::read(&path).unwrap()).len(), 2);
    assert!(!dir.join("cap.zip.part").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entry_names_and_the_default_zip_name() {
    let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(entry_names(&n(&["a.psd", "a.psd", "b"]), "png", false, ""), ["a.png", "a-2.png", "b.png"]);
    let many: Vec<String> = (0..1200).map(|i| format!("p{i}")).collect();
    assert_eq!(entry_names(&many, "jpg", true, "")[0], "0001.jpg");
    let mut s = Session::new();
    assert_eq!(default_zip_name(&s), "capitulo.zip");
    s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
    s.active_mut().unwrap().path = Some("/data/obra/capitulo-12/p001.psd".into());
    assert_eq!(default_zip_name(&s), "capitulo-12.zip");
}
