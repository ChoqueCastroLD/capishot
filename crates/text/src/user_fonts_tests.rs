use super::*;
use crate::fonts::{INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO_REGULAR};

fn prepared(bytes: &[u8], name: &str) -> Prepared {
    prepare(name, bytes.to_vec()).unwrap()
}

/// Wraps OpenType data in WOFF 1.0, every table zlib-compressed (when that's smaller).
fn to_woff(sfnt: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let u16_at = |at: usize| u16::from_be_bytes([sfnt[at], sfnt[at + 1]]);
    let u32_at = |at: usize| u32::from_be_bytes([sfnt[at], sfnt[at + 1], sfnt[at + 2], sfnt[at + 3]]);
    let n = usize::from(u16_at(4));
    let mut dir = Vec::new();
    let mut data = Vec::new();
    let start = 44 + 20 * n;
    for i in 0..n {
        let r = 12 + 16 * i;
        let (tag, sum, off, len) = (u32_at(r), u32_at(r + 4), u32_at(r + 8) as usize, u32_at(r + 12) as usize);
        let table = &sfnt[off..off + len];
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(table).unwrap();
        let z = z.finish().unwrap();
        let stored = if z.len() < table.len() { z } else { table.to_vec() };
        for v in [tag, (start + data.len()) as u32, stored.len() as u32, len as u32, sum] {
            dir.extend_from_slice(&v.to_be_bytes());
        }
        data.extend_from_slice(&stored);
        data.resize(data.len().div_ceil(4) * 4, 0);
    }
    let mut out = b"wOFF".to_vec();
    out.extend_from_slice(&u32_at(0).to_be_bytes());
    out.extend_from_slice(&((start + data.len()) as u32).to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(sfnt.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 0]);
    out.extend_from_slice(&[0; 20]);
    assert_eq!(out.len(), 44);
    out.extend(dir);
    out.extend(data);
    out
}

#[test]
fn a_font_file_is_checked_named_and_hashed() {
    let p = prepared(&INTER_REGULAR, "Fonts/Latin/Inter-Regular.ttf");
    assert_eq!(p.entry.name, "Inter-Regular.ttf", "the folders are dropped");
    assert_eq!(p.entry.id, crate::sha256::sha256_hex(&INTER_REGULAR));
    assert_eq!(p.entry.bytes, INTER_REGULAR.len() as u64);
    assert_eq!(p.entry.faces.len(), 1);
    let f = &p.entry.faces[0];
    assert_eq!((f.family.as_str(), f.style.as_str(), f.weight, f.italic), ("Inter", "Regular", 400, false));
    assert_eq!(f.postscript.as_deref(), Some("Inter-Regular"));
    assert!(p.entry.provides("inter") && p.entry.provides("Inter-Regular") && !p.entry.provides("Inter-Bold"));
    let s = prepared(&INTER_SEMIBOLD, "Inter-SemiBold.ttf");
    assert_eq!(s.entry.faces[0].weight, 600);
}

#[test]
fn files_that_are_not_usable_fonts_say_why() {
    assert!(prepare("notes.ttf", b"hello, not a font".to_vec()).unwrap_err().contains("not a font"));
    assert!(prepare("a.woff2", b"wOF2\0\0\0\0rest".to_vec()).unwrap_err().contains("WOFF2"));
    assert!(prepare("a.pfb", INTER_REGULAR.to_vec()).unwrap_err().contains("not supported"));
    assert!(prepare("a.woff", b"wOFFshort".to_vec()).unwrap_err().contains("WOFF"));
    assert!(prepare("empty.otf", Vec::new()).is_err());
}

