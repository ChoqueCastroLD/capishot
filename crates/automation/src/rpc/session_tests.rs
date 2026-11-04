//! CLI session mode: stable layer ids, region renders, timeouts, jobs, saves (Visoria C.12–C.18).

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pc-rpc-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn layers(h: &mut Headless) -> Vec<Value> {
    h.handle("doc.inspect", json!({})).unwrap()["layers"].as_array().unwrap().clone()
}

fn uid_of(h: &mut Headless, name: &str) -> Value {
    layers(h).into_iter().find(|l| l["name"] == name).unwrap_or_else(|| panic!("no layer {name}"))["uid"].clone()
}

#[test]
fn layer_uids_survive_save_and_reopen_and_a_duplicate_gets_a_new_one() {
    let dir = temp_dir("uids");
    let mut h = Headless::trusted_local();
    h.handle("doc.new", json!({"width": 32, "height": 32, "background": "white"})).unwrap();
    for name in ["ink", "lang_es"] {
        h.handle("engine.execute", json!({"command": "layer.new.layer", "params": {"name": name}})).unwrap();
    }
    let all = layers(&mut h);
    assert!(all.iter().all(|l| l["uid"].is_u64()), "every layer has a uid: {all:?}");
    let es = uid_of(&mut h, "lang_es");
    let ink = uid_of(&mut h, "ink");
    assert_ne!(es, ink);
    let path = dir.join("page.psd");
    h.handle("doc.save", json!({"path": path.to_string_lossy()})).unwrap();
    for _ in 0..2 {
        h.handle("doc.close", json!({})).unwrap();
        h.handle("doc.open", json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(uid_of(&mut h, "lang_es"), es, "kept on reopen");
        assert_eq!(uid_of(&mut h, "ink"), ink);
        h.handle("doc.save", json!({})).unwrap();
    }
    // Duplicate: a new uid, the original keeps its own; undo and redo keep both.
    let id = layers(&mut h).into_iter().find(|l| l["name"] == "lang_es").unwrap()["id"].clone();
    h.handle("engine.execute", json!({"command": "layer.select", "params": {"layer": id}})).unwrap();
    h.handle("engine.execute", json!({"command": "layer.duplicate"})).unwrap();
    let uids: Vec<Value> = layers(&mut h).into_iter().filter(|l| l["name"].as_str().unwrap().starts_with("lang_es")).map(|l| l["uid"].clone()).collect();
    assert_eq!(uids.len(), 2, "{uids:?}");
    assert!(uids.contains(&es) && uids.iter().all(Value::is_u64) && uids[0] != uids[1], "{uids:?}");
    let copy = uids.into_iter().find(|u| *u != es).unwrap();
    h.handle("engine.execute", json!({"command": "edit.undo"})).unwrap();
    h.handle("engine.execute", json!({"command": "edit.redo"})).unwrap();
    assert!(layers(&mut h).iter().any(|l| l["uid"] == copy), "redo keeps the copy's uid");
    std::fs::remove_dir_all(dir).unwrap();
}
