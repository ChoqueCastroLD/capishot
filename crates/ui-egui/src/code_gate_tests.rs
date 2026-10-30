use std::cell::{Cell, RefCell};
use std::rc::Rc;

use photocraft_engine::Session;
use serde_json::json;

use super::{CodeDone, CodeWait, RETRY};
use crate::{PhotocraftApp, Services};

/// An app whose code loader is driven by the test: `loaded` says whether everything is in,
/// `first` lists what runs before, `done` is the pending load's completion.
struct Loader {
    loaded: Rc<Cell<bool>>,
    done: Rc<RefCell<Option<CodeDone>>>,
    waits: Rc<Cell<u32>>,
}

fn app() -> (PhotocraftApp, Loader) {
    let loaded = Rc::new(Cell::new(false));
    let done: Rc<RefCell<Option<CodeDone>>> = Rc::default();
    let waits = Rc::new(Cell::new(0));
    let (l, d, w) = (loaded.clone(), done.clone(), waits.clone());
    let mut services = Services::default();
    services.code_wait = Some(CodeWait {
        ready: Box::new(move |id| l.get() || id == "file.new" || id == "open:jpg"),
        wait: Box::new(move |_ctx, f| {
            w.set(w.get() + 1);
            *d.borrow_mut() = Some(f);
        }),
    });
    let mut app = PhotocraftApp::new(Session::new(), services);
    app.run("file.new", json!({"width": 16, "height": 16})).expect("a document");
    (app, Loader { loaded, done, waits })
}

/// Let the job's end reach the app (it arrives through the job channel).
fn settle(app: &mut PhotocraftApp, ctx: &egui::Context) {
    for _ in 0..200 {
        crate::jobs_ui::tick(app, ctx);
        if app.code_waiting.job.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn history_len(app: &PhotocraftApp) -> usize {
    app.session.active().map_or(0, |st| st.history.entries().len())
}

#[test]
fn a_command_whose_code_isnt_in_waits_for_it_then_runs() {
    let (mut app, loader) = app();
    let ctx = egui::Context::default();
    let before = history_len(&app);
    let r = crate::menus::invoke(&mut app, &ctx, "image.adjustments.invert", json!({})).expect("waits");
    assert_eq!(r["pending"], true, "{r}");
    assert_eq!(history_len(&app), before, "not run yet");
    // A second one joins the same load.
    crate::menus::invoke(&mut app, &ctx, "image.adjustments.invert", json!({})).expect("waits");
    assert_eq!(loader.waits.get(), 1, "one load");
    assert!(app.session.jobs().iter().any(|j| j.command == super::JOB), "a job card");
    loader.loaded.set(true);
    (loader.done.borrow_mut().take().expect("the loader was asked"))(Ok(()));
    settle(&mut app, &ctx);
    assert_eq!(history_len(&app), before + 2, "both ran once the code was in");
}

#[test]
fn what_is_in_the_first_part_runs_at_once() {
    let (mut app, loader) = app();
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "file.new", json!({"width": 8, "height": 8})).expect("runs");
    assert_eq!(loader.waits.get(), 0);
    assert!(super::open_ready(&mut app, "page.JPG"));
    assert!(!super::open_ready(&mut app, "scan.tiff"), "a TIFF waits");
    assert!(!super::files_ready(&mut app));
    (loader.done.borrow_mut().take().expect("asked"))(Ok(()));
    settle(&mut app, &ctx);
    assert!(super::files_ready(&mut app));
}

#[test]
fn a_failed_load_drops_the_command_with_a_notice_whose_retry_runs_it() {
    let (mut app, loader) = app();
    let ctx = egui::Context::default();
    let before = history_len(&app);
    crate::menus::invoke(&mut app, &ctx, "image.adjustments.invert", json!({})).expect("waits");
    (loader.done.borrow_mut().take().expect("asked"))(Err("network".into()));
    settle(&mut app, &ctx);
    assert_eq!(history_len(&app), before, "dropped");
    let notice = app.ui.notices.last().expect("a notice");
    assert!(notice.error && notice.actions.iter().any(|a| a.command == RETRY), "{notice:?}");
    crate::menus::invoke(&mut app, &ctx, RETRY, json!({})).expect("retries");
    assert_eq!(loader.waits.get(), 2, "loads again");
    loader.loaded.set(true);
    (loader.done.borrow_mut().take().expect("asked"))(Ok(()));
    settle(&mut app, &ctx);
    assert_eq!(history_len(&app), before + 1, "ran on retry");
}

#[test]
fn control_requests_wait_in_order() {
    let (mut app, loader) = app();
    let ctx = egui::Context::default();
    let (tx, rx) = std::sync::mpsc::channel();
    app.control_rx = Some(rx);
    let before = history_len(&app);
    let (a, ra) = crate::control::ControlRequest::new("engine.execute", json!({"command": "image.adjustments.invert", "params": {}}));
    let (b, rb) = crate::control::ControlRequest::new("engine.execute", json!({"command": "file.new", "params": {"width": 4, "height": 4}}));
    tx.send(a).expect("send");
    tx.send(b).expect("send");
    app.drain_control(&ctx);
    assert!(ra.try_recv().is_err() && rb.try_recv().is_err(), "both held (the second keeps its place)");
    loader.loaded.set(true);
    (loader.done.borrow_mut().take().expect("asked"))(Ok(()));
    settle(&mut app, &ctx);
    app.drain_control(&ctx);
    assert_eq!(ra.try_recv().expect("first answered")["ok"], true);
    assert_eq!(rb.try_recv().expect("second answered")["ok"], true);
    assert!(app.session.documents().len() >= 2 && history_len(&app) >= before, "ran");
}
