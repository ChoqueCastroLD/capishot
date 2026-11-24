//! Remove with AI on a large region of a page, on a 4 GB device: the run's peak memory stays under
//! a quarter of that device's Memory Usage (1 GB, a quarter of its RAM), what an AI run may take
//! beside the open pages and the model. A 4 GB Celeron with seven pages open died in Remove with
//! AI: the fill held its region as floats several times over (some 60 bytes a pixel).
//!
//! The one test here runs in its own process: it reads the process's peak RSS (`VmHWM`, Linux
//! only) and nothing else may allocate meanwhile.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use photocraft_ai_lama::service::FakeInpainter;
use photocraft_engine::Session;
use serde_json::json;

fn rss_bytes(key: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with(key))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// The page's side, and the side of what is selected in its middle.
const PAGE: u32 = 4000;
const HOLE: u32 = 2800;

#[test]
fn remove_with_ai_on_a_large_region_stays_within_a_4_gb_devices_ai_budget() {
    let mut s = Session::new();
    s.edit_prefs(|p| p.performance.memory_usage_mb = 1024);
    s.execute("file.new", json!({"width": PAGE, "height": PAGE, "background": "white"})).unwrap();
    // Art over the whole page: every tile has pixels of its own.
    for k in 0..40 {
        s.execute("select.rect", json!({"x": k * 100, "y": 0, "width": 40, "height": PAGE})).unwrap();
        s.execute("edit.fill", json!({"color": "#202020"})).unwrap();
    }
    let at = (PAGE - HOLE) / 2;
    s.execute("select.rect", json!({"x": at, "y": at, "width": HOLE, "height": HOLE})).unwrap();
    s.set_inpainter(Some(Arc::new(FakeInpainter::default())));
    // Writing 5 resets the peak RSS to the current RSS; skip where the kernel doesn't allow it.
    if std::fs::write("/proc/self/clear_refs", "5").is_err() {
        return;
    }
    let Some(before) = rss_bytes("VmHWM:") else { return };
    let t = std::time::Instant::now();
    let r = s.execute("edit.removeAi", json!({})).unwrap();
    let ms = t.elapsed().as_millis();
    let grown = rss_bytes("VmHWM:").unwrap().saturating_sub(before);
    let region = r["region"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect::<Vec<_>>();
    let pixels = region[2] * region[3];
    assert_eq!(r["pixels"].as_u64(), Some(u64::from(HOLE) * u64::from(HOLE)), "{r}");

    let ceiling = 1024u64 << 20 >> 2;
    eprintln!(
        "Remove with AI over {pixels} px ({} MP): peak RSS growth {} MiB ({:.1} B/px of the region) in {ms} ms",
        pixels >> 20,
        grown >> 20,
        grown as f64 / pixels as f64
    );
    assert!(grown < ceiling, "peak RSS grew by {} MiB, over {} MiB (a quarter of a 4 GB device's Memory Usage)", grown >> 20, ceiling >> 20);
}

/// The tool's own path: a wide stroke painted over the page (its window and hole read first, then
/// filled as a selection of the hole's shape).
#[test]
fn remove_with_ai_of_a_wide_stroke_stays_within_a_4_gb_devices_ai_budget() {
    let mut s = Session::new();
    s.edit_prefs(|p| p.performance.memory_usage_mb = 1024);
    s.execute("file.new", json!({"width": PAGE, "height": PAGE, "background": "white"})).unwrap();
    for k in 0..40 {
        s.execute("select.rect", json!({"x": k * 100, "y": 0, "width": 40, "height": PAGE})).unwrap();
        s.execute("edit.fill", json!({"color": "#202020"})).unwrap();
    }
    s.execute("select.deselect", json!({})).unwrap();
    s.set_inpainter(Some(Arc::new(FakeInpainter::default())));
    if std::fs::write("/proc/self/clear_refs", "5").is_err() {
        return;
    }
    let Some(before) = rss_bytes("VmHWM:") else { return };
    let t = std::time::Instant::now();
    let r = s.execute("edit.removeAi", json!({"points": [[600, 1400], [3400, 1400], [3400, 2600], [600, 2600]], "size": 900})).unwrap();
    let ms = t.elapsed().as_millis();
    let grown = rss_bytes("VmHWM:").unwrap().saturating_sub(before);
    let d = r["damage"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect::<Vec<_>>();
    let pixels = d[2] * d[3];
    let ceiling = 1024u64 << 20 >> 2;
    eprintln!(
        "Remove with AI stroke over {pixels} px ({} MP): peak RSS growth {} MiB ({:.1} B/px of the window) in {ms} ms",
        pixels >> 20,
        grown >> 20,
        grown as f64 / pixels as f64
    );
    assert!(grown < ceiling, "peak RSS grew by {} MiB, over {} MiB (a quarter of a 4 GB device's Memory Usage)", grown >> 20, ceiling >> 20);
}
