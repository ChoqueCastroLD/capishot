//! Runs the mask model on a raw page and writes its labels, to compare with the model's own script.
//!
//! ```sh
//! textmask MODEL.onnx PAGE.rgb WIDTH HEIGHT OUT.labels
//! ```

use photocraft_ai_textmask::native::NativeTextMasker;
use photocraft_ai_textmask::{Page, TextMasker};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        return Err("usage: textmask MODEL.onnx PAGE.rgb WIDTH HEIGHT OUT.labels".into());
    }
    let rgb = std::fs::read(&a[2])?;
    let (width, height): (usize, usize) = (a[3].parse()?, a[4].parse()?);
    let m = NativeTextMasker::new(a[1].clone().into());
    let t = std::time::Instant::now();
    let d = m.detect(Page { rgb: &rgb, width, height }, &|| false)?;
    println!("{} regions in {:.2?} on {}", d.regions.len(), t.elapsed(), m.device());
    // Where the time went and what stays in memory, and once more with the model loaded.
    println!("{:?}, {}", m.timing(), resident());
    let t = std::time::Instant::now();
    m.detect(Page { rgb: &rgb, width, height }, &|| false)?;
    println!("again in {:.2?}: {:?}, {}", t.elapsed(), m.timing(), resident());
    for r in &d.regions {
        println!("{} {:?} {} {:.3}", r.id, r.bbox, r.area, r.score);
    }
    let bytes: Vec<u8> = d.labels.iter().flat_map(|l| l.to_le_bytes()).collect();
    std::fs::write(&a[5], bytes)?;
    Ok(())
}

/// The process's memory now and at its most, where the system says (Linux).
fn resident() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |key: &str| status.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).map_or(0, |kb| kb / 1024);
    format!("resident {} MB (peak {} MB)", field("VmRSS"), field("VmHWM"))
}
