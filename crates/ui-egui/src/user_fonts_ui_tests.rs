//! Load Fonts…, Manage Fonts… and the missing fonts notice, without a browser. The index and the
//! font database are process-wide, so each test uses fonts of its own (renamed bundled fonts).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_engine::Session;
use serde_json::json;

use super::*;
use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::{PhotocraftApp, Services};

/// `source` with every UTF-16 occurrence of `from` in its names replaced by `to` (same length).
fn renamed(source: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.chars().count(), to.chars().count());
    let enc = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    let (from, to) = (enc(from), enc(to));
    let mut b = source.to_vec();
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

/// Inter as a five-letter family.
fn sans(name: &str) -> Vec<u8> {
    renamed(&photocraft_text::fonts::INTER_REGULAR, "Inter", name)
}

/// JetBrains Mono (which draws unlike the fallback, Inter) as a fourteen-letter family.
fn mono(name: &str) -> Vec<u8> {
    renamed(&photocraft_text::fonts::JETBRAINS_MONO_REGULAR, "JetBrains Mono", name)
}

fn listed(name: &str) -> bool {
    crate::type_tool::families().iter().any(|f| f == name)
}

fn registered(name: &str) -> bool {
    photocraft_text::shared().lock().unwrap().fonts.has_family(name)
}

fn id_of(family: &str) -> String {
    photocraft_text::user_fonts::store().entries().iter().find(|e| e.provides(family)).map(|e| e.id.clone()).expect("in the index")
}

fn notice<'a>(app: &'a PhotocraftApp, title_start: &str) -> Option<&'a crate::notices::Notice> {
    app.ui.notices.iter().find(|n| n.title.starts_with(title_start))
}

/// An app whose browser-style picker records what it was asked for and hands over `files`.
fn web_app(files: Vec<(String, Vec<u8>)>) -> (PhotocraftApp, Rc<RefCell<Vec<bool>>>) {
    let asked: Rc<RefCell<Vec<bool>>> = Rc::default();
    let log = asked.clone();
    let pick: PickFontsFn = Box::new(move |folder, sink| {
        log.borrow_mut().push(folder);
        for (name, bytes) in files.clone() {
            sink.file(name, bytes);
        }
        sink.done(false);
    });
    (PhotocraftApp::new(Session::new(), Services { pick_fonts: Some(pick), ..Default::default() }), asked)
}

#[test]
fn load_fonts_registers_the_picked_files_and_sums_them_up() {
    let (show, asked) = crate::file_dialog::fake(vec![
        Some(FileDialogAnswer::ContentsMany(vec![
            ("Fonts/Zrtyu-Regular.ttf".into(), sans("Zrtyu")),
            ("Bad.ttf".into(), b"not a font at all".to_vec()),
            ("Wide.woff2".into(), b"wOF2 fake".to_vec()),
        ])),
        Some(FileDialogAnswer::ContentsMany(vec![("Copy.ttf".into(), sans("Zrtyu"))])),
    ]);
    let mut app = PhotocraftApp::new(Session::new(), Services { file_dialog: Some(show), ..Default::default() });
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap();
    app.poll_file_dialog(&ctx, None);
    let request = asked.borrow()[0].clone();
    let FileDialogRequest::Open { multiple, extensions, .. } = &request else { panic!("an open dialog") };
    assert!(*multiple);
    assert_eq!(extensions.as_deref(), Some(&["ttf".to_string(), "otf".into(), "ttc".into(), "otc".into(), "woff".into()][..]));
    // Usable at once: registered, in the font menus, in the index, handed to psd-compiler.
    assert!(registered("Zrtyu") && listed("Zrtyu"));
    let id = id_of("Zrtyu");
    assert!(photocraft_text::shared().lock().unwrap().fonts.font_data().iter().any(|d| d.as_ref() == sans("Zrtyu").as_slice()));
    assert!(photocraft_text::user_fonts::store().is_loaded(&id));
    let n = notice(&app, "Fonts loaded").expect("a summary notice");
    assert_eq!(n.title, "Fonts loaded: 1");
    assert!(n.lines.contains(&"Zrtyu".to_string()), "{:?}", n.lines);
    assert!(n.lines.iter().any(|l| l.starts_with("Skipped Bad.ttf: not a font")), "{:?}", n.lines);
    assert!(n.lines.iter().any(|l| l.contains("Wide.woff2") && l.contains("WOFF2")), "{:?}", n.lines);
    // The same file again (another name): loaded once.
    app.ui.notices.clear();
    crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap();
    app.poll_file_dialog(&ctx, None);
    let n = notice(&app, "Fonts loaded").expect("a summary notice");
    assert!(n.lines.contains(&"Already loaded: 1".to_string()), "{:?}", n.lines);
    assert_eq!(photocraft_text::user_fonts::store().entries().iter().filter(|e| e.provides("Zrtyu")).count(), 1);
    assert!(remove(&mut app, &id).is_some());
    assert!(!registered("Zrtyu") && !listed("Zrtyu"));
}

