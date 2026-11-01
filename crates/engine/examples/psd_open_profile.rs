//! Times opening PSD/PSB files the way File › Open does, by phase: decode (the file read in
//! place, channels decompressed, layers, type and effects built), the layer bounds warm-up, the
//! text engine's font wait, adding the document to a session (colour policy), and the first full
//! composite the canvas needs. Each file's best of `--reps` runs.
//!
//! `--bytes` decodes from the file read into memory first (as the browser opens a picked file),
//! `--copied` from those bytes copied (the open before shared bytes); `--check` compares the
//! layers' pixels of the in-memory decode with the file decode.
//!
//! ```sh
//! cargo run --release -p photocraft-engine --example psd_open_profile -- [--reps 3] [--bytes|--copied] [--check] <file.psd>...
//! ```

use std::time::Instant;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reps: usize = args.iter().position(|a| a == "--reps").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(3).max(1);
    let files: Vec<&String> = args.iter().enumerate().filter(|(i, a)| !a.starts_with("--") && !(*i > 0 && args[i - 1] == "--reps")).map(|(_, a)| a).collect();
    let (mode_bytes, mode_copied, check) = (args.iter().any(|a| a == "--bytes"), args.iter().any(|a| a == "--copied"), args.iter().any(|a| a == "--check"));
    if files.is_empty() {
        eprintln!("usage: psd_open_profile [--reps N] <file.psd>...");
        std::process::exit(2);
    }
    // The text engine's installed-font scan, once, as at launch.
    let t = Instant::now();
    drop(photocraft_text::shared());
    println!("text engine ready: {:.0} ms", ms(t));
    println!("{:<44} {:>8} {:>8} {:>8} {:>8} {:>9} {:>8}  layers", "file", "decode", "bounds", "fonts", "session", "composite", "total");
    for f in files {
        let mut best: Option<[f64; 6]> = None;
        let mut layers = 0;
        for _ in 0..reps {
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let t0 = Instant::now();
            let t = Instant::now();
            let none = &photocraft_raster::Interrupt::NONE;
            let opened = if mode_bytes || mode_copied {
                let bytes = std::sync::Arc::new(std::fs::read(f).unwrap());
                if mode_copied {
                    photocraft_io::import_for_open_with(&name, &bytes, &Default::default(), none)
                } else {
                    photocraft_io::import_shared_for_open_with(&name, &bytes, &Default::default(), none)
                }
            } else {
                photocraft_io::import_file_for_open_with(&name, std::path::Path::new(f), &Default::default(), none)
            };
            let (r, _) = match opened {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{f}: {e}");
                    break;
                }
            };
            let decode = ms(t);
            let t = Instant::now();
            photocraft_engine::jobs::warm_layer_bounds(&r.document, || Ok(())).unwrap();
            let bounds = ms(t);
            let t = Instant::now();
            if photocraft_engine::type_extra_cmds::has_type_layers(&r.document) {
                drop(photocraft_text::shared());
            }
            let fonts = ms(t);
            layers = r.document.walk().len();
            let t = Instant::now();
            let mut s = photocraft_engine::Session::new();
            s.open_document(r.document, Some(f.clone()));
            let session = ms(t);
            let t = Instant::now();
            let doc = &s.active().unwrap().doc;
            let _ = photocraft_compose::render(doc, doc.bounds());
            let composite = ms(t);
            let total = ms(t0);
            let row = [decode, bounds, fonts, session, composite, total];
            if best.is_none_or(|b| total < b[5]) {
                best = Some(row);
            }
            drop(s);
        }
        if check {
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let none = &photocraft_raster::Interrupt::NONE;
            let bytes = std::sync::Arc::new(std::fs::read(f).unwrap());
            let a = photocraft_io::import_for_open_with(&name, &bytes, &Default::default(), none).unwrap().0.document;
            let b = photocraft_io::import_shared_for_open_with(&name, &bytes, &Default::default(), none).unwrap().0.document;
            let same = a.walk().len() == b.walk().len()
                && a.walk().iter().zip(b.walk().iter()).all(|((_, _, x), (_, _, y))| match (x.surface(), y.surface()) {
                    (Some(p), Some(q)) => p.to_interleaved(a.bounds()) == q.to_interleaved(a.bounds()),
                    (None, None) => true,
                    _ => false,
                });
            println!("  check: shared bytes decode {} the copied bytes", if same { "the same as" } else { "DIFFERENTLY from" });
        }
        if let Some(b) = best {
            let short: String = f.chars().rev().take(44).collect::<Vec<_>>().into_iter().rev().collect();
            println!("{short:<44} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>9.0} {:>8.0}  {layers}", b[0], b[1], b[2], b[3], b[4], b[5]);
        }
    }
}
