//! The contract, headless: the steps the app's buttons run, in order, against the engine API.
//! If this passes, a host built the same way can open, edit, undo and save a page in process.

#![cfg(not(target_arch = "wasm32"))]

use capishot_host::{Event, ExecOptions, Host, OpenOptions};
use capishot_host_egui::script::{self, Removal};

#[test]
fn open_tool_remove_undo_text_save() {
    let mut host = Host::new();
    let events = host.events();

    // 1. A PSD from bytes, shown.
    let bytes = script::sample_psd(&mut host).unwrap();
    assert_eq!(&bytes[..4], b"8BPS", "PSD bytes");
    let (doc, warnings) = script::open(&mut host, "page.psd", bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(host.active_document(), Some(doc));
    let page = script::paint_target(&host, doc).unwrap().expect("the page's pixel layer").id;
    let before = script::blob_pixels(&mut host, doc, page).unwrap();

    // 2. The host sets the tool; the editor reports it.
    script::set_tool(&mut host, "removeAi").unwrap();
    assert_eq!(host.tool(), "removeAi");
    host.poll();
    assert!(events.try_iter().any(|e| matches!(e, Event::ToolChanged { ref tool } if tool == "removeAi")));
    assert!(script::set_tool(&mut host, "not a tool!").is_err(), "a bad tool id is an error, not a panic");

    // 3. Remove with AI over the blob (no download in tests: without the model, or without AI in
    // this build, the plain Remove runs so the rest of the contract still does).
    match script::remove_ai(&mut host, doc, false).unwrap() {
        Removal::Job(job) => {
            if host.wait(job).is_err() {
                host.execute_with("paint.remove", script::blob_stroke(), ExecOptions::on(doc)).unwrap();
            }
        }
        Removal::Done(_) | Removal::Plain(_) => {}
    }
    host.poll();
    let removed = script::blob_pixels(&mut host, doc, page).unwrap();
    assert_ne!(removed, before, "the blob changed");

    // 4. One undo brings it back.
    assert!(script::undo(&mut host, doc).unwrap());
    assert_eq!(script::blob_pixels(&mut host, doc, page).unwrap(), before, "one undo step undid the removal");

    // 5. A text layer.
    let text = script::add_text(&mut host, doc, "¡Hola!").unwrap();
    let rows = script::layers(&host, doc).unwrap();
    assert!(rows.iter().any(|r| r.id == text && r.kind == "type"), "{rows:?}");

    // Host list actions.
    script::set_visible(&mut host, doc, text, false).unwrap();
    assert!(!script::layers(&host, doc).unwrap().iter().any(|r| r.id == text && r.visible));
    script::select(&mut host, doc, page).unwrap();

    // 6. Saved as PSD bytes, which open again with both layers.
    let saved = script::save(&mut host, doc).unwrap();
    assert_eq!(&saved[..4], b"8BPS");
    let again = host.open(saved, OpenOptions { name: Some("again.psd".into()), hidden: true }).unwrap().document;
    assert_eq!(host.active_document(), Some(doc), "a hidden document leaves the shown one active");
    let kinds: Vec<String> = script::layers(&host, again).unwrap().into_iter().map(|r| r.kind).collect();
    assert!(kinds.iter().any(|k| k == "type") && kinds.iter().any(|k| k == "pixel"), "{kinds:?}");
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    let mut host = Host::new();
    assert!(script::open(&mut host, "junk.psd", b"8BPS not really".to_vec()).is_err());
    assert!(script::open(&mut host, "empty.psd", Vec::new()).is_err());
    let missing = capishot_host::DocumentId(9999);
    assert!(script::undo(&mut host, missing).is_err());
    assert!(script::save(&mut host, missing).is_err());
    assert!(script::layers(&host, missing).is_err());
    assert!(script::add_text(&mut host, missing, "x").is_err());
}