#[test]
fn a_cancelled_or_empty_pick_says_so() {
    let (show, _) = crate::file_dialog::fake(vec![None, Some(FileDialogAnswer::ContentsMany(vec![("notes.txt".into(), b"hi".to_vec())]))]);
    let mut app = PhotocraftApp::new(Session::new(), Services { file_dialog: Some(show), ..Default::default() });
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap();
    app.poll_file_dialog(&ctx, None);
    assert!(app.ui.notices.is_empty() && !app.ui.status_error, "Cancel is not an error");
    crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap();
    app.poll_file_dialog(&ctx, None);
    let n = notice(&app, "No fonts were found").expect("a notice");
    assert!(n.error);
}

#[test]
fn the_browser_pickers_get_files_or_a_folder() {
    let (mut app, asked) = web_app(vec![("My Fonts/sub/Plkjh.ttf".into(), sans("Plkjh"))]);
    let ctx = egui::Context::default();
    assert_eq!(crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap(), json!({"picker": "files"}));
    assert_eq!(crate::menus::invoke(&mut app, &ctx, "type.loadFontFolder", json!({})).unwrap(), json!({"picker": "folder"}));
    assert_eq!(*asked.borrow(), [false, true]);
    tick(&mut app, Some(&ctx));
    assert!(registered("Plkjh"));
    let n = notice(&app, "Fonts loaded").unwrap();
    assert!(n.lines.contains(&"Already loaded: 1".to_string()) || app.ui.notices.len() == 1, "the second pick found the same file");
    let id = id_of("Plkjh");
    remove(&mut app, &id);
}

#[test]
fn dropped_font_files_are_loaded_not_opened() {
    let mut app = PhotocraftApp::new(Session::new(), Services::default());
    assert!(app.open_bytes("Dropped.otf", &sans("Drpqz")).unwrap().is_empty());
    assert!(app.session.active().is_none(), "no document");
    assert!(registered("Drpqz"));
    let id = id_of("Drpqz");
    remove(&mut app, &id);
}

/// A document with one type layer set in `family` (missing until the test loads it).
fn doc_with_text(app: &mut PhotocraftApp, family: &str) -> photocraft_doc::LayerId {
    app.run("file.new", json!({"width": 240, "height": 80, "background": "white"})).unwrap();
    let r = app.run("type.create", json!({"x": 10, "y": 50, "text": "Wiiiide MMM", "size": 30, "font": family})).unwrap();
    photocraft_doc::LayerId(r["layer"].as_u64().unwrap())
}

fn ink(app: &PhotocraftApp, id: photocraft_doc::LayerId) -> photocraft_geom::Rect {
    app.session.active().unwrap().doc.layer(id).unwrap().surface().unwrap().content_bounds()
}

