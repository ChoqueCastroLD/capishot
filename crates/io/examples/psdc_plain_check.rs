//! Checks `psdc::compose_agrees` on real files: for every PSD given (files or folders), when the
//! predicate says photocraft-compose draws it as psd-compiler does, both composites of the
//! document (as Capishot would save it) must agree within one level per channel.
//! `cargo run --release -p photocraft-io --example psdc_plain_check -- <psd or dir>...`
use photocraft_io::psdc;

fn files(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if p.is_dir() {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            files(&e.path(), out);
        }
    } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("psd")) {
        out.push(p.to_path_buf());
    }
}

fn main() {
    let mut list = Vec::new();
    for a in std::env::args().skip(1) {
        files(std::path::Path::new(&a), &mut list);
    }
    let (mut plain, mut bad, mut total) = (0, 0, 0);
    for path in list {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if bytes.len() > 200 << 20 {
            continue;
        }
        let Ok(r) = photocraft_io::import(&path.to_string_lossy(), &bytes) else { continue };
        let doc = r.document;
        total += 1;
        if !psdc::compose_agrees(&doc) {
            continue;
        }
        plain += 1;
        let compiled = match psdc::compile_document(&doc, &Default::default()) {
            Ok(c) => c,
            Err(e) => {
                println!("{}: psd-compiler failed: {e}", path.display());
                continue;
            }
        };
        let flat = photocraft_compose::flatten(&doc);
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let mut worst = 0u8;
        let mut over = 0usize;
        for (p, c) in flat.px.iter().zip(compiled.rgba.as_chunks::<4>().0) {
            let a = q(p[3]);
            let d = (0..4).map(|i| if i < 3 && a == 0 && c[3] == 0 { 0 } else { q(p[i]).abs_diff(c[i]) }).max().unwrap_or(0);
            worst = worst.max(d);
            over += usize::from(d > 1);
        }
        if worst > 1 {
            bad += 1;
            println!("DIFF {}: max {worst}, {over} px over 1", path.display());
        }
    }
    println!("{total} files, {plain} plain, {bad} plain with a difference over one level");
}
