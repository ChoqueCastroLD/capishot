//! A scanlation team's Photoshop action, as Photoshop records it, loaded with Load Actions and
//! run over a folder with File › Automate › Batch (Override Action "Save As", a destination folder
//! and file naming): every step maps to a command, each page is written, and the result says
//! which steps Batch left out.

use super::*;
use photocraft_psd::atn::{AtnAction, AtnEvent, AtnFile, AtnItem};
use photocraft_psd::descriptor::{Descriptor, ReferenceItem, UnicodeString, Value as D};

fn item(event: &str, label: &str, d: Option<Descriptor>) -> AtnItem {
    let event = match event.as_bytes() {
        [a, b, c, dd] => AtnEvent::Code([*a, *b, *c, *dd]),
        _ => AtnEvent::name(event),
    };
    AtnItem { event, dictionary: label.as_bytes().to_vec(), descriptor: d, enabled: true, ..Default::default() }
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("photocraft-atn-batch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}

/// A Save As in the shape Photoshop 2024 records it, to a path on the recording computer.
fn save_as(format: Descriptor, path: &str) -> Descriptor {
    desc("null")
        .with("As  ", D::Descriptor(format))
        .with("In  ", path_value(path))
        .with("DocI", long(219.0))
        .with("Cpy ", D::Boolean(true))
        .with("LwCs", D::Boolean(true))
        .with("saveStage", enm("saveStageType", "saveSucceeded"))
}

fn scan_action() -> AtnFile {
    let image_size =
        desc("null").with("Wdth", px(30.0)).with("scaleStyles", D::Boolean(true)).with("CnsP", D::Boolean(true)).with("Intr", enm("Intp", "bicubicSharper"));
    let gray = desc("null").with("T   ", D::Class(class("Grys")));
    let rgb = desc("null").with("T   ", D::Class(class("RGBM")));
    let levels = desc("null").with("presetKind", enm("presetKindType", "presetKindCustom")).with(
        "Adjs",
        D::List(vec![D::Descriptor(
            desc("LvlA").with("Chnl", channel_ref(None)).with("Inpt", D::List(vec![long(12.0), long(240.0)])).with("Gmm ", D::Double(1.1)),
        )]),
    );
    let point = |x: f64, y: f64| D::Descriptor(desc("Pnt ").with("Hrzn", D::Double(x)).with("Vrtc", D::Double(y)));
    let curves = desc("null").with("presetKind", enm("presetKindType", "presetKindCustom")).with(
        "Adjs",
        D::List(vec![D::Descriptor(
            desc("CrvA").with("Chnl", channel_ref(None)).with("Crv ", D::List(vec![point(0.0, 0.0), point(128.0, 140.0), point(255.0, 255.0)])),
        )]),
    );
    let unsharp = desc("null").with("Amnt", pct(80.0)).with("Rds ", px(1.0)).with("Thsh", long(2.0));
    let jpeg = desc("JPEG").with("EQlt", long(10.0)).with("MttC", enm("MttC", "None"));
    let png = desc("PNGF").with("PGIT", enm("PGIT", "PGIN")).with("PNGf", enm("PNGf", "PGAd"));
    let webp = desc("WebPFormat")
        .with("compression", enm("WebPCompression", "compressionLossy"))
        .with("quality", long(80.0))
        .with("includeXMPData", D::Boolean(false))
        .with("includeEXIFData", D::Boolean(false));
    let web = desc("null")
        .with("Usng", D::Descriptor(desc("SaveForWeb").with("Op  ", enm("SWOp", "OpSa")).with("Fmt ", enm("IRFm", "JPEG")).with("Qlty", long(60.0))))
        .with("In  ", path_value("C:\\Users\\scan\\Desktop\\web\\page.jpg"));
    // Save As with "In" a folder (DIDr): Capishot can't save to a folder, but Batch decides where.
    let to_folder = save_as(desc("PNGF"), "C:\\Users\\scan\\Desktop\\out").with("DIDr", D::Boolean(true));
    let fit = desc("null").with("null", D::Reference(vec![ReferenceItem::Enumerated { class: class("Mn  "), type_id: code("MnIt"), value: code("FtOn") }]));
    let tool = desc("null").with("null", class_ref("MvTl"));
    AtnFile {
        name: UnicodeString::new_nul("Scanlation"),
        expanded: true,
        actions: vec![AtnAction {
            name: UnicodeString::new_nul("Export pages"),
            items: vec![
                item("ImgS", "Image Size", Some(image_size)),
                item("CnvM", "Convert Mode", Some(gray)),
                item("Lvls", "Levels", Some(levels)),
                item("Crvs", "Curves", Some(curves)),
                item("UnsM", "Unsharp Mask", Some(unsharp)),
                item("FltI", "Flatten Image", None),
                item("CnvM", "Convert Mode", Some(rgb)),
                item("slct", "Select", Some(fit)),
                item("slct", "Select", Some(tool)),
                item("save", "Save", Some(save_as(jpeg, "C:\\Users\\scan\\Desktop\\out\\page.jpg"))),
                item("save", "Save", Some(save_as(png, "C:\\Users\\scan\\Desktop\\out\\page.png"))),
                item("save", "Save", Some(save_as(webp, "C:\\Users\\scan\\Desktop\\out\\page.webp"))),
                item("Expr", "Export", Some(web)),
                item("save", "Save", Some(to_folder)),
            ],
            ..Default::default()
        }],
        warnings: Vec::new(),
    }
}

/// Three 40 x 60 colour pages in `dir`.
fn pages(dir: &str) {
    for (i, rgb) in [[200u8, 30, 30], [30, 200, 30], [30, 30, 200]].iter().enumerate() {
        let data: Vec<u8> = (0..40 * 60).flat_map(|_| *rgb).collect();
        let img = photocraft_codecs::Image::from_raw(40, 60, photocraft_codecs::ChannelLayout::Rgb, photocraft_codecs::SampleType::U8, data).unwrap();
        let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        std::fs::write(format!("{dir}/p{i}.png"), png).unwrap();
    }
}

#[test]
fn a_photoshop_scanlation_action_maps_every_step() {
    let imp = import(&atn::write(&scan_action()).unwrap()).unwrap();
    let unsupported: Vec<_> = imp.unsupported.iter().map(|u| (u.step, u.event.clone())).collect();
    // The tool selection (no document effect) and the Save As to a folder stay Photoshop steps.
    assert_eq!(unsupported, [(8, "slct".to_string()), (13, "save".to_string())]);
    let steps = &imp.actions[0].steps;
    let ids: Vec<&str> = steps.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "image.imageSize",
            "image.mode.grayscale",
            "image.adjustments.levels",
            "image.adjustments.curves",
            "filter.sharpen.unsharpMask",
            "layer.flattenImage",
            "image.mode.rgb",
            "view.fitOnScreen",
            PHOTOSHOP_STEP,
            "file.saveACopy",
            "file.saveACopy",
            "file.saveACopy",
            "file.export.saveForWebLegacy",
            PHOTOSHOP_STEP,
        ]
    );
    assert_eq!(steps[0].1, json!({"width": 30, "resample": "bicubicSharper"}));
    assert_eq!(steps[4].1, json!({"amount": 80, "radius": 1, "threshold": 2}));
    assert_eq!(steps[9].1, json!({"path": "C:\\Users\\scan\\Desktop\\out\\page.jpg", "quality": 10}), "JPEG Options' quality is kept");
    assert_eq!(steps[10].1, json!({"path": "C:\\Users\\scan\\Desktop\\out\\page.png"}));
    assert_eq!(steps[11].1["quality"], json!(9.58), "WebP's 0-100 quality on the 0-12 scale");
    assert!(imp.actions[0].disabled.contains(&8), "the tool selection is imported switched off");
    // Save Actions writes the qualities back where Photoshop keeps them.
    let actions: Vec<&Action> = imp.actions.iter().collect();
    let out = atn::parse(&export("Scanlation", &actions, &[], false).unwrap().0).unwrap();
    let jpeg = out.actions[0].items.iter().filter_map(|i| i.descriptor.as_ref()).find_map(|d| get_obj(d, "As  ").filter(|f| trimmed(&f.class_id) == "JPEG"));
    assert_eq!(jpeg.and_then(|f| get_num(f, "EQlt")), Some(10.0));
}

