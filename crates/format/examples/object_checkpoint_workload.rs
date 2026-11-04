//! What a browser recovery checkpoint costs on an 18 MP manga page (3000 x 6000, screentone-like
//! pixels): the first one (every tile), then one after each small edit with the phone's history
//! limits (5 steps within 16 MB), as `recovery.rs` writes them after every edit.
//! cargo run --release -p photocraft-format --example object_checkpoint_workload
use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_format::{HistoryLimits, ObjectAutosaver, RecoveryInfo};
use photocraft_ops::History;
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = (3000u32, 6000u32);
    let mut doc = Arc::new(Document::with_background("page", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE));
    {
        // Screentone dots and ink lines: compresses like a scanned page, not like a flat fill.
        let layer = Arc::make_mut(&mut doc).layers.first_mut().ok_or("no background")?;
        let s = layer.surface_mut().ok_or("no pixels")?;
        for y in (0..h).step_by(3) {
            for x in (0..w).step_by(3) {
                let v = if (x / 7 + y / 11) % 5 == 0 { 0.1 } else { 0.85 + ((x * 31 + y * 17) % 13) as f32 / 100.0 };
                s.write_pixel(x as i32, y as i32, &[v, v, v, 1.0]);
            }
        }
    }
    let limits = HistoryLimits { max_states: 5, max_bytes: 16 << 20 };
    let mut history = History::default();
    let mut saver = ObjectAutosaver::default();
    let info = |revision| RecoveryInfo { key: "web-bench-1".into(), document_name: "page".into(), original_path: None, saved_at: 0, revision };
    for revision in 0..=6u64 {
        if revision > 0 {
            // A small edit: a 300 x 300 patch, like a removed SFX.
            let before = doc.clone();
            let layer = Arc::make_mut(&mut doc).layers.first_mut().ok_or("no background")?;
            let s = layer.surface_mut().ok_or("no pixels")?;
            for y in 0..300 {
                for x in 0..300 {
                    s.write_pixel((revision as i32 * 350 + x) % w as i32, 1000 + y, &[1.0, 1.0, 1.0, 1.0]);
                }
            }
            history.record(format!("Edit {revision}"), before, photocraft_ops::LayerTarget::default());
        }
        let start = Instant::now();
        let c = saver.checkpoint_with_history(&doc, Some(&history.checkpoint()), limits, info(revision), serde_json::Value::Null)?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let bytes: usize = c.objects.iter().map(|(_, d)| d.len()).sum();
        println!(
            "{}",
            serde_json::json!({"revision": revision, "encode_ms": (ms * 10.0).round() / 10.0, "objects": c.objects.len(), "object_mb": bytes as f64 / 1e6, "live": c.live.len(), "descriptor_kb": c.descriptor.len() / 1024})
        );
        saver.committed(c.live);
    }
    Ok(())
}
