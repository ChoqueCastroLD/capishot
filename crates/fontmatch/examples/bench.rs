//! The font finder's benchmark: words drawn in the library's own fonts, damaged like scans, and how
//! often the right face comes back. Faces with the same look count as the same answer.
//!
//! bench [--by-file] <folder of fonts> [targets] [seed]; prints top-1, top-5 and top-20 hit rates and
//! times, a JSON line per mode. Fonts are read from the folder and aren't part of the repo. A face is
//! named after its parent folder, as in Google Fonts (one folder, one family); with `--by-file`, after
//! its file, for a library whose folders are categories.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use photocraft_fontmatch::glyph::{bitmap, size_for_cap, trim};
use photocraft_fontmatch::{Bitmap, FaceSource, Library, Zone, find};
use skrifa::{FontRef, MetadataProvider};

const WORDS: [&str; 10] = ["NO PUEDE SER", "DAME UN MOMENTO", "VAMOS A CASA", "ESO ES MENTIRA", "TE LO DIJE", "QUIERO SABER", "ESPERA UN POCO", "MUY BIEN HECHO", "Hola, como estas", "Ya no lo escucho"];
const MODES: [&str; 6] = ["clean", "blur", "noise", "tone", "white-on-black", "outline"];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut items: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    items.sort();
    for p in items {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf")) {
            out.push(p);
        }
    }
}

/// `text` in `data`'s font, as lines of ink on a white ground, spaces and all.
fn draw(data: &[u8], text: &str, cap: f32) -> Option<Bitmap> {
    let font = FontRef::new(data).ok()?;
    let size = size_for_cap(&font, cap)?;
    let map = font.charmap();
    let adv = font.glyph_metrics(skrifa::instance::Size::new(size), skrifa::instance::LocationRef::default());
    let (pad, line) = ((cap * 0.8) as usize, (cap * 1.5) as usize);
    let words: Vec<&str> = text.split(' ').collect();
    let mid = words.len().div_ceil(2);
    let lines = [words[..mid].join(" "), words[mid..].join(" ")];
    let widths: Vec<f32> = lines.iter().map(|l| l.chars().map(|c| map.map(c).and_then(|g| adv.advance_width(g)).unwrap_or(cap / 2.0)).sum()).collect();
    let w = widths.iter().copied().fold(0.0, f32::max) as usize + 2 * pad;
    let h = 2 * line + 2 * pad;
    let mut out = Bitmap::new(w, h);
    for (k, l) in lines.iter().enumerate() {
        let mut pen = pad as f32;
        let base = pad + line * k + cap as usize;
        for c in l.chars() {
            let Some(g) = map.map(c) else { continue };
            if let Some(b) = bitmap_placed(&font, g, size) {
                let (bx, by, b) = b;
                for y in 0..b.height {
                    for x in 0..b.width {
                        let (xx, yy) = (pen as i32 + bx + x as i32, base as i32 + by + y as i32);
                        if b.get(x, y) && xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                            out.data[yy as usize * w + xx as usize] = true;
                        }
                    }
                }
            }
            pen += adv.advance_width(g).unwrap_or(cap / 2.0);
        }
    }
    Some(out)
}

/// A glyph with its offset from the pen and the baseline.
fn bitmap_placed(font: &FontRef<'_>, g: skrifa::GlyphId, size: f32) -> Option<(i32, i32, Bitmap)> {
    let b = font.glyph_metrics(skrifa::instance::Size::new(size), skrifa::instance::LocationRef::default()).bounds(g)?;
    let bm = bitmap(font, g, size)?;
    Some((b.x_min.round() as i32, -b.y_max.round() as i32, trim(&bm)))
}

