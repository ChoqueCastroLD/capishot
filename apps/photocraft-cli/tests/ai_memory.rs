//! Memory of Text Selection on a real page: finding the text (`select.detectText`), choosing
//! every piece (`select.textPieces`) and Clean with AI (`edit.removeAi` by blocks), as the tool
//! runs them. A page with hundreds of pieces took the desktop app down (an allocation failed).
//!
//! Opt-in, on a local page that is never committed: `CAPISHOT_AI_MEMORY_PAGE` names the image,
//! `CAPISHOT_TEXTMASK_MODEL` the mask model and `CAPISHOT_LAMA_WEIGHTS` Manga LaMa's weights
//! (ONNX Runtime from `PHOTOCRAFT_AI_ONNXRUNTIME`). Without the first the test passes without
//! doing anything. `CAPISHOT_AI_MEMORY_CLEAN=0` skips the clean (minutes on a CPU). With
//! `CAPISHOT_AI_DEVICE=cpu` Manga LaMa runs on ONNX Runtime, the model written as for DirectML:
//! before it was written once, into one buffer, loading it took 1.9 GB of this heap (2.3 GB with
//! an 8.8 megapixel page's buffers; 0.9 GB now).
//!
//! Each step reports the peak of this process's Rust heap over what was live before it and the
//! process's resident peak (which has ONNX Runtime's own memory). The Rust heap must stay within
//! a few buffers the size of the page.
#![cfg(all(feature = "ai", target_os = "linux"))]

use std::alloc::System;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

use photocraft_automation::Headless;
use serde_json::{Value, json};
use stats_alloc::{INSTRUMENTED_SYSTEM, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The Rust heap's peak, as a thread that looks every half millisecond sees it (a large buffer
/// lives far longer than that).
static PEAK: AtomicUsize = AtomicUsize::new(0);
static WATCHING: AtomicBool = AtomicBool::new(true);

/// What the Rust heap holds now.
fn live() -> usize {
    let s = GLOBAL.stats();
    (s.bytes_allocated as isize - s.bytes_deallocated as isize + s.bytes_reallocated).max(0) as usize
}

/// A field of `/proc/self/status`, in MB.
fn status_mb(key: &str) -> usize {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    text.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0) / 1024
}

struct Step {
    name: &'static str,
    ms: u128,
    /// Rust heap: peak over what was live at the start, and what stayed.
    heap_peak: usize,
    kept: isize,
    rss_peak: usize,
    rss_after: usize,
    reply: Value,
}

fn step(h: &mut Headless, name: &'static str, cmd: &str, params: Value) -> Step {
    // The resident peak starts over.
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let before = live();
    PEAK.store(before, Relaxed);
    let t = std::time::Instant::now();
    let reply = h.command_run(cmd, params).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    Step {
        name,
        ms: t.elapsed().as_millis(),
        heap_peak: PEAK.load(Relaxed).saturating_sub(before),
        kept: live() as isize - before as isize,
        rss_peak: status_mb("VmHWM"),
        rss_after: status_mb("VmRSS"),
        reply,
    }
}

#[test]
fn text_selection_and_its_clean_stay_within_a_few_pages_of_memory() {
    let Some(page) = std::env::var_os("CAPISHOT_AI_MEMORY_PAGE").map(PathBuf::from) else { return };
    let watcher = std::thread::spawn(|| {
        while WATCHING.load(Relaxed) {
            PEAK.fetch_max(live(), Relaxed);
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
    });
    let model = PathBuf::from(std::env::var_os("CAPISHOT_TEXTMASK_MODEL").expect("CAPISHOT_TEXTMASK_MODEL"));
    let mut h = Headless::trusted_local();
    let masker = Arc::new(photocraft_ai_textmask::native::NativeTextMasker::new(model));
    h.session.set_text_masker(Some(masker));
    let clean = std::env::var("CAPISHOT_AI_MEMORY_CLEAN").map_or(true, |v| v != "0");
    if clean {
        let inpainter = photocraft_ai_lama::native::NativeInpainter::locate(None).expect("CAPISHOT_LAMA_WEIGHTS");
        h.session.set_inpainter(Some(Arc::new(inpainter)));
    }
    h.open(&page).unwrap_or_else(|e| panic!("{}: {e}", page.display()));
    let size = h.session.active().map(|st| st.doc.size).expect("a document");
    let px = size.width as usize * size.height as usize;
    let mut steps = vec![step(&mut h, "detect", "select.detectText", json!({}))];
    let pieces = steps[0].reply["pieces"].as_array().map_or(0, Vec::len);
    steps.push(step(&mut h, "detect again", "select.detectText", json!({})));
    steps.push(step(&mut h, "and again", "select.detectText", json!({})));
    steps.push(step(&mut h, "choose all", "select.textPieces", json!({"pieces": "all"})));
    if clean {
        steps.push(step(&mut h, "clean", "edit.removeAi", json!({"byBlocks": true, "output": "current"})));
    }
    if let Some(out) = std::env::var_os("CAPISHOT_AI_MEMORY_DUMP") {
        let _ = std::fs::write(out, steps[0].reply.to_string());
    }
    let mb = |b: usize| b as f64 / (1 << 20) as f64;
    println!("{}x{} ({:.1} MP), {pieces} pieces; a byte per pixel is {:.1} MB", size.width, size.height, px as f64 / 1e6, mb(px));
    println!("{:<13} {:>8} {:>12} {:>9} {:>9} {:>10} {:>10}", "step", "ms", "heap peak MB", "bytes/px", "kept MB", "rss peak", "rss after");
    for s in &steps {
        println!(
            "{:<13} {:>8} {:>12.0} {:>9.1} {:>9.0} {:>10} {:>10}",
            s.name,
            s.ms,
            mb(s.heap_peak),
            s.heap_peak as f64 / px as f64,
            s.kept as f64 / (1 << 20) as f64,
            s.rss_peak,
            s.rss_after
        );
    }
    if let Some(t) = steps[0].reply.get("stages") {
        let bubble = steps[0].reply["pieces"].as_array().map_or(0, |a| a.iter().filter(|p| p["method"] == "bubble").count());
        println!("detect stages: {t}, device {}, {} bubbles, {bubble} pieces for the Bubble Text Remover", steps[0].reply["device"], steps[0].reply["bubbles"]);
    }
    if let Some(t) = steps[1].reply.get("stages") {
        println!("detect again stages: {t}");
    }
    WATCHING.store(false, Relaxed);
    let _ = watcher.join();
    for s in &steps {
        // An RGB float page is 12 bytes a pixel: no step may hold more than a few of those. The
        // first clean also loads Manga LaMa: its file, its weights and the model written from them
        // (200 MB each, two at a time), or on burn's CPU backend a tile's activations.
        let model = if s.name == "clean" { 700 << 20 } else { 64 << 20 };
        assert!(s.heap_peak <= px * 56 + model, "{}: {:.0} MB on the Rust heap, {} bytes a pixel", s.name, mb(s.heap_peak), s.heap_peak / px);
    }
    // What a detection leaves is its labels, which take the place of the last one's: the process
    // doesn't grow from one to the next (the runtime's arena did, by gigabytes). The heap's own
    // count isn't used for this: it misses what the runtime frees on its side.
    assert!(steps[2].rss_after <= steps[0].rss_after + 160, "a third detection left {} MB resident, the first {}", steps[2].rss_after, steps[0].rss_after);
}
