//! A preview is a new document, never a save target for the Affinity source.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use photocraft_engine::Session;
use photocraft_engine::file_cmds::open_bytes_as;
use photocraft_engine::jobs::{OpenSource, Started};
use serde_json::json;

fn fixture() -> Vec<u8> {
    let mut src = Session::new();
    src.execute("file.new", json!({"width": 2, "height": 1, "background": "#dc283c"})).unwrap();
    let png = photocraft_io::export(&src.active().unwrap().doc, "preview.png", &Default::default()).unwrap().bytes;
    // Observed v12 preview envelope, without a native object graph.
    let mut bytes = vec![0; 72];
    bytes[..4].copy_from_slice(b"\x00\xffKA");
    bytes[4..6].copy_from_slice(&12u16.to_le_bytes());
    bytes[8..12].copy_from_slice(b"nsrP");
    bytes[12..16].copy_from_slice(b"#Inf");
    bytes[24..32].copy_from_slice(&72u64.to_le_bytes());
    bytes[64..68].copy_from_slice(b"Prot");
    bytes.extend(b"\xff\xff\xff\xffThmb");
    bytes.extend(1u32.to_le_bytes());
    bytes.extend((png.len() as u32 + 13).to_le_bytes());
    bytes.extend(29u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend((png.len() as u32).to_le_bytes());
    bytes.push(1);
    bytes.extend(png);
    bytes
}

#[test]
fn preview_never_saves_back_to_its_native_or_renamed_source() {
    let dir = std::env::temp_dir().join(format!("photocraft-affinity-source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = fixture();
    for name in ["source.af", "renamed.psd"] {
        let path = dir.join(name).to_string_lossy().into_owned();
        std::fs::write(&path, &bytes).unwrap();
        let mut s = Session::new();
        let r = open_bytes_as(&mut s, &path, &bytes, None, Some(path.clone())).unwrap();
        assert!(r["warnings"].to_string().contains("embedded"));
        assert!(s.active().unwrap().path.is_none(), "{name}");
        assert!(s.active().unwrap().source_read_only);
        assert!(!s.is_enabled("file.revert"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // Explicit native export is refused before any file is written.
        let native = dir.join("refused.af");
        assert!(s.execute("file.saveACopy", json!({"path": native})).is_err());
        assert!(!native.exists());
        let copy = dir.join(format!("{name}.pcraft"));
        s.execute("file.saveACopy", json!({"path": copy})).unwrap();
        assert!(photocraft_io::import("copy.pcraft", &std::fs::read(copy).unwrap()).is_ok());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn background_preview_retains_the_source_protection_and_warning() {
    let mut s = Session::new();
    let started = s.start_open("source.af", OpenSource::Bytes(Arc::new(fixture()))).unwrap();
    let result = match started {
        Started::Job(id) => s.wait_job(id).unwrap(),
        Started::Done(v) => v,
    };
    assert!(result["warnings"].to_string().contains("Native layers"));
    assert!(s.active().unwrap().source_read_only);
    assert!(s.active().unwrap().path.is_none());
}

#[test]
fn warningless_auxiliary_paths_cannot_silently_use_a_thumbnail() {
    let bytes = fixture();
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    let layers = s.active().unwrap().doc.layers.len();
    let error = photocraft_engine::file_cmds::place_bytes(&mut s, "source.af", bytes.clone(), None, &json!({})).unwrap_err().to_string();
    assert!(error.contains("only an Affinity preview"), "{error}");
    assert_eq!(s.active().unwrap().doc.layers.len(), layers);
    assert!(photocraft_engine::smart_cmds::decode_source("source.af", &bytes).unwrap_err().to_string().contains("export PSD or PNG"));
}