#[test]
fn batch_runs_the_photoshop_action_over_a_folder_to_avif() {
    let dir = tmp("avif");
    let input = format!("{dir}/in");
    std::fs::create_dir_all(&input).unwrap();
    pages(&input);
    let mut s = Session::new();
    let bytes = atn::write(&scan_action()).unwrap();
    s.execute("actions.load", json!({"data": photocraft_paint::tile::b64_encode(&bytes)})).unwrap();
    let action = s.actions.list.iter().find(|a| a.name == "Export pages").unwrap().clone();
    let steps: Vec<Value> = action.enabled_steps().map(|(_, (id, p))| json!([id, p])).collect();
    let output = format!("{dir}/out");
    let r = s
        .execute(
            "file.automate.batch",
            json!({"steps": steps, "input": input, "output": output, "format": "avif", "naming": "page_{serial}", "startingSerial": 1}),
        )
        .unwrap();
    assert!(r["errors"].as_array().unwrap().is_empty(), "{r}");
    let files: Vec<&str> = r["files"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(files.len(), 3, "{r}");
    for (i, f) in files.iter().enumerate() {
        assert!(f.ends_with(&format!("page_{:03}.avif", i + 1)), "{f}");
        let bytes = std::fs::read(f).unwrap();
        assert_eq!(photocraft_codecs::detect(&bytes), Some(photocraft_codecs::Format::Avif), "{f}");
        assert_eq!(&bytes[4..12], b"ftypavif");
    }
    // The Save for Web step exported each page beside its result, not to the recording computer.
    for i in 1..=3 {
        assert!(std::path::Path::new(&format!("{output}/page_{i:03}.jpg")).is_file(), "Save for Web export of page {i}");
    }
    // Fit on Screen is a view step: Batch leaves it out and says so.
    let skipped: Vec<&str> = r["skipped"].as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(skipped, ["Fit on Screen"], "{r}");
}

#[test]
fn batch_writes_the_format_of_a_recorded_export_as() {
    let dir = tmp("export-as");
    let input = format!("{dir}/in");
    std::fs::create_dir_all(&input).unwrap();
    pages(&input);
    let output = format!("{dir}/out");
    // Capishot's own Export As step (format AVIF), recorded in an action; Batch keeps the format "same".
    let steps = json!([
        ["image.adjustments.invert", {}],
        ["file.export.exportAs", {"format": "avif", "quality": 70, "speed": 8, "path": "/somewhere/else/page.avif"}]
    ]);
    let r = Session::new().execute("file.automate.batch", json!({"steps": steps, "input": input, "output": output, "format": "same"})).unwrap();
    assert!(r["errors"].as_array().unwrap().is_empty(), "{r}");
    let files: Vec<&str> = r["files"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(files.len(), 3);
    assert!(files.iter().all(|f| f.ends_with(".avif") && f.starts_with(&output)), "{files:?}");
    assert_eq!(r["skipped"], json!([]));
}
