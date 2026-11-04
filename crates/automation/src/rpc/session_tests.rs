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

#[test]
fn render_a_region_at_a_scale_of_some_layers() {
    let mut h = Headless::trusted_local();
    h.handle("doc.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    h.handle("engine.execute", json!({"command": "layer.new.layer", "params": {"name": "red"}})).unwrap();
    h.handle("engine.execute", json!({"command": "select.rect", "params": {"x": 10, "y": 10, "width": 20, "height": 10}})).unwrap();
    h.handle("engine.execute", json!({"command": "edit.fill", "params": {"color": "#ff0000"}})).unwrap();
    h.handle("engine.execute", json!({"command": "select.deselect"})).unwrap();
    let decode = |r: &Value| {
        let png = base64::engine::general_purpose::STANDARD.decode(r["base64"].as_str().unwrap()).unwrap();
        let img = photocraft_codecs::decode(&png).unwrap();
        (img.dimensions(), img.to_rgba8())
    };
    let r = h.handle("doc.render", json!({"rect": [8, 8, 20, 10]})).unwrap();
    assert_eq!((r["width"].clone(), r["height"].clone(), r["rect"].clone()), (json!(20), json!(10), json!([8, 8, 20, 10])), "{r}");
    assert!(r["ms"].is_u64());
    let img = decode(&r);
    assert_eq!(img.0, (20, 10));
    let px = |img: &((u32, u32), Vec<u8>), x: u32, y: u32| -> [u8; 4] {
        let i = ((y * img.0.0 + x) * 4) as usize;
        img.1[i..i + 4].try_into().unwrap()
    };
    assert_eq!(px(&img, 0, 0), [255, 255, 255, 255], "white outside the fill");
    assert_eq!(px(&img, 5, 5), [255, 0, 0, 255], "red inside");
    // Half scale, and a rect past the canvas is clipped to it.
    let r = h.handle("doc.render", json!({"rect": [40, 40, 100, 100], "scale": 0.5})).unwrap();
    assert_eq!((r["rect"].clone(), r["width"].clone(), r["height"].clone()), (json!([40, 40, 24, 8]), json!(12), json!(4)), "{r}");
    // Only the red layer (by session id, then by uid): transparent outside its pixels.
    let red = layers(&mut h).into_iter().find(|l| l["name"] == "red").unwrap();
    for sel in [json!({"layers": [red["id"]]}), json!({"uids": [red["uid"]]})] {
        let mut p = json!({"rect": [8, 8, 20, 10]});
        p.as_object_mut().unwrap().extend(sel.as_object().unwrap().clone());
        let img = decode(&h.handle("doc.render", p).unwrap());
        assert_eq!(px(&img, 0, 0)[3], 0, "the background is not drawn");
        assert_eq!(px(&img, 5, 5), [255, 0, 0, 255]);
    }
    // Bad params are errors, never panics.
    for bad in [
        json!({"rect": [0, 0, 0, 5]}),
        json!({"rect": [0, 0, 5]}),
        json!({"rect": "all"}),
        json!({"rect": [1000, 1000, 5, 5]}),
        json!({"rect": [0, 0, 5, 5], "scale": 0}),
        json!({"rect": [0, 0, 5, 5], "scale": 2}),
        json!({"rect": [0, 0, i64::MAX, 5]}),
        json!({"layers": [999_999]}),
        json!({"uids": [999_999]}),
        json!({"layers": "red"}),
    ] {
        assert!(h.handle("doc.render", bad.clone()).is_err(), "{bad}");
    }
    // Without the new params the reply is as before.
    let r = h.handle("doc.render", json!({"maxSide": 16})).unwrap();
    assert!(r.get("rect").is_none() && r["base64"].is_string());
}

#[test]
fn jobs_status_and_wait_follow_a_background_job() {
    let h = session_mutex();
    {
        let mut g = h.lock().unwrap();
        g.handle("doc.new", json!({"width": 600, "height": 400})).unwrap();
        g.handle("engine.execute", json!({"command": "layer.new.layer"})).unwrap();
        g.handle("engine.execute", json!({"command": "edit.fill", "params": {"color": "#808080"}})).unwrap();
    }
    let start = |h: &Mutex<Headless>| {
        let r = h.lock().unwrap().handle("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 30}, "wait": false})).unwrap();
        r["job"].as_u64().unwrap()
    };
    // Over the wire (the session unlocked while waiting).
    let job = start(&h);
    let now = respond(&h, &json!({"id": 1, "method": "jobs.wait", "params": {"job": job, "timeoutMs": 0}}).to_string());
    assert_eq!(now["ok"], true, "{now}");
    assert!(["running", "done"].contains(&now["result"]["state"].as_str().unwrap()), "{now}");
    let done = respond(&h, &json!({"id": 2, "method": "job.wait", "params": {"job": job, "timeoutMs": 60000}}).to_string());
    assert_eq!(done["result"]["state"], "done", "{done}");
    assert!(done["result"]["result"].is_object(), "a done job carries its result: {done}");
    let status = respond(&h, &json!({"id": 3, "method": "jobs.status", "params": {"job": job}}).to_string());
    assert_eq!(status["result"]["state"], "done", "{status}");
    let history = h.lock().unwrap().handle("doc.inspect", json!({})).unwrap()["history"].clone();
    assert_eq!(history.as_array().unwrap().last().unwrap(), "Gaussian Blur", "applied: {history}");
    // In-process (`handle`, as a batch step).
    let job = start(&h);
    let r = h.lock().unwrap().handle("batch", json!({"steps": [{"method": "jobs.wait", "params": {"job": job}}]})).unwrap();
    assert_eq!(r["results"][0]["result"]["state"], "done", "{r}");
    // A cancelled job says so.
    let job = start(&h);
    h.lock().unwrap().handle("jobs.cancel", json!({"job": job})).unwrap();
    let r = h.lock().unwrap().handle("job.status", json!({"job": job})).unwrap();
    assert_eq!(r["state"], "cancelled", "{r}");
    for bad in [json!({}), json!({"job": "x"}), json!({"job": 999_999}), json!({"job": job, "timeoutMs": -1})] {
        assert!(h.lock().unwrap().handle("jobs.wait", bad.clone()).is_err(), "{bad}");
        assert_eq!(respond(&h, &json!({"id": 9, "method": "jobs.wait", "params": bad}).to_string())["ok"], false);
    }
}

fn session_mutex() -> Mutex<Headless> {
    Mutex::new(Headless::trusted_local())
}

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A served TCP session with `opts`; returns an authenticated client and its reader.
fn tcp_client(opts: ServeOptions) -> (std::net::TcpStream, std::io::BufReader<std::net::TcpStream>) {
    let h = Arc::new(Mutex::new(Headless::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = serve_tcp_with("127.0.0.1:0", h, TOKEN.into(), opts, move |a| tx.send(a).unwrap());
    });
    let s = std::net::TcpStream::connect(rx.recv().unwrap()).unwrap();
    let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
    let mut w = s.try_clone().unwrap();
    std::io::Write::write_all(&mut w, format!("{{\"id\":0,\"method\":\"auth\",\"params\":{{\"token\":\"{TOKEN}\"}}}}\n").as_bytes()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("authenticated"), "{line}");
    (s, reader)
}

fn ask(s: &mut std::net::TcpStream, r: &mut std::io::BufReader<std::net::TcpStream>, req: &str) -> std::io::Result<String> {
    std::io::Write::write_all(s, format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    r.read_line(&mut line)?;
    Ok(line)
}

#[test]
fn the_idle_timeout_is_configurable_and_can_be_off() {
    let (mut s, mut r) = tcp_client(ServeOptions { connection_idle: Some(Duration::from_millis(200)), ..Default::default() });
    std::thread::sleep(Duration::from_millis(600));
    let gone = ask(&mut s, &mut r, r#"{"id":1,"method":"methods"}"#);
    assert!(gone.as_ref().is_err() || gone.as_ref().is_ok_and(|l| l.is_empty()), "dropped after 200 ms idle: {gone:?}");
    let (mut s, mut r) = tcp_client(ServeOptions { connection_idle: None, ..Default::default() });
    std::thread::sleep(Duration::from_millis(600));
    let line = ask(&mut s, &mut r, r#"{"id":1,"method":"methods"}"#).unwrap();
    assert!(line.contains("\"ok\":true"), "kept without an idle timeout: {line}");
}

/// Before: a reply went out in two writes (the JSON, then its newline) on a socket with Nagle's
/// algorithm, so each request waited out the client's delayed ACK (about 40 ms on Linux).
#[test]
fn replies_go_out_at_once_without_the_client_setting_tcp_nodelay() {
    let (mut s, mut r) = tcp_client(ServeOptions::default());
    assert!(!s.nodelay().unwrap(), "the client leaves Nagle on");
    let n = 40;
    let t = std::time::Instant::now();
    for i in 0..n {
        let line = ask(&mut s, &mut r, &format!(r#"{{"id":{i},"method":"session.list"}}"#)).unwrap();
        assert!(line.contains("\"ok\":true"), "{line}");
    }
    let per = t.elapsed() / n;
    assert!(per < Duration::from_millis(15), "{per:?} per request");
}

#[test]
fn accepted_streams_set_tcp_nodelay() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let _c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (a, _) = l.accept().unwrap();
    configure_stream(&a).unwrap();
    assert!(a.nodelay().unwrap());
    assert_eq!(a.read_timeout().unwrap(), Some(IO_TIMEOUT));
    configure_stream_idle(&a, None).unwrap();
    assert_eq!(a.read_timeout().unwrap(), None);
}

fn png_b64(w: u32, h: u32, rgb: [u8; 3]) -> String {
    let px: Vec<u8> = (0..w * h).flat_map(|_| rgb).collect();
    let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgb, px).unwrap();
    base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap())
}

/// `file.placeEmbedded` was refused in automation (it reads an ambient `path`); with the image in
/// the params it reads nothing and runs, with a box (`x`, `y`, `width`, `height`) and `rasterize`.
#[test]
fn place_embedded_takes_the_image_and_a_box_without_any_file_access() {
    let mut h = Headless::new();
    h.handle("doc.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    let b64 = png_b64(8, 4, [0, 0, 255]);
    let r = h.handle("doc.place", json!({"base64": b64, "x": 10, "y": 6, "width": 32, "height": 8})).unwrap();
    assert_eq!(r["bounds"], json!([10.0, 6.0, 42.0, 14.0]), "{r}");
    assert_eq!((r["scale"].clone(), r["scaleY"].clone(), r["smartObject"].clone()), (json!(400.0), json!(200.0), json!(true)), "{r}");
    // Only a width keeps the aspect; rasterize makes a pixel layer.
    let r = h
        .handle("engine.execute", json!({"command": "file.placeEmbedded", "params": {"data": format!("data:image/png;base64,{b64}"), "x": 0, "y": 30, "width": 16, "rasterize": true}}))
        .unwrap();
    assert_eq!((r["bounds"].clone(), r["smartObject"].clone()), (json!([0.0, 30.0, 16.0, 38.0]), json!(false)), "{r}");
    assert!(r.get("scaleY").is_none(), "{r}");
    let top = layers(&mut h).into_iter().find(|l| l["id"] == r["layer"]).unwrap();
    assert_eq!(top["kind"], "Pixel", "{top}");
    let img = h.handle("doc.render", json!({"rect": [0, 30, 16, 8]})).unwrap();
    let png = base64::engine::general_purpose::STANDARD.decode(img["base64"].as_str().unwrap()).unwrap();
    let px = photocraft_codecs::decode(&png).unwrap().to_rgba8();
    assert_eq!(&px[(3 * 16 + 8) * 4..(3 * 16 + 8) * 4 + 4], [0, 0, 255, 255], "blue where it was placed");
    // A path is still refused, and bad boxes or data are errors.
    assert!(h.handle("engine.execute", json!({"command": "file.placeEmbedded", "params": {"path": "/etc/hosts"}})).is_err());
    assert!(h.handle("engine.execute", json!({"command": "file.placeEmbedded", "params": {"path": "/etc/hosts", "data": b64}})).is_err());
    for bad in [
        json!({"base64": b64, "width": 0}),
        json!({"base64": b64, "width": -3}),
        json!({"base64": b64, "x": "left"}),
        json!({"base64": b64, "width": 1e7, "height": 1e7}),
        json!({"base64": "not base64!"}),
        json!({"base64": ""}),
        json!({"base64": 7}),
    ] {
        assert!(h.handle("doc.place", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(layers(&mut h).len(), 3, "nothing else was placed");
}

#[test]
fn save_reports_bytes_and_time_and_takes_a_fast_merged_image() {
    let dir = temp_dir("save-stats");
    let mut h = Headless::trusted_local();
    h.handle("doc.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    h.handle("engine.execute", json!({"command": "layer.new.layer", "params": {"name": "ink"}})).unwrap();
    h.handle("engine.execute", json!({"command": "edit.fill", "params": {"color": "#204080"}})).unwrap();
    let (exact, fast) = (dir.join("exact.psd"), dir.join("fast.psd"));
    let r = h.handle("doc.save", json!({"path": exact.to_string_lossy()})).unwrap();
    assert_eq!(r["bytes"].as_u64(), Some(std::fs::metadata(&exact).unwrap().len()), "{r}");
    assert!(r["ms"].is_u64(), "{r}");
    let r = h.handle("doc.save", json!({"path": fast.to_string_lossy(), "mergedImage": "fast"})).unwrap();
    assert_eq!(r["bytes"].as_u64(), Some(std::fs::metadata(&fast).unwrap().len()), "{r}");
    // Both reopen to the same layers and composite.
    let render = |h: &mut Headless, p: &std::path::Path| {
        h.handle("doc.open", json!({"path": p.to_string_lossy()})).unwrap();
        let r = h.handle("doc.render", json!({"maxSide": 0})).unwrap()["base64"].clone();
        h.handle("doc.close", json!({})).unwrap();
        r
    };
    assert_eq!(render(&mut h, &exact), render(&mut h, &fast));
    for bad in [json!("quick"), json!(true)] {
        assert!(h.handle("doc.save", json!({"path": exact.to_string_lossy(), "mergedImage": bad})).is_err());
    }
    let r = h.handle("doc.save", json!({"path": dir.join("page.pcraft").to_string_lossy()})).unwrap();
    assert!(r["bytes"].is_u64() && r["ms"].is_u64(), "{r}");
    std::fs::remove_dir_all(dir).unwrap();
}