#[test]
fn woff_files_unpack_to_the_font_they_wrap() {
    let woff = to_woff(&INTER_REGULAR);
    assert!(woff.len() < INTER_REGULAR.len(), "the tables were compressed");
    let p = prepared(&woff, "Inter.woff");
    assert_eq!(p.entry.faces, prepared(&INTER_REGULAR, "x.ttf").entry.faces);
    let a = FileRef::new(&INTER_REGULAR).unwrap();
    let b = FileRef::new(&p.data).unwrap();
    let (FileRef::Font(a), FileRef::Font(b)) = (a, b) else { panic!("single fonts") };
    for rec in a.table_directory.table_records() {
        assert_eq!(a.table_data(rec.tag()).unwrap().as_bytes(), b.table_data(rec.tag()).unwrap().as_bytes(), "{}", rec.tag());
    }
}

#[test]
fn damaged_woff_files_are_refused_without_panicking() {
    let woff = to_woff(&JETBRAINS_MONO_REGULAR);
    for cut in [10, 44, 60, woff.len() / 2] {
        assert!(woff_to_sfnt(&woff[..cut]).is_err(), "cut at {cut}");
    }
    let mut lying = woff.clone();
    // The first table claims a huge original length.
    lying[44 + 12..44 + 16].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(woff_to_sfnt(&lying).is_err());
    let mut many = woff;
    many[12..14].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(woff_to_sfnt(&many).is_err());
}

#[test]
fn the_same_data_is_loaded_once() {
    let mut s = Store::new();
    let p = prepared(&INTER_REGULAR, "Inter-Regular.ttf");
    assert_eq!(s.add(&p), Added::New { id: p.entry.id.clone(), families: vec!["Inter".into()] });
    let again = prepared(&INTER_REGULAR, "Copy of Inter.ttf");
    assert_eq!(s.add(&again), Added::Duplicate { name: "Inter-Regular.ttf".into() });
    assert_eq!(s.entries().len(), 1);
    assert!(matches!(s.add(&prepared(&JETBRAINS_MONO_REGULAR, "mono.ttf")), Added::New { .. }));
    assert_eq!(s.families(), ["Inter", "JetBrains Mono"]);
    assert!(s.is_user_family("jetbrains mono"));
    assert_eq!(s.total_bytes(), (INTER_REGULAR.len() + JETBRAINS_MONO_REGULAR.len()) as u64);
}

#[test]
fn without_a_store_fonts_are_for_the_session() {
    let mut s = Store::new();
    let p = prepared(&INTER_REGULAR, "a.ttf");
    s.add(&p);
    assert!(s.take_ops().is_empty(), "nothing to write on the desktop");
    assert!(s.is_unsaved(&p.entry.id));
}

#[test]
fn added_fonts_are_written_once_the_store_is_open() {
    let mut s = Store::new();
    s.set_persistence(Persistence::Pending);
    let p = prepared(&INTER_REGULAR, "a.ttf");
    s.add(&p);
    assert!(s.want_persist, "the first font asks for persistent storage");
    assert!(s.take_ops().is_empty(), "the store isn't open yet");
    s.set_persistence(Persistence::Stored);
    let ops = s.take_ops();
    assert!(matches!(ops.as_slice(), [Op::Put { entry, data }] if entry.id == p.entry.id && data.len() == INTER_REGULAR.len()));
    assert!(!s.is_unsaved(&p.entry.id));
    // Removing it deletes it from the store.
    assert!(s.remove(&p.entry.id).is_some());
    assert!(matches!(s.take_ops().as_slice(), [Op::Delete(id)] if *id == p.entry.id));
    assert!(s.remove(&p.entry.id).is_none());
    // A removal before the write was made just drops the write.
    s.add(&p);
    s.remove(&p.entry.id);
    assert!(s.take_ops().is_empty());
    // Clear All.
    s.add(&p);
    s.add(&prepared(&JETBRAINS_MONO_REGULAR, "b.ttf"));
    assert_eq!(s.clear().len(), 2);
    assert!(matches!(s.take_ops().as_slice(), [Op::Clear]));
    assert!(s.entries().is_empty() && s.families().is_empty());
}

