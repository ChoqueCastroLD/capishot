//! Memory of a type-heavy session (the phone crash report): open a page, then type, Bubble Type,
//! paste, edit, move and undo, printing the process memory (resident and peak) and the
//! history's resident bytes after each step, with an autosave checkpoint every few steps.
//!
//! ```text
//! cargo run --release -p photocraft-engine --example type_memory -- <page.jpg> [ops]
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

use photocraft_engine::Session;
use serde_json::json;

fn rss() -> (usize, usize) {
    let st = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb =
        |k: &str| st.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0) * 1024;
    (kb("VmRSS"), kb("VmHWM"))
}

fn mb(b: usize) -> f64 {
    (b as f64 / 1e6 * 10.0).round() / 10.0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let page = args.get(1).expect("a page");
    let ops: usize = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(60);
    let mut s = Session::new();
    let bytes = std::fs::read(page).unwrap();
    s.add_document(photocraft_io::import(page, &bytes).unwrap().document, None);
    let (w, h) = {
        let d = &s.active().unwrap().doc;
        (d.size.width as f64, d.size.height as f64)
    };
    let mut saver = photocraft_format::ObjectAutosaver::default();
    println!("open: rss {} MB, peak {} MB", mb(rss().0), mb(rss().1));
    let mut seed = 7u64;
    let mut rnd = |n: f64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as f64 / (1u64 << 31) as f64 * n
    };
    let bubbles = [(0.79, 0.10), (0.34, 0.47), (0.16, 0.78)];
    for op in 0..ops {
        let text = format!("Line {op}: Lorem ipsum dolor sit amet, consectetur adipiscing elit. ¿Qué pasa? ¡Vamos!");
        let layers: Vec<u64> = s
            .active()
            .unwrap()
            .doc
            .walk()
            .into_iter()
            .filter(|(_, _, l)| matches!(l.content, photocraft_doc::LayerContent::Text(_)))
            .map(|(_, _, l)| l.id.0)
            .collect();
        let pick = |r: f64| layers.get((r * layers.len() as f64) as usize).copied();
        let r = match op % 6 {
            0 => s.execute("type.create", json!({"x": rnd(w), "y": rnd(h), "text": text, "size": 30})),
            1 => {
                let (bx, by) = bubbles[op % 3];
                s.execute("type.createInBubble", json!({"x": bx * w, "y": by * h, "text": text}))
            }
            2 => s.execute("type.create", json!({"x": rnd(w), "y": rnd(h), "text": text.repeat(3), "size": 30})),
            3 => match pick(rnd(1.0)) {
                Some(id) => s.execute("type.edit", json!({"layer": id, "text": format!("{text} (edited)")})),
                None => Ok(json!(null)),
            },
            4 => match pick(rnd(1.0)) {
                Some(id) => s.execute("type.edit", json!({"layer": id, "move": [10, 10]})),
                None => Ok(json!(null)),
            },
            _ => s.execute("edit.undo", json!({})),
        };
        if let Err(e) = r {
            println!("op {op}: {e}");
        }
        let mut note = String::new();
        if op % 10 == 9 {
            let st = s.active().unwrap();
            let info =
                photocraft_format::RecoveryInfo { key: "web-x-1".into(), document_name: "p".into(), original_path: None, saved_at: 0, revision: st.revision };
            let hist = st.history.checkpoint();
            let c = saver
                .checkpoint_with_history(&st.doc, Some(&hist), photocraft_format::HistoryLimits { max_states: 20, max_bytes: 50 << 20 }, info, json!({}))
                .unwrap();
            let written: usize = c.objects.iter().map(|(_, d)| d.len()).sum();
            note = format!(", autosave wrote {} MB", mb(written));
            saver.committed(c.live);
        }
        println!("op {op} kind {}: rss {} MB, peak {} MB, history {} MB{note}", op % 6, mb(rss().0), mb(rss().1), mb(s.history_resident_bytes()));
    }
}
