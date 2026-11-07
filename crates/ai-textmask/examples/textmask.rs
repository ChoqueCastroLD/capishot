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
    for r in &d.regions {
        println!("{} {:?} {} {:.3}", r.id, r.bbox, r.area, r.score);
    }
    let bytes: Vec<u8> = d.labels.iter().flat_map(|l| l.to_le_bytes()).collect();
    std::fs::write(&a[5], bytes)?;
    Ok(())
}
