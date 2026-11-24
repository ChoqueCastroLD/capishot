use super::*;
use crate::file_dialog::{FileDialogAnswer, FileDialogReply, FileDialogRequest};
use crate::{Services, menus};
use std::cell::RefCell;
use std::rc::Rc;

type Open = Rc<RefCell<Vec<(FileDialogRequest, FileDialogReply)>>>;
type Written = Rc<RefCell<Vec<String>>>;

/// An app whose file dialogs stay open until the test answers them, whose writer records the
/// paths written and fails for any path containing "fail", with `n` new documents.
fn app(n: usize) -> (PhotocraftApp, Open, Written) {
    let (open, written): (Open, Written) = Default::default();
    let (held, w) = (open.clone(), written.clone());
    let services = Services {
        export: Some(Box::new(|_: &photocraft_doc::Document, _: &str, _: &crate::ExportSettings| Ok((b"out".to_vec(), Vec::new())))),
        write: Some(Box::new(move |path: &str, _: &[u8]| {
            if path.contains("fail") {
                return Err("disk full".into());
            }
            w.borrow_mut().push(path.to_string());
            Ok(())
        })),
        file_dialog: Some(Box::new(move |request, _parent, reply| held.borrow_mut().push((request, reply)))),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    for _ in 0..n {
        app.run("file.new", json!({"width": 4, "height": 4})).unwrap();
    }
    (app, open, written)
}

fn make_dirty(app: &mut PhotocraftApp, i: usize) {
    app.session.set_active(i);
    app.run("layer.new.layer", json!({})).unwrap();
}

/// Gives document `i` its own layered file, saved (clean).
fn with_file(app: &mut PhotocraftApp, i: usize, path: &str) {
    app.session.set_active(i);
    app.session.active_mut().unwrap().saved_to(path.to_string());
}

fn answer(open: &Open, answer: Option<FileDialogAnswer>) {
    let (_, reply) = open.borrow_mut().pop().expect("a dialog is open");
    std::thread::spawn(move || reply.send(answer)).join().unwrap();
}

fn suggested(open: &Open) -> String {
    match open.borrow().as_slice() {
        [(FileDialogRequest::Save { suggested }, _)] => suggested.clone(),
        other => panic!("one save dialog expected, got {}", other.len()),
    }
}

fn enabled(app: &PhotocraftApp) -> bool {
    menus::menu_items(app).iter().find(|i| i.id == COMMAND).expect("File › Save All is in the menus").enabled
}

#[test]
fn saves_dirty_documents_asks_for_new_ones_and_a_cancel_skips_only_that_one() {
    let ctx = egui::Context::default();
    let (mut app, open, written) = app(4);
    // 0: saved before, edited since. 1: saved, untouched. 2 and 3: never saved, edited.
    with_file(&mut app, 0, "/pics/a.psd");
    with_file(&mut app, 1, "/pics/b.psd");
    make_dirty(&mut app, 0);
    make_dirty(&mut app, 2);
    make_dirty(&mut app, 3);
    let names: Vec<String> = app.session.documents().iter().map(|d| d.doc.name.clone()).collect();
    app.session.set_active(1);
    let first = app.session.active().unwrap().doc.id;

    menus::invoke(&mut app, &ctx, COMMAND, json!({})).unwrap();
    // The one with a file is written in place at once; the first new one asks where.
    assert_eq!(*written.borrow(), ["/pics/a.psd"]);
    app.poll_file_dialog(&ctx, None);
    assert!(suggested(&open).contains(&names[2]), "the dialog names the document: {}", suggested(&open));
    assert!(app.ui.status.contains(&names[2]), "{}", app.ui.status);
    answer(&open, Some(FileDialogAnswer::SaveTo("/pics/c.psd".into())));
    app.poll_file_dialog(&ctx, None);
    tick(&mut app, &ctx);
    assert_eq!(*written.borrow(), ["/pics/a.psd", "/pics/c.psd"]);

    // The next new one asks too; cancelling it skips it and ends the run.
    app.poll_file_dialog(&ctx, None);
    assert!(suggested(&open).contains(&names[3]), "{}", suggested(&open));
    answer(&open, None);
    app.poll_file_dialog(&ctx, None);
    tick(&mut app, &ctx);

    assert!(app.save_all.is_none(), "the run is over");
    assert!(open.borrow().is_empty(), "no other dialog");
    assert_eq!(*written.borrow(), ["/pics/a.psd", "/pics/c.psd"], "the clean document was left alone");
    let docs = app.session.documents();
    assert!(!docs[0].is_dirty() && !docs[1].is_dirty() && !docs[2].is_dirty());
    assert!(docs[3].is_dirty(), "the cancelled one keeps its changes");
    assert_eq!(docs[2].path.as_deref(), Some("/pics/c.psd"));
    assert_eq!(app.session.active().unwrap().doc.id, first, "the tab it started on is shown again");
    let notice = app.ui.notices.last().expect("a summary notice");
    assert_eq!(notice.title, "Saved 2 documents");
    assert!(!notice.error);
    assert_eq!(notice.lines.len(), 1, "{:?}", notice.lines);
    assert!(notice.lines[0].contains(&names[3]) && notice.lines[0].contains("skipped"), "{:?}", notice.lines);
    assert!(app.ui.status.contains("1 skipped"), "{}", app.ui.status);
}

#[test]
fn a_failed_save_is_reported_and_the_rest_still_save() {
    let ctx = egui::Context::default();
    let (mut app, _open, written) = app(2);
    with_file(&mut app, 0, "/pics/fail.psd");
    with_file(&mut app, 1, "/pics/ok.psd");
    make_dirty(&mut app, 0);
    make_dirty(&mut app, 1);
    menus::invoke(&mut app, &ctx, COMMAND, json!({})).unwrap();
    tick(&mut app, &ctx);
    assert!(app.save_all.is_none());
    assert_eq!(*written.borrow(), ["/pics/ok.psd"]);
    let notice = app.ui.notices.last().unwrap();
    assert_eq!(notice.title, "Saved 1 document");
    assert!(notice.error);
    assert!(notice.lines.iter().any(|l| l.contains("failed") && l.contains("disk full")), "{:?}", notice.lines);
}

#[test]
fn disabled_with_nothing_unsaved() {
    let ctx = egui::Context::default();
    let (mut app, _open, written) = app(0);
    assert!(!enabled(&app), "no document");
    app.run("file.new", json!({"width": 4, "height": 4})).unwrap();
    with_file(&mut app, 0, "/pics/a.psd");
    assert!(!enabled(&app), "only clean documents");
    assert!(menus::invoke(&mut app, &ctx, COMMAND, json!({})).is_err());
    assert!(written.borrow().is_empty());
    make_dirty(&mut app, 0);
    assert!(enabled(&app));
}

#[test]
fn menu_palette_help_and_translations_list_it() {
    let (app, ..) = app(1);
    let items = menus::menu_items(&app);
    let file: Vec<&str> = items.iter().filter(|i| i.path.first().is_some_and(|p| p == "File") && i.path.len() == 1).map(|i| i.id.as_str()).collect();
    let at = file.iter().position(|id| *id == "file.saveAs").unwrap();
    assert_eq!(file.get(at + 1), Some(&COMMAND), "right after Save As");
    let item = items.iter().find(|i| i.id == COMMAND).unwrap();
    assert_eq!(item.shortcut.as_deref().map(crate::shortcuts::parse), Some(crate::shortcuts::parse("Cmd+Alt+Shift+D")));
    assert!(menus::is_live(COMMAND));
    for query in ["guardar todo", "salvar tudo", "save all"] {
        assert_eq!(crate::palette_search::synonym_targets(query).first().map(|t| t.1), Some(COMMAND), "{query}");
    }
    assert!(crate::help_text::entry(COMMAND).is_some());
    for language in crate::i18n::LANGUAGES.iter().filter(|l| l.complete_menus || l.code == "pt-pt") {
        let lang = crate::i18n::Lang::from_code(language.code).unwrap();
        assert_ne!(crate::i18n::tr(lang, "Save All"), "Save All", "{}", language.code);
    }
    for code in ["es", "pt-br", "pt-pt"] {
        let lang = crate::i18n::Lang::from_code(code).unwrap();
        for key in [
            "No open document has unsaved changes",
            "\u{201c}{name}\u{201d} skipped: {reason}",
            "\u{201c}{name}\u{201d} failed: {reason}",
            "{n} failed",
            "cancelled",
        ] {
            assert_ne!(crate::i18n::tr(lang, key), key, "{code}: {key}");
        }
        assert_ne!(crate::i18n::trn(lang, 2, "Saved {n} document", "Saved {n} documents"), "Saved {n} documents", "{code}");
    }
}
