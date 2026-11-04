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