#[test]
fn a_store_that_fails_to_open_keeps_fonts_for_the_session() {
    let mut s = Store::new();
    s.set_persistence(Persistence::Pending);
    let p = prepared(&INTER_REGULAR, "a.ttf");
    s.add(&p);
    s.set_persistence(Persistence::Unavailable("IndexedDB unavailable".into()));
    assert!(s.take_ops().is_empty());
    assert!(s.is_unsaved(&p.entry.id) && s.is_loaded(&p.entry.id));
}

#[test]
fn a_quota_error_keeps_the_font_for_the_session() {
    let mut s = Store::new();
    s.set_persistence(Persistence::Stored);
    let p = prepared(&INTER_REGULAR, "a.ttf");
    s.add(&p);
    let _ = s.take_ops();
    let g = s.generation();
    assert_eq!(s.write_failed(&p.entry.id, "QuotaExceededError: The quota has been exceeded."), WriteError::Quota);
    assert!(s.is_unsaved(&p.entry.id) && s.is_loaded(&p.entry.id), "still usable");
    assert_ne!(s.generation(), g, "the dialog shows it unsaved");
    assert_eq!(s.write_failed(&p.entry.id, "NS_ERROR_DOM_QUOTA_REACHED"), WriteError::Quota);
    assert_eq!(s.write_failed(&p.entry.id, "UnknownError: disk I/O"), WriteError::Other("UnknownError: disk I/O".into()));
    // Removing an unsaved font has nothing to delete.
    s.remove(&p.entry.id);
    assert!(s.take_ops().is_empty());
}

#[test]
fn the_index_restores_without_the_bytes_and_loads_lazily() {
    let inter = prepared(&INTER_REGULAR, "Inter-Regular.ttf").entry;
    let mono = prepared(&JETBRAINS_MONO_REGULAR, "mono.ttf").entry;
    let mut bad_id = inter.clone();
    bad_id.id = "../../etc".into();
    let mut no_faces = mono.clone();
    no_faces.faces.clear();
    let records = vec![
        serde_json::to_string(&inter).unwrap(),
        serde_json::to_string(&mono).unwrap(),
        serde_json::to_string(&inter).unwrap(),
        "{not json".into(),
        serde_json::to_string(&bad_id).unwrap(),
        serde_json::to_string(&no_faces).unwrap(),
    ];
    let mut s = Store::new();
    s.set_persistence(Persistence::Stored);
    let warnings = s.restore(records);
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert_eq!(s.entries().len(), 2);
    assert_eq!(s.families(), ["Inter", "JetBrains Mono"], "listed before any byte is read");
    assert!(!s.is_loaded(&inter.id) && !s.is_loaded(&mono.id));
    assert!(s.take_ops().is_empty(), "restoring writes nothing");

    // A layout asks for a family (or a PSD for a PostScript name): that file is read first.
    assert!(s.request("JetBrainsMono-Regular"));
    assert!(!s.request("Nowhere Sans"));
    assert_eq!(s.next_loads(true), std::slice::from_ref(&mono.id));
    assert!(s.next_loads(true).is_empty(), "one idle read at a time");
    s.request("JetBrains Mono");
    assert!(s.next_loads(false).is_empty(), "already being read");
    s.loaded(&mono.id);
    assert!(s.is_loaded(&mono.id));
    // Idle time reads the rest; not while busy.
    assert!(s.next_loads(false).is_empty());
    assert_eq!(s.next_loads(true), std::slice::from_ref(&inter.id));
    s.loaded(&inter.id);
    assert!(s.next_loads(true).is_empty() && s.unloaded() == 0 && !s.loading());
    // Picking a restored file again uses the stored copy: no duplicate, no second write.
    assert!(matches!(s.add(&prepared(&INTER_REGULAR, "again.ttf")), Added::Duplicate { .. }));
    assert!(s.take_ops().is_empty());
}