/// The drawing damaged as a scan would be, as RGB.
fn damage(ink: &Bitmap, mode: &str, rng: &mut Rng) -> Vec<u8> {
    let (w, h) = (ink.width, ink.height);
    let mut g: Vec<f32> = ink.data.iter().map(|v| if *v { 0.0 } else { 255.0 }).collect();
    match mode {
        "blur" => {
            for _ in 0..2 {
                let s = g.clone();
                for y in 1..h - 1 {
                    for x in 1..w - 1 {
                        let i = y * w + x;
                        g[i] = (s[i] * 4.0 + s[i - 1] + s[i + 1] + s[i - w] + s[i + w]) / 8.0;
                    }
                }
            }
        }
        "noise" => {
            for v in &mut g {
                *v = (*v + (rng.pick(61) as f32 - 30.0)).clamp(0.0, 255.0);
            }
        }
        "tone" => {
            for y in 0..h {
                for x in 0..w {
                    if !ink.data[y * w + x] && (x + y) % 5 == 0 && (x + h - y) % 5 == 0 {
                        g[y * w + x] = 140.0;
                    }
                }
            }
        }
        "white-on-black" => {
            for v in &mut g {
                *v = 255.0 - *v;
            }
        }
        "outline" => {
            // The letters white, in a black ring on white.
            let r = 3usize;
            let mut ring = vec![false; w * h];
            for y in 0..h {
                for x in 0..w {
                    if ink.data[y * w + x] {
                        for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                            for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                                ring[yy * w + xx] = true;
                            }
                        }
                    }
                }
            }
            for i in 0..w * h {
                g[i] = if ink.data[i] || !ring[i] { 255.0 } else { 0.0 };
            }
        }
        _ => {}
    }
    g.iter().flat_map(|v| [*v as u8, *v as u8, *v as u8]).collect()
}

fn main() {
    let mut a: Vec<String> = std::env::args().collect();
    let by_file = a.iter().any(|v| v == "--by-file");
    a.retain(|v| v != "--by-file");
    let targets: usize = a.get(2).map_or(100, |v| v.parse().unwrap());
    let mut rng = Rng(a.get(3).map_or(88172645463325252, |v| v.parse().unwrap()));
    let mut paths = Vec::new();
    walk(std::path::Path::new(&a[1]), &mut paths);
    let files: Vec<(String, Vec<u8>)> = paths
        .iter()
        .filter_map(|p| Some(((if by_file { p.file_stem()? } else { p.parent()?.file_name()? }).to_str()?.to_string(), std::fs::read(p).ok()?)))
        .collect();
    let sources: Vec<FaceSource<'_>> = files.iter().map(|(f, d)| FaceSource { family: f, style: "", data: d, index: 0 }).collect();
    let t = std::time::Instant::now();
    let lib = Library::build(&sources, &mut |_| {});
    println!("{{\"files\":{},\"faces\":{},\"build_s\":{:.1}}}", files.len(), lib.faces.len(), t.elapsed().as_secs_f32());
    for mode in MODES {
        let (mut n, mut hit1, mut hit5, mut hit20, mut times) = (0, 0, 0, 0, Vec::new());
        for _ in 0..targets {
            let f = rng.pick(lib.faces.len());
            let face = &lib.faces[f];
            let Some((_, data)) = files.iter().find(|(name, _)| *name == face.family) else { continue };
            let word = WORDS[rng.pick(WORDS.len())];
            let Some(ink) = draw(data, word, [18.0, 24.0, 32.0, 44.0][rng.pick(4)]) else { continue };
            let rgb = damage(&ink, mode, &mut rng);
            let zone = Zone { rgb: &rgb, width: ink.width, height: ink.height };
            let t = std::time::Instant::now();
            let found = find(&lib, &zone, 20);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            // The answer's family, or a copy of its design: those are one choice.
            let at = found.iter().position(|c| c.family == face.family || lib.faces.iter().filter(|x| x.family == c.family).any(|x| x.near_copy(face) || x.base_name() == face.base_name()));
            n += 1;
            hit1 += usize::from(at.is_some_and(|p| p < 1));
            hit5 += usize::from(at.is_some_and(|p| p < 5));
            hit20 += at.is_some() as usize;
        }
        times.sort_by(f64::total_cmp);
        let pc = |h: usize| 100.0 * h as f32 / n.max(1) as f32;
        println!(
            "{{\"mode\":\"{mode}\",\"n\":{n},\"top1\":{:.0},\"top5\":{:.0},\"top20\":{:.0},\"p50_ms\":{:.0},\"p95_ms\":{:.0}}}",
            pc(hit1),
            pc(hit5),
            pc(hit20),
            times.get(times.len() / 2).copied().unwrap_or(0.0),
            times.get(times.len() * 95 / 100).copied().unwrap_or(0.0)
        );
    }
}
