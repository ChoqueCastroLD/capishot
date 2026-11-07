//! Writes a pack file (`photocraft_fontmatch::pack`) from a checkout of google/fonts: of each family
//! the faces most lettering is close to, of each variable font its default.
//!
//! pack <checkout> <base url, ending in /> <out.bin.gz>

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use photocraft_fontmatch::{Face, FaceSource, pack};
use skrifa::raw::FileRef;
use skrifa::{MetadataProvider, string::StringId};

/// Past this it's a font for another script: Latin in it is rarely what's on a page.
const MAX_FILE: u64 = 16 << 20;
/// Static styles worth looking through. The others are between these.
const STYLES: [&str; 6] = ["Regular", "Bold", "Italic", "Bold Italic", "Black", "Light"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "ttf" || x == "otf") && e.metadata().is_ok_and(|m| m.len() <= MAX_FILE) {
            out.push(p);
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&a[1]);
    let mut files = Vec::new();
    for licence in ["ofl", "apache", "ufl"] {
        walk(&root.join(licence), &mut files);
    }
    files.sort();
    let mut faces: Vec<(Face, String, u32)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let Ok(file) = FileRef::new(&data) else { continue };
        let variable = path.to_string_lossy().contains('[');
        for (index, font) in file.fonts().enumerate() {
            let Ok(font) = font else { continue };
            let name = |id| font.localized_strings(id).english_or_first().map(|s| s.to_string()).filter(|s| !s.trim().is_empty());
            let Some(family) = name(StringId::TYPOGRAPHIC_FAMILY_NAME).or_else(|| name(StringId::FAMILY_NAME)) else { continue };
            let style = name(StringId::TYPOGRAPHIC_SUBFAMILY_NAME).or_else(|| name(StringId::SUBFAMILY_NAME)).unwrap_or_else(|| "Regular".to_string());
            if !variable && !STYLES.contains(&style.as_str()) || !seen.insert((family.clone(), style.clone())) {
                continue;
            }
            let source = FaceSource { family: &family, style: &style, data: &data, index: index as u32 };
            if let Some(face) = Face::read(&source) {
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                faces.push((face, rel, index as u32));
            }
        }
    }
    let file = pack::write(&a[2], &faces).unwrap();
    std::fs::write(&a[3], &file).unwrap();
    println!("{} files, {} faces, {} MB", files.len(), faces.len(), file.len() >> 20);
}