#[test]
fn a_picked_file_that_is_already_stored_needs_no_read() {
    let inter = prepared(&INTER_REGULAR, "Inter-Regular.ttf");
    let mut s = Store::new();
    s.set_persistence(Persistence::Stored);
    s.restore([serde_json::to_string(&inter.entry).unwrap()]);
    s.request("Inter");
    assert!(matches!(s.add(&inter), Added::New { .. }), "known but not loaded: these bytes register it");
    assert!(s.is_loaded(&inter.entry.id));
    assert!(s.next_loads(true).is_empty(), "the queued read was dropped");
    assert!(s.take_ops().is_empty(), "already stored");
}

#[test]
fn a_stored_file_that_fails_to_read_is_dropped() {
    let inter = prepared(&INTER_REGULAR, "Inter-Regular.ttf").entry;
    let mut s = Store::new();
    s.set_persistence(Persistence::Stored);
    s.restore([serde_json::to_string(&inter).unwrap()]);
    let ids = s.next_loads(true);
    assert_eq!(s.load_failed(&ids[0]).map(|e| e.name), Some("Inter-Regular.ttf".into()));
    assert!(s.entries().is_empty());
    assert!(matches!(s.take_ops().as_slice(), [Op::Delete(_)]));
}

#[test]
fn idle_reads_stop_at_the_budget() {
    let mut big = prepared(&INTER_REGULAR, "big.ttf").entry;
    big.bytes = MAX_FONT_BYTES as u64;
    let mut more = prepared(&JETBRAINS_MONO_REGULAR, "more.ttf").entry;
    more.bytes = IDLE_BUDGET_BYTES - big.bytes + 1;
    let mut s = Store::new();
    s.restore([serde_json::to_string(&big).unwrap(), serde_json::to_string(&more).unwrap()]);
    assert!(s.idle_work());
    assert_eq!(s.next_loads(true), std::slice::from_ref(&big.id));
    s.loaded(&big.id);
    assert!(!s.idle_work(), "nothing more to do in idle time");
    assert!(s.next_loads(true).is_empty(), "over budget: read when needed");
    s.request("JetBrains Mono");
    assert_eq!(s.next_loads(false), std::slice::from_ref(&more.id), "a request is always honoured");
}

#[test]
fn font_file_names_are_recognised_by_extension() {
    for n in ["a.ttf", "B.OTF", "dir/c.ttc", "d.otc", "e.woff", "f.woff2", "g.pfb"] {
        assert!(is_font_file(n), "{n}");
    }
    for n in ["readme.txt", "ttf", "a.ttf.zip", "LICENSE", ".DS_Store", "a.psd"] {
        assert!(!is_font_file(n), "{n}");
    }
    assert_eq!(file_name("A/B\\c.ttf"), "c.ttf");
}

#[test]
fn quota_errors_are_recognised() {
    assert!(is_quota_error("QuotaExceededError: ..."));
    assert!(is_quota_error("NS_ERROR_DOM_QUOTA_REACHED"));
    assert!(!is_quota_error("AbortError: transaction aborted"));
}

/// Inter renamed to a five-letter family (UTF-16 names replaced in place).
pub(crate) fn renamed_inter(name: &str) -> Vec<u8> {
    assert_eq!(name.chars().count(), 5);
    let enc = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    let (from, to) = (enc("Inter"), enc(name));
    let mut b = INTER_REGULAR.to_vec();
    let mut i = 0;
    while i + from.len() <= b.len() {
        if b[i..i + from.len()] == from[..] {
            b[i..i + from.len()].copy_from_slice(&to);
            i += from.len();
        } else {
            i += 1;
        }
    }
    b
}

