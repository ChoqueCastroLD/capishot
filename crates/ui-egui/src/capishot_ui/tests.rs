use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::Services;
use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};

type Written = Rc<RefCell<Vec<(String, Vec<u8>)>>>;
type Asked = Rc<RefCell<Vec<FileDialogRequest>>>;

/// An app with a 20×10 red document whose dialogs give `answers` and whose writer (the
/// browser's download) records what it was handed.
fn app(answers: Vec<Option<FileDialogAnswer>>) -> (PhotocraftApp, Asked, Written) {
    let (dialog, asked) = crate::file_dialog::fake(answers);
    let written: Written = Rc::default();
    let w = written.clone();
    let services = Services {
        file_dialog: Some(dialog),
        write: Some(Box::new(move |path: &str, bytes: &[u8]| {
            w.borrow_mut().push((path.to_string(), bytes.to_vec()));
            Ok(())
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    app.run("file.new", json!({"width": 20, "height": 10, "background": "#ff0000"})).unwrap();
    std::sync::Arc::make_mut(&mut app.session.active_mut().unwrap().doc).name = "Page 7.psd".into();
    (app, asked, written)
}

fn answer_dialogs(app: &mut PhotocraftApp) {
    let ctx = egui::Context::default();
    for _ in 0..3 {
        app.poll_file_dialog(&ctx, None);
    }
}

fn size_of(name: &str, bytes: &[u8]) -> (u32, u32) {
    let doc = photocraft_io::import(name, bytes).unwrap().document;
    (doc.size.width, doc.size.height)
}

fn psd(app: &PhotocraftApp) -> Vec<u8> {
    photocraft_io::export(&app.session.active().unwrap().doc, "p.psd", &Default::default()).unwrap().bytes
}

#[test]
fn web_compile_psd_downloads_the_compiled_page() {
    let (mut app, asked, written) = app(vec![Some(FileDialogAnswer::SaveTo("Page 7.png".into()))]);
    let r = compile_psd(&mut app, &json!({}), true).unwrap().unwrap();
    assert_eq!(r["fileDialog"], "save");
    answer_dialogs(&mut app);
    assert!(matches!(asked.borrow().as_slice(), [FileDialogRequest::Save { suggested }] if suggested == "Page 7.png"), "{:?}", asked.borrow());
    let written = written.borrow();
    let [(name, bytes)] = written.as_slice() else { panic!("one download expected") };
    assert_eq!(name, "Page 7.png");
    assert_eq!(size_of(name, bytes), (20, 10));
    assert!(!app.ui.status_error && app.ui.status.contains("Page 7.png"), "{}", app.ui.status);
}

#[test]
fn web_compile_psd_with_a_path_downloads_without_a_dialog() {
    let (mut app, asked, written) = app(vec![]);
    let r = compile_psd(&mut app, &json!({"path": "out.jpg", "quality": 80}), true).unwrap().unwrap();
    assert_eq!(r["width"], 20);
    assert!(asked.borrow().is_empty());
    assert_eq!(written.borrow().first().map(|(n, _)| n.as_str()), Some("out.jpg"));
    // Bad params fail without a download.
    assert!(compile_psd(&mut app, &json!({"path": "out.gif"}), true).unwrap().is_err());
    assert_eq!(written.borrow().len(), 1);
    assert!(app.ui.status_error);
}

#[test]
fn web_compile_folder_zips_the_picked_files() {
    let (mut app, asked, written) = app(vec![]);
    let page = psd(&app);
    let (dialog, asked2) =
        crate::file_dialog::fake(vec![Some(FileDialogAnswer::ContentsMany(vec![("02.psd".into(), page.clone()), ("01.psd".into(), page.clone())]))]);
    app.services.file_dialog = Some(dialog);
    let r = compile_folder(&mut app, &json!({}), true).unwrap().unwrap();
    assert_eq!(r["fileDialog"], "open");
    answer_dialogs(&mut app);
    assert!(asked.borrow().is_empty());
    assert!(matches!(
        asked2.borrow().as_slice(),
        [FileDialogRequest::Open { multiple: true, extensions: Some(exts), .. }] if exts == &["psd", "psb"]
    ));
    let written = written.borrow();
    let [(name, zip)] = written.as_slice() else { panic!("one download expected") };
    assert_eq!(name, ZIP_NAME);
    let reader = photocraft_format::zip::ZipReader::new(zip).unwrap();
    for page in ["01.png", "02.png"] {
        let e = reader.find(page).unwrap();
        assert_eq!(size_of(page, &reader.read(e, 1 << 20).unwrap()), (20, 10));
    }
    assert!(app.ui.status.contains("2 page(s)"), "{}", app.ui.status);
}

#[test]
fn web_compile_folder_reports_pages_that_fail_and_never_reads_a_folder() {
    let (mut app, _, written) = app(vec![Some(FileDialogAnswer::ContentsMany(vec![("bad.psd".into(), b"8BPS nope".to_vec())]))]);
    compile_folder(&mut app, &json!({}), true).unwrap().unwrap();
    answer_dialogs(&mut app);
    assert!(written.borrow().is_empty());
    assert!(app.ui.status_error && app.ui.status.contains("bad.psd"), "{}", app.ui.status);
    assert!(compile_folder(&mut app, &json!({"input": "/x", "output": "/y"}), true).unwrap().is_err());
}

#[test]
fn cancelling_writes_nothing_and_reports_nothing() {
    let (mut app, _, written) = app(vec![None]);
    compile_psd(&mut app, &json!({}), true).unwrap().unwrap();
    answer_dialogs(&mut app);
    let (mut app2, _, written2) = self::app(vec![None]);
    compile_folder(&mut app2, &json!({}), true).unwrap().unwrap();
    answer_dialogs(&mut app2);
    assert!(written.borrow().is_empty() && written2.borrow().is_empty());
    assert!(!app.ui.status_error && !app2.ui.status_error);
}

#[test]
fn desktop_compile_psd_asks_where_and_writes_the_file() {
    let dir = std::env::temp_dir().join(format!("capishot-ui-compile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("page.png").to_string_lossy().into_owned();
    let (mut app, _, written) = app(vec![Some(FileDialogAnswer::SaveTo(path.clone()))]);
    compile_psd(&mut app, &json!({}), false).unwrap().unwrap();
    answer_dialogs(&mut app);
    assert!(written.borrow().is_empty(), "the engine writes desktop files");
    assert_eq!(size_of(&path, &std::fs::read(&path).unwrap()), (20, 10));
    // With params the engine command runs as is.
    assert!(compile_psd(&mut app, &json!({"path": path}), false).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn desktop_compile_folder_opens_its_form() {
    let (mut app, asked, _) = app(vec![]);
    let r = compile_folder(&mut app, &json!({}), false).unwrap().unwrap();
    let id = r["dialog"].as_u64().unwrap();
    let d = app.ui.dialogs.iter().find(|d| d.id == id).unwrap();
    assert_eq!(d.fields["__command"], COMPILE_FOLDER);
    assert_eq!(d.fields["format"], "png");
    assert!(asked.borrow().is_empty());
    assert!(compile_folder(&mut app, &json!({"input": ".", "output": "out"}), false).is_none());
}

#[test]
fn the_menu_reaches_the_front_ends() {
    let (mut app, _, _) = app(vec![]);
    let ctx = egui::Context::default();
    let r = crate::menus::invoke(&mut app, &ctx, COMPILE, json!({})).unwrap();
    assert_eq!(r["fileDialog"], "save");
    let mut empty = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    assert!(compile_psd(&mut empty, &json!({}), true).unwrap().is_err());
}

/// Saving a PSD goes through the export service, which both the desktop and the web shell build
/// from [`crate::ExportSettings::io_options`]: the merged image is psd-compiler's (8-bit RGB).
#[test]
fn psd_saves_through_the_shells_export_options_carry_the_compiled_composite() {
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::{Document, Layer};
    use photocraft_geom::{Rect, Size};
    let mut doc = Document::new("t", Size::new(64, 48), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    let mut bg = Layer::raster("blue", fmt);
    bg.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 48), &[0.2, 0.4, 0.8, 1.0]);
    let mut top = Layer::raster("red", fmt);
    top.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 48), &[1.0, 0.0, 0.0, 1.0]);
    top.opacity = 0.5;
    doc.layers.push(bg);
    doc.layers.push(top);
    let opts = crate::ExportSettings::default().io_options();
    assert!(opts.psdc_composite);
    let out = photocraft_io::export(&doc, "page.psd", &opts).unwrap();
    assert!(out.warnings.iter().all(|w| !w.contains("psd-compiler")), "{:?}", out.warnings);
    let file = photocraft_psd::PsdFile::from_bytes(&out.bytes).unwrap();
    let rgba = file.composite_rgba8().unwrap();
    // 50 % red over (51, 102, 204).
    assert!(rgba.data[..4].iter().zip([153u8, 51, 102, 255]).all(|(g, w)| g.abs_diff(w) <= 1), "{:?}", &rgba.data[..4]);
}
