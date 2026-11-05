//! `allowDownload` on the CLI's inpainter: a request with it goes to the models URL.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

/// One `serve` session on an empty config folder; returns what the models server was asked.
fn inpaint(allow_download: bool) -> (Value, Vec<String>) {
    let name = format!("pc-cli-ai-{}-{allow_download}", std::process::id());
    let config = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&config);
    std::fs::create_dir_all(&config).unwrap();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", server.local_addr().unwrap());
    let (asked, requests) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in server.incoming().flatten() {
            let mut stream = stream;
            let mut head = [0u8; 1024];
            let n = stream.read(&mut head).unwrap_or(0);
            let _ = asked.send(String::from_utf8_lossy(&head[..n]).lines().next().unwrap_or_default().to_string());
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let mut child = Command::new(crate::binary())
        .arg("serve")
        .env("PHOTOCRAFT_CONFIG_DIR", &config)
        .env("PHOTOCRAFT_AI_MODELS_URL", &url)
        .env_remove("CAPISHOT_LAMA_WEIGHTS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut next = 0;
    let mut call = |method: &str, params: Value| -> Value {
        next += 1;
        writeln!(input, "{}", json!({"id": next, "method": method, "params": params})).unwrap();
        input.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0, "the CLI ended");
            let Ok(reply) = serde_json::from_str::<Value>(&line) else { continue };
            if reply["id"] == next {
                return reply;
            }
        }
    };
    let execute = |command: &str, params: Value| json!({"command": command, "params": params});
    assert_eq!(call("engine.execute", execute("file.new", json!({"width": 64, "height": 64})))["ok"], true);
    assert_eq!(call("engine.execute", execute("select.rect", json!({"x": 8, "y": 8, "width": 16, "height": 16})))["ok"], true);
    let mut params = json!({"model": "manga-lama", "device": "cpu"});
    if allow_download {
        params["allowDownload"] = json!(true);
    }
    let mut run = execute("edit.inpaintAi", params);
    run["wait"] = json!(false);
    let reply = call("engine.execute", run);
    let reply = match reply["result"]["job"].clone() {
        Value::Null => reply,
        job => loop {
            let row = call("jobs.wait", json!({"job": job, "timeoutMs": 2000}));
            if row["result"]["state"] != "running" {
                break row;
            }
        },
    };
    drop(input);
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&config);
    (reply, requests.recv_timeout(Duration::from_millis(100)).into_iter().collect())
}

#[test]
fn allow_download_asks_the_models_server_for_the_inpainter_weights() {
    let (reply, asked) = inpaint(true);
    assert!(asked.iter().any(|line| line.contains("manga-lama")), "asked {asked:?}, replied {reply}");
    let text = reply.to_string();
    assert!(!text.contains("weights not found"), "{text}");
    assert!(text.contains("404"), "the download's failure reaches the agent: {text}");
}

#[test]
fn without_allow_download_nothing_is_fetched() {
    let (reply, asked) = inpaint(false);
    assert!(asked.is_empty(), "asked {asked:?}");
    assert!(reply.to_string().contains("allowDownload"), "the error says how: {reply}");
}