#[test]
fn user_fonts_register_and_unregister_from_memory() {
    let mut db = crate::FontDb::new();
    let before = db.font_data().len();
    let p = prepared(&renamed_inter("Qwyzt"), "Qwyzt.ttf");
    assert_eq!(p.entry.faces[0].family, "Qwyzt");
    assert_eq!(p.entry.faces[0].postscript.as_deref(), Some("Qwyzt-Regular"));
    assert!(!db.has_family("Qwyzt"));
    assert_eq!(db.register_user_font(&p.entry.id, p.data.clone()), ["Qwyzt"]);
    assert!(db.has_family("Qwyzt") && db.has_user_font(&p.entry.id));
    assert_eq!(db.register_user_font(&p.entry.id, p.data.clone()), ["Qwyzt"], "twice is once");
    assert_eq!(db.faces("Qwyzt").len(), 1);
    // psd-compiler gets the same bytes.
    let data = db.font_data();
    assert_eq!(data.len(), before + 1);
    assert!(matches!(data.last(), Some(crate::fonts::FontData::Shared(a)) if std::sync::Arc::ptr_eq(a, &p.data)));
    // PSD runs name the face by PostScript name.
    assert!(db.resolve_postscript("Qwyzt-Regular").exact);
    // A layout picks the exact face (registering an alias), then the font goes away entirely.
    let mut engine = crate::TextEngine::new();
    engine.fonts.register_user_font(&p.entry.id, p.data.clone());
    let t = photocraft_doc::TextLayer { text: "Hi".into(), font_family: "Qwyzt".into(), size_pt: 12.0, ..Default::default() };
    assert_eq!(engine.layout(&t, 72.0).glyphs.len(), 2);
    assert_eq!(engine.fonts.unregister_user_font(&p.entry.id), ["Qwyzt"]);
    assert!(!engine.fonts.has_family("Qwyzt"));
    assert_eq!(db.unregister_user_font(&p.entry.id), ["Qwyzt"]);
    assert!(!db.has_family("Qwyzt") && !db.families().iter().any(|f| f == "Qwyzt"));
    assert_eq!(db.font_data().len(), before, "its bytes are no longer handed on");
    assert!(db.unregister_user_font(&p.entry.id).is_empty());
    // Data that isn't a font registers nothing.
    assert!(db.register_user_font("x", std::sync::Arc::new(b"nope".to_vec())).is_empty());
    assert!(!db.has_user_font("x"));
}

/// One test for the process-wide store: parallel tests would see each other's fonts.
#[test]
fn layouts_ask_the_process_wide_store_for_missing_fonts() {
    let fam = prepared(&renamed_inter("Vbnmq"), "Vbnmq.ttf");
    let ps = prepared(&renamed_inter("Plokm"), "Plokm.ttf");
    let records = [serde_json::to_string(&fam.entry).unwrap(), serde_json::to_string(&ps.entry).unwrap()];
    let g = generation();
    assert!(store().restore(records).is_empty());
    assert_ne!(generation(), g);
    assert!(families().iter().any(|f| f == "Vbnmq") && families().iter().any(|f| f == "Plokm"));
    // A layout in a missing family asks for it (served::request forwards here).
    let t = photocraft_doc::TextLayer { text: "Hi".into(), font_family: "Vbnmq".into(), size_pt: 12.0, ..Default::default() };
    let _ = crate::TextEngine::new().layout(&t, 72.0);
    // A PSD run with a guessed family and the face's PostScript name asks by PostScript name.
    let mut psd = photocraft_doc::TextLayer { text: "Hi".into(), font_family: "Guessed Family".into(), size_pt: 12.0, ..Default::default() };
    psd.runs = vec![photocraft_doc::text::TextRun {
        len: 2,
        style: photocraft_doc::text::CharStyle {
            font_family: "Guessed Family".into(),
            postscript_name: Some("Plokm-Regular".into()),
            size_pt: 12.0,
            ..Default::default()
        },
    }];
    let _ = crate::TextEngine::new().layout(&psd, 72.0);
    let asked = store().next_loads(false);
    assert!(asked.contains(&fam.entry.id), "{asked:?}");
    assert!(asked.contains(&ps.entry.id), "{asked:?}");
    for id in [&fam.entry.id, &ps.entry.id] {
        store().remove(id);
    }
}