#[test]
fn the_missing_fonts_notice_offers_load_fonts_and_goes_once_they_arrive() {
    let family = "Missing Mono A";
    let (mut app, asked) = web_app(vec![("MissingMonoA.ttf".into(), mono(family))]);
    let layer = doc_with_text(&mut app, family);
    let before = ink(&app, layer);
    after_open(&mut app);
    let n = notice(&app, "Missing fonts").expect("a missing fonts notice");
    assert_eq!(n.lines, [family]);
    let labels: Vec<&str> = n.actions.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, ["Load Fonts…", "Resolve Missing Fonts…"]);
    assert_eq!(n.actions[0].command, "type.loadFonts");
    // The notice's Load Fonts… button, clicked.
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                crate::notices::show(app, ui.ctx());
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(3);
    h.get_by_label("Load Fonts…").click();
    h.run_steps(2);
    assert_eq!(*asked.borrow(), [false], "the picker opened");
    let mut app = h.into_state();
    tick(&mut app, None);
    assert!(registered(family));
    assert!(notice(&app, "Missing fonts").is_none(), "nothing is missing any more");
    assert!(photocraft_engine::type_extra_cmds::missing_fonts(&app.session.active().unwrap().doc).is_empty());
    // The layer drawn with the fallback is drawn with the font now (monospace: wider).
    let after = ink(&app, layer);
    assert_ne!(before, after, "re-rendered");
    assert!(after.width() > before.width(), "{before:?} → {after:?}");
    // Removing it falls back again.
    let id = id_of(family);
    remove(&mut app, &id);
    tick(&mut app, None);
    assert_eq!(ink(&app, layer), before);
}

#[test]
fn stored_fonts_are_read_when_an_opened_document_needs_them() {
    let family = "Stored Mono BB";
    let data = mono(family);
    let p = photocraft_text::user_fonts::prepare("StoredMono.ttf", data.clone()).unwrap();
    photocraft_text::user_fonts::store().restore([serde_json::to_string(&p.entry).unwrap()]);
    let mut app = PhotocraftApp::new(Session::new(), Services::default());
    assert!(listed(family), "listed before its bytes are read");
    let layer = doc_with_text(&mut app, family);
    let before = ink(&app, layer);
    after_open(&mut app);
    assert!(notice(&app, "Missing fonts").is_none(), "a stored font provides it: no notice");
    let ids = take_loads(false);
    assert!(ids.contains(&p.entry.id), "{ids:?}");
    // The shell reads it back from its store and hands it over.
    app.font_sink(None).send(Delivery::Stored(p.entry.id.clone(), Ok(data)));
    tick(&mut app, None);
    assert!(registered(family) && photocraft_text::user_fonts::store().is_loaded(&p.entry.id));
    assert_ne!(ink(&app, layer), before, "re-rendered with it");
    assert!(app.ui.notices.is_empty(), "a restore is silent");
    // A stored record that no longer reads is dropped.
    let other = photocraft_text::user_fonts::prepare("x.ttf", mono("Broken Mono CC")).unwrap();
    photocraft_text::user_fonts::store().restore([serde_json::to_string(&other.entry).unwrap()]);
    app.font_sink(None).send(Delivery::Stored(other.entry.id.clone(), Ok(b"garbage".to_vec())));
    tick(&mut app, None);
    assert!(photocraft_text::user_fonts::store().entry(&other.entry.id).is_none());
    remove(&mut app, &p.entry.id);
}

#[test]
fn a_full_browser_storage_is_reported_once_and_the_fonts_stay() {
    let (mut app, _) = web_app(vec![("Qtqta.ttf".into(), sans("Qtqta")), ("Qtqtb.ttf".into(), sans("Qtqtb"))]);
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap();
    tick(&mut app, None);
    let (a, b) = (id_of("Qtqta"), id_of("Qtqtb"));
    let sink = app.font_sink(None);
    sink.send(Delivery::Written(a.clone(), Err("QuotaExceededError: The quota has been exceeded.".into())));
    tick(&mut app, None);
    sink.send(Delivery::Written(b.clone(), Err("QuotaExceededError: The quota has been exceeded.".into())));
    tick(&mut app, None);
    let full: Vec<_> = app.ui.notices.iter().filter(|n| n.title == "Browser storage is full").collect();
    assert_eq!(full.len(), 1, "one notice for both");
    assert!(full[0].error);
    assert!(full[0].lines[0].contains("Qtqta") && full[0].lines[0].contains("Qtqtb"), "{:?}", full[0].lines);
    assert_eq!(full[0].actions[0].command, "type.manageFonts");
    assert!(registered("Qtqta") && registered("Qtqtb"), "usable for the session");
    assert!(photocraft_text::user_fonts::store().is_unsaved(&a));
    remove(&mut app, &a);
    remove(&mut app, &b);
}

