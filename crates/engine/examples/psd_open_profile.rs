//! Times opening PSD/PSB files the way File › Open does, by phase: decode (the file read in
//! place, channels decompressed, layers, type and effects built), the layer bounds warm-up, the
//! text engine's font wait, adding the document to a session (colour policy), and the first full
//! composite the canvas needs. Each file's best of `--reps` runs.
//!
//! `--bytes` decodes from the file read into memory first (as the browser opens a picked file),
//! `--copied` from those bytes copied (the open before shared bytes); `--check` compares the
//! layers (pixels, masks, warnings) of the layer walk decoding each layer itself with the layers
//! decoded side by side, a band at a time between frames, and from copied bytes. `--stepped`
//! times the single-threaded browser build's open a step at a time. The `preview` column is the
//! merged image an open shows first.
//!
//! ```sh
//! cargo run --release -p photocraft-engine --example psd_open_profile -- [--reps 3] [--bytes|--copied] [--check] [--stepped] <file.psd>...
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
    let stepped = args.iter().any(|a| a == "--stepped");
    if files.is_empty() {
        eprintln!("usage: psd_open_profile [--reps N] <file.psd>...");
        std::process::exit(2);
    }
    // The text engine's installed-font scan, once, as at launch.
    let t = Instant::now();
    let _ = photocraft_text::shared();
    println!("text engine ready: {:.0} ms", ms(t));
    println!("{:<44} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>8}  layers", "file", "preview", "decode", "bounds", "fonts", "session", "composite", "total");
    for f in files {
        let mut best: Option<[f64; 6]> = None;
        let mut best_preview = f64::INFINITY;
        let mut layers = 0;
        for _ in 0..reps {
            // The merged image an open shows first (`jobs::open_preview`).
            let src = photocraft_engine::jobs::OpenSource::Bytes(std::sync::Arc::new(std::fs::read(f).unwrap()));
            let t = Instant::now();
            let _ = photocraft_engine::jobs::open_preview(&src);
            best_preview = best_preview.min(ms(t));
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
                let _ = photocraft_text::shared();
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
        if stepped {
            // The single-threaded browser build's open, a step at a time with a 4 ms budget: the
            // parse, the longest step and the steps' total, and the last step (the document).
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let bytes = std::sync::Arc::new(std::fs::read(f).unwrap());
            let t = Instant::now();
            let mut o = photocraft_io::PsdOpen::shared(&name, &bytes).expect("a PSD").unwrap();
            let parse = ms(t);
            let (mut longest, mut total, mut n) = (0.0f64, 0.0, 0);
            loop {
                let t = Instant::now();
                let until = t + std::time::Duration::from_millis(4);
                let done = o.step(&|| Instant::now() < until);
                let took = ms(t);
                (longest, total, n) = (longest.max(took), total + took, n + 1);
                if done {
                    break;
                }
            }
            let t = Instant::now();
            let _ = o.finish(&photocraft_raster::Interrupt::NONE).unwrap();
            println!("  stepped: parse {parse:.1} ms, {n} steps of {total:.0} ms (longest {longest:.1}), document {:.1} ms", ms(t));
        }
        if check {
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let none = &photocraft_raster::Interrupt::NONE;
            let bytes = std::sync::Arc::new(std::fs::read(f).unwrap());
            let open = || photocraft_io::PsdOpen::shared(&name, &bytes).expect("a PSD").unwrap();
            // The layer walk decoding each layer itself (as before pre-decoding), the layers
            // decoded side by side, and a band at a time (the single-threaded browser build).
            let walk = open().finish(none).unwrap().0;
            let mut o = open();
            o.decode_all(none).unwrap();
            let parallel = o.finish(none).unwrap().0;
            let mut o = open();
            let mut steps = 0;
            while !o.step(&|| false) {
                steps += 1;
            }
            let stepped = o.finish(none).unwrap().0;
            let copied = photocraft_io::import_for_open_with(&name, &bytes, &Default::default(), none).unwrap().0;
            let same = |a: &photocraft_io::ImportResult, b: &photocraft_io::ImportResult| {
                let (x, y) = (&a.document, &b.document);
                a.warnings == b.warnings
                    && x.walk().len() == y.walk().len()
                    && x.walk().iter().zip(y.walk().iter()).all(|((_, _, p), (_, _, q))| {
                        let px = |l: &photocraft_doc::Layer| l.surface().map(|s| s.to_interleaved(x.bounds()));
                        let mask = |l: &photocraft_doc::Layer| l.mask.as_ref().map(|m| m.surface.to_interleaved(x.bounds()));
                        p.name == q.name && p.visible == q.visible && px(p) == px(q) && mask(p) == mask(q)
                    })
            };
            for (what, r) in [("side by side", &parallel), (&*format!("in {} steps", steps + 1), &stepped), ("from copied bytes", &copied)] {
                println!("  check: decoded {what}: {} the walk's decode", if same(&walk, r) { "the same as" } else { "DIFFERENT from" });
            }
        }
        if let Some(b) = best {
            let short: String = f.chars().rev().take(44).collect::<Vec<_>>().into_iter().rev().collect();
            println!("{short:<44} {best_preview:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>9.0} {:>8.0}  {layers}", b[0], b[1], b[2], b[3], b[4], b[5]);
        }
    }
}
