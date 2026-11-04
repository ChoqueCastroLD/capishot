use super::*;
use serde_json::json;

const LIMITS: Limits = Limits { total: 1 << 20, files: 8, file: 64 << 10 };

fn allowed(name: &str) -> bool {
    matches!(name, "a.txt" | "icon.png")
}

/// A stored (uncompressed) zip with exactly these names, contents and external attributes, so
/// a test can write what `ZipWriter` refuses to (unsafe names, symbolic links).
pub(crate) fn raw_zip(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for (name, data, external) in entries {
        let offset = out.len() as u32;
        let crc = photocraft_format::zip::crc32(data);
        let n = name.len() as u16;
        let len = data.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cd.extend_from_slice(&[20, 3, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&len.to_le_bytes());
        cd.extend_from_slice(&len.to_le_bytes());
        cd.extend_from_slice(&n.to_le_bytes());
        cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&external.to_le_bytes());
        cd.extend_from_slice(&offset.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A zip of deflated entries; `lie` declares that uncompressed size for the last one instead.
fn deflated_zip(entries: &[(&str, &[u8])], lie: Option<u32>) -> Vec<u8> {
    use std::io::Write;
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for (i, (name, data)) in entries.iter().enumerate() {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(data).unwrap();
        let packed = enc.finish().unwrap();
        let size = if i + 1 == entries.len() { lie.unwrap_or(data.len() as u32) } else { data.len() as u32 };
        let crc = photocraft_format::zip::crc32(data);
        let offset = out.len() as u32;
        let n = name.len() as u16;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&packed);
        cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cd.extend_from_slice(&[20, 3, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        cd.extend_from_slice(&size.to_le_bytes());
        cd.extend_from_slice(&n.to_le_bytes());
        cd.extend_from_slice(&[0; 12]);
        cd.extend_from_slice(&offset.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A bomb that understates its size is caught when it's read: it never grows past what it
/// declares.
#[test]
fn an_entry_that_lies_about_its_size_is_refused_when_read() {
    let bytes = deflated_zip(&[("manifest.json", &manifest()), ("a.txt", &vec![b'x'; 1 << 20])], Some(10));
    let p = open(&bytes).unwrap();
    assert!(p.read("a.txt").is_err());
}

fn manifest() -> Vec<u8> {
    serde_json::to_vec(&json!({"format": "test", "version": 1})).unwrap()
}

fn open(bytes: &[u8]) -> Result<Package<'_>> {
    Package::open(bytes, "test package", &[("test", 1)], allowed, LIMITS)
}

pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
    let data: Vec<u8> = (0..w * h).flat_map(|_| [200, 10, 10, 255]).collect();
    let img = photocraft_codecs::Image::from_raw(w, h, photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, data).unwrap();
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()
}

#[test]
fn a_package_round_trips_with_its_metadata() {
    let meta = Meta { title: "Tinta".into(), author: "Equipo".into(), tags: vec!["dark".into(), "Dark".into(), " manga ".into()], ..Meta::default() };
    let mut w = Writer::new("test", 1, &meta, Map::new());
    w.add("a.txt", b"hola").unwrap();
    let bytes = w.finish().unwrap();
    let p = open(&bytes).unwrap();
    assert_eq!(p.meta.title, "Tinta");
    assert_eq!(p.meta.tags, vec!["dark".to_string(), "manga".into()], "tags trimmed and de-duplicated");
    assert_eq!(p.read_text("a.txt").unwrap().as_deref(), Some("hola"));
    assert!(p.ignored.is_empty());
}

#[test]
fn unsafe_paths_and_symbolic_links_are_rejected() {
    for name in ["../a.txt", "/a.txt", "x/../../a.txt", "C:/a.txt", "a\\..\\b", "./a.txt", "a//b", "a\u{1}"] {
        let bytes = raw_zip(&[("manifest.json", &manifest(), 0), (name, b"x", 0)]);
        let err = open(&bytes).err().unwrap_or_else(|| panic!("{name:?} accepted"));
        assert!(err.to_string().contains("unsafe path"), "{name:?}: {err}");
    }
    let link = raw_zip(&[("manifest.json", &manifest(), 0), ("a.txt", b"/etc/passwd", 0o120_777 << 16)]);
    assert!(open(&link).err().unwrap().to_string().contains("symbolic link"));
}

#[test]
fn unknown_files_are_listed_and_never_read() {
    let bytes = raw_zip(&[("manifest.json", &manifest(), 0), ("a.txt", b"ok", 0), ("run.exe", b"MZ", 0)]);
    let p = open(&bytes).unwrap();
    assert_eq!(p.ignored, vec!["run.exe".to_string()]);
    assert!(p.read("run.exe").is_err());
}

#[test]
fn size_and_count_limits_hold_before_anything_is_decompressed() {
    // A zip bomb: 32 MB of zeros deflates to a few KB but declares its size.
    let bomb = deflated_zip(&[("manifest.json", &manifest()), ("a.txt", &vec![0u8; 32 << 20])], None);
    assert!(bomb.len() < LIMITS.total, "small on disk ({} bytes)", bomb.len());
    assert!(open(&bomb).err().unwrap().to_string().contains("larger than"));
    // Too many files.
    let names: Vec<String> = (0..20).map(|i| format!("f{i}")).collect();
    let mut entries: Vec<(&str, &[u8], u32)> = names.iter().map(|n| (n.as_str(), &b"x"[..], 0)).collect();
    let m = manifest();
    entries.push(("manifest.json", &m, 0));
    assert!(open(&raw_zip(&entries)).err().unwrap().to_string().contains("more than"));
}

#[test]
fn manifests_are_checked() {
    let with = |m: Value| raw_zip(&[("manifest.json", &serde_json::to_vec(&m).unwrap(), 0)]);
    assert!(open(&with(json!({"format": "other", "version": 1}))).is_err());
    assert!(open(&with(json!({"format": "test", "version": 9}))).err().unwrap().to_string().contains("newer Capishot"));
    assert!(open(&with(json!({"format": "test", "version": 1, "meta": {"minCapishotVersion": "999.0.0"}}))).err().unwrap().to_string().contains("999"));
    assert!(open(&raw_zip(&[("manifest.json", b"[1]", 0)])).is_err());
    assert!(open(&raw_zip(&[("a.txt", b"x", 0)])).err().unwrap().to_string().contains("manifest"));
    // Links that aren't web pages and stray icon names are dropped, with a warning.
    let p = with(json!({"format": "test", "version": 1, "meta": {"homepage": "javascript:alert(1)", "icon": "../x.png", "title": "t\u{7}x"}}));
    let p = open(&p).unwrap();
    assert!(p.meta.homepage.is_empty() && p.meta.icon.is_empty());
    assert_eq!(p.meta.title, "tx");
    assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
}

#[test]
fn images_must_be_pngs_within_their_size() {
    assert_eq!(check_png(&png(16, 8), 256), Ok((16, 8)));
    assert!(check_png(&png(300, 4), 256).is_err());
    assert!(check_png(b"\x89PNG\r\n\x1a\nnot really", 256).is_err());
    assert!(check_png(b"GIF89a", 256).is_err());
    let mut forged = png(8, 8);
    forged.truncate(forged.len() - 20);
    assert!(check_png(&forged, 256).is_err(), "a PNG that doesn't decode");
}

/// Mutated and truncated packages are errors or packages, never panics.
#[test]
fn damaged_packages_never_panic() {
    let mut w = Writer::new("test", 1, &Meta { title: "x".into(), ..Meta::default() }, Map::new());
    w.add("a.txt", &[7u8; 3000]).unwrap();
    w.add("icon.png", &png(4, 4)).unwrap();
    let good = w.finish().unwrap();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..3000 {
        let mut b = good.clone();
        match next() % 3 {
            0 => b.truncate((next() as usize) % good.len()),
            _ => {
                for _ in 0..1 + next() % 8 {
                    let i = (next() as usize) % b.len();
                    b[i] = next() as u8;
                }
            }
        }
        if let Ok(p) = open(&b) {
            let _ = p.read("a.txt");
            let _ = p.read_png("icon.png", 256);
        }
    }
}
