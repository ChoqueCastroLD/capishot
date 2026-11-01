//! Export Open Documents as ZIP from the app: the dialog (every open document, in tab order,
//! fitting a 360 px phone), and the export into a download written a piece at a time, finished
//! only when every page is in (a failed export downloads nothing).

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;

#[derive(Default)]
struct Got {
    chunks: Vec<Vec<u8>>,
    finished: Vec<String>,
}

struct Fake {
    name: String,
    got: Arc<Mutex<Got>>,
}

impl crate::DownloadStream for Fake {
    fn push(&mut self, chunk: Vec<u8>) -> Result<(), String> {
        self.got.lock().unwrap().chunks.push(chunk);
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<(), String> {
        self.got.lock().unwrap().finished.push(self.name.clone());
        Ok(())
    }
}

fn app(n: usize) -> (PhotocraftApp, Arc<Mutex<Got>>) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    for i in 0..n {
        app.run("file.new", json!({"width": 120, "height": 80, "background": "white", "name": format!("p{}", i + 1)})).unwrap();
    }
    let got: Arc<Mutex<Got>> = Arc::default();
    let g = Arc::clone(&got);
    app.services.download_stream =
        Some(Box::new(move |name: &str| Ok(Box::new(Fake { name: name.into(), got: Arc::clone(&g) }) as Box<dyn crate::DownloadStream>)));
    (app, got)
}

fn names(zip: &[u8]) -> Vec<String> {
    photocraft_format::zip::ZipReader::new(zip).unwrap().entries.iter().map(|e| e.name.clone()).collect()
}

#[test]
fn the_dialog_lists_the_open_documents_in_tab_order_and_exports_them() {
    let (mut app, got) = app(3);
    crate::zip_export_ui::open(&mut app);
    let docs: Vec<&str> = app.zip_export.rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(docs, ["p1", "p2", "p3"]);
    assert!(app.zip_export.rows.iter().all(|r| r.checked), "all checked");
    // Reordered (the third first), numbered.
    let r = app.zip_export.rows.remove(2);
    app.zip_export.rows.insert(0, r);
    app.zip_export.naming = "sequence".into();
    app.zip_export.format = "jpg".into();
    let p = crate::zip_export_ui::params(&app.zip_export);
    crate::zip_export_ui::start(&mut app, &p, "capitulo").unwrap();
    // On a worker: wait for it, as the app's frames do.
    for _ in 0..500 {
        app.session.poll_jobs();
        crate::zip_export_ui::tick(&mut app);
        if !got.lock().unwrap().finished.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(app.ui.status.contains('3'), "the report: {}", app.ui.status);
    let g = got.lock().unwrap();
    assert_eq!(g.finished, ["capitulo.zip"]);
    assert!(g.chunks.len() >= 4, "a piece per page and the directory: {}", g.chunks.len());
    let zip: Vec<u8> = g.chunks.concat();
    assert_eq!(names(&zip), ["001.jpg", "002.jpg", "003.jpg"]);
}

#[test]
fn an_export_that_fails_downloads_nothing() {
    let (mut app, got) = app(1);
    // A document with no pixels can't be encoded: no page, an error, no download.
    let r = app.run("file.new", json!({"width": 10, "height": 10}));
    assert!(r.is_ok());
    let ids: Vec<u64> = app.session.documents().iter().map(|d| d.doc.id.0).collect();
    let p = json!({"docs": [ids[0]], "format": "png", "resize": {"maxWidth": 0}});
    assert!(crate::zip_export_ui::start(&mut app, &p, "x.zip").is_err(), "bad params refused");
    assert!(got.lock().unwrap().finished.is_empty());
}

#[test]
fn on_a_360_phone_the_dialog_fits() {
    let (mut a, _) = app(4);
    a.run("prefs.set", json!({"values": {"interface.simpleUi": "on"}})).unwrap();
    crate::zip_export_ui::open(&mut a);
    let mut h = Harness::builder().with_size(egui::vec2(360.0, 780.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        a
    });
    h.run_steps(6);
    let screen = h.ctx.content_rect();
    for l in ["export", "cancel"] {
        let r = crate::zip_export_ui::noted(&h.ctx, l).unwrap_or_else(|| panic!("{l}"));
        assert!(screen.contains_rect(r), "{l} {r:?} inside {screen:?}");
    }
    let win = h.ctx.memory(|m| m.area_rect(egui::Id::unique("zip-export-window"))).expect("the dialog");
    assert!(win.width() <= 360.0 + 0.5 && win.left() >= -0.5, "{win:?}");
}