#[test]
fn manage_fonts_lists_and_removes() {
    let (mut app, _) = web_app(vec![("Mngfa.ttf".into(), sans("Mngfa"))]);
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "type.loadFonts", json!({})).unwrap();
    tick(&mut app, None);
    let d = crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({})).unwrap()["dialog"].as_u64().unwrap();
    assert_eq!(crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({})).unwrap()["dialog"].as_u64(), Some(d), "one dialog");
    assert!(photocraft_text::user_fonts::store().want_estimate, "the shell refreshes the storage figures");
    let mut h = Harness::builder().with_size(egui::vec2(700.0, 700.0)).build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            let Some(mut fields) = app.ui.dialog_mut(d).map(|d| d.fields.clone()) else { return };
            body(app, ui, &mut fields);
            if let Some(dl) = app.ui.dialog_mut(d) {
                dl.fields = fields;
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(3);
    assert!(h.query_by_label("Mngfa").is_some(), "the family is listed");
    assert!(h.query_by_label("Loaded for this session only.").is_some());
    let mut app = h.into_state();
    let id = id_of("Mngfa");
    // Agents: list and remove by id.
    let list = crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({"list": true})).unwrap();
    let mine = list["fonts"].as_array().unwrap().iter().find(|f| f["id"] == json!(id)).cloned().unwrap();
    assert_eq!(mine["faces"][0]["family"], "Mngfa");
    assert_eq!(mine["loaded"], true);
    assert_eq!(list["kept"], "session");
    assert!(crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({"remove": 5})).is_err());
    assert!(crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({"remove": "nope"})).is_err());
    assert!(crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({"bogus": 1})).is_err());
    assert_eq!(crate::menus::invoke(&mut app, &ctx, "type.manageFonts", json!({"remove": id})).unwrap(), json!({"removed": "Mngfa.ttf"}));
    assert!(remove(&mut app, &id).is_none());
    assert!(!registered("Mngfa"));
    assert!(photocraft_text::user_fonts::store().entry(&id).is_none());
}

#[test]
fn resolve_missing_fonts_offers_load_fonts_and_picks_loaded_matches() {
    // A PSD-style family spelled without spaces: the loaded font matches it loosely.
    let (mut app, asked) = web_app(vec![("Loose.ttf".into(), mono("Loose Match DD"))]);
    doc_with_text(&mut app, "LooseMatchDD");
    let ctx = egui::Context::default();
    let d = crate::menus::invoke(&mut app, &ctx, "type.resolveMissingFonts", json!({})).unwrap()["dialog"].as_u64().unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(700.0, 700.0)).build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            let Some(mut fields) = app.ui.dialog_mut(d).map(|d| d.fields.clone()) else { return };
            crate::fonts_ui::body(app, ui, &mut fields);
            if let Some(dl) = app.ui.dialog_mut(d) {
                dl.fields = fields;
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(3);
    h.get_by_label("Load Fonts…").click();
    h.run_steps(2);
    assert_eq!(*asked.borrow(), [false]);
    let mut app = h.into_state();
    let layer = app.session.active().unwrap().active_layer.unwrap();
    let before = ink(&app, layer);
    tick(&mut app, None);
    // Mapped on its own (one undo step), redrawn, and the dialog has nothing left to resolve.
    let family = |app: &PhotocraftApp| match &app.session.active().unwrap().doc.layer(layer).unwrap().content {
        photocraft_doc::LayerContent::Text(t) => t.char_runs()[0].style.font_family.clone(),
        _ => String::new(),
    };
    assert_eq!(family(&app), "Loose Match DD");
    assert_eq!(app.ui.dialog_mut(d).unwrap().fields["missing"], json!([]));
    assert_ne!(ink(&app, layer), before, "redrawn with the loaded font");
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(family(&app), "LooseMatchDD", "undoable");
    let id = id_of("Loose Match DD");
    remove(&mut app, &id);
}

