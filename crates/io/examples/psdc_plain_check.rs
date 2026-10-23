//! Checks `psdc::compose_agrees` on real files: for every PSD given (files or folders), when the
//! predicate says photocraft-compose draws it as psd-compiler does, the merged image Capishot
//! saves with the shortcut must be psd-compiler's within one level per channel.
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
        // What a save writes: the merged image with the shortcut, and with psd-compiler.
        let fast = photocraft_io::export(&doc, "x.psd", &Default::default());
        let slow = photocraft_io::export(&doc, "x.psd", &photocraft_io::ExportOptions { psdc_plain_compose: false, ..Default::default() });
        let (Ok(fast), Ok(slow)) = (fast, slow) else {
            println!("{}: save failed", path.display());
            bad += 1;
            continue;
        };
        let (a, b) = (photocraft_psd::PsdFile::from_bytes(&fast.bytes).unwrap(), photocraft_psd::PsdFile::from_bytes(&slow.bytes).unwrap());
        let (pa, pb) = (a.image_data.decode(&a.header).unwrap(), b.image_data.decode(&b.header).unwrap());
        let worst = if a.header != b.header || pa.len() != pb.len() { u8::MAX } else { pa.iter().zip(&pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0) };
        if worst > 1 {
            bad += 1;
            println!("DIFF {}: max {worst}", path.display());
        }
    }
    println!("{total} files, {plain} plain, {bad} plain with a difference over one level");
}