#[test]
fn the_commands_are_in_the_type_menu_and_counted() {
    let app = PhotocraftApp::new(Session::new(), Services::default());
    let items = crate::menus::menu_items(&app);
    let at = |id: &str| items.iter().position(|i| i.id == id).unwrap_or_else(|| panic!("{id} in the menus"));
    assert_eq!(at("type.loadFonts"), at("type.resolveMissingFonts") + 1);
    assert_eq!(at("type.loadFontFolder"), at("type.loadFonts") + 1);
    assert_eq!(at("type.manageFonts"), at("type.loadFontFolder") + 1);
    for id in ["type.loadFonts", "type.loadFontFolder", "type.manageFonts"] {
        assert_eq!(items[at(id)].path, ["Type"]);
        assert!(items[at(id)].enabled, "{id}");
        // Usage statistics count the id; font names never travel (the commands take no names).
        assert_eq!(crate::telemetry::sanitize_command_id(id), id);
    }
}

#[test]
fn user_families_come_first_in_the_font_menu() {
    let fams: Vec<String> = ["Arial", "Mine B", "Inter", "mine a"].iter().map(|s| s.to_string()).collect();
    let mut rows: Vec<(&String, &str)> = fams.iter().map(|f| (f, f.as_str())).collect();
    let user: std::collections::HashSet<String> = ["mine a", "mine b"].iter().map(|s| s.to_string()).collect();
    assert_eq!(crate::type_tool::partition_user_rows(&mut rows, &user), 2);
    let order: Vec<&str> = rows.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(order, ["Mine B", "mine a", "Arial", "Inter"]);
}

/// The host's `fonts/manifest.txt` lists `family` (the web shell reads it at start).
fn serve(family: &str) {
    let (fonts, skipped) = crate::served_fonts::parse_manifest(&format!("{family} | Regular | fonts/x/{}.ttf\n", family.replace(' ', "")));
    assert!(skipped.is_empty());
    crate::served_fonts::add_fonts(&fonts);
}

#[test]
fn fonts_the_host_serves_are_fetched_not_reported_missing() {
    let family = "Served Mono DD";
    serve(family);
    let mut app = PhotocraftApp::new(Session::new(), Services::default());
    let layer = doc_with_text(&mut app, family);
    let before = ink(&app, layer);
    after_open(&mut app);
    assert!(notice(&app, "Missing fonts").is_none(), "every font is served: no notice");
    assert!(crate::served_fonts::take_requests().iter().any(|f| f == family), "and it is fetched");
    // The shell hands the file over: the layer is drawn with it.
    crate::served_fonts::install(&mut app, &egui::Context::default(), vec![(family.into(), mono(family))]);
    assert!(registered(family));
    assert!(photocraft_engine::type_extra_cmds::missing_fonts(&app.session.active().unwrap().doc).is_empty());
    assert!(ink(&app, layer).width() > before.width(), "re-rendered with the served font");
}

#[test]
fn the_missing_fonts_notice_drops_what_a_late_manifest_serves() {
    let (served, other) = ("Served Mono EE", "Missing Mono F");
    let mut app = PhotocraftApp::new(Session::new(), Services::default());
    doc_with_text(&mut app, served);
    app.run("type.create", json!({"x": 10, "y": 20, "text": "x", "size": 12, "font": other})).unwrap();
    crate::served_fonts::install(&mut app, &egui::Context::default(), Vec::new());
    after_open(&mut app);
    let mut lines = notice(&app, "Missing fonts").expect("before the manifest, both are missing").lines.clone();
    lines.sort();
    assert_eq!(lines, [other, served]);
    // The manifest arrives (next frame): the served family is fetched and leaves the notice.
    serve(served);
    crate::served_fonts::install(&mut app, &egui::Context::default(), Vec::new());
    assert_eq!(notice(&app, "Missing fonts").expect("still one missing").lines, [other]);
    assert!(crate::served_fonts::take_requests().iter().any(|f| f == served));
}
