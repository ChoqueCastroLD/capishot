//! Phones (the simple interface at 360 × 780): every dialog a menu item opens, and the text
//! colour's Color Picker, keeps its OK and Cancel buttons inside the window; a tap outside, the
//! × and Esc (the phone's Back) close it; the tool options popover is a form that fits.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use crate::PhotocraftApp;

const W: f32 = 360.0;
const H: f32 = 780.0;

fn phone() -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(W, H)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
    });
    h.state_mut().run("file.new", json!({"width": 800, "height": 600})).unwrap();
    h.state_mut().run("layer.new", json!({})).ok();
    h.state_mut().run("select.all", json!({})).ok();
    h.state_mut().sync_views();
    h.run_steps(3);
    assert!(h.state().simple, "the simple interface on a phone");
    h
}

/// The labels the dialogs' confirm and dismiss buttons carry.
const OK: [&str; 7] = ["OK", "Create", "Delete", "Replace", "Export", "Apply", "Rasterize"];
const CANCEL: [&str; 2] = ["Cancel", "Close"];

fn on_screen(h: &Harness<'static, PhotocraftApp>, labels: &[&str]) -> Result<(), String> {
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(W, H));
    let mut seen = None;
    for l in labels {
        for n in h.query_all_by_label(l) {
            let r = n.rect();
            seen = Some(r);
            if window.contains_rect(r.shrink(0.5)) {
                return Ok(());
            }
        }
    }
    match seen {
        Some(r) => Err(format!("{} off the screen at {r:?}", labels[0])),
        None => Err(format!("no {} button", labels[0])),
    }
}

/// Menu items that would leave the test (quit, close the document, open the system) or that
/// show no dialog of their own.
fn skipped(id: &str) -> bool {
    id.starts_with("file.close") || id.starts_with("file.exit") || id == "file.quit" || id.starts_with("file.revert") || id.starts_with("help.") || id.starts_with("window.")
}

#[test]
fn every_dialog_keeps_ok_and_cancel_on_a_phone() {
    let mut h = phone();
    let ctx = h.ctx.clone();
    let ids: Vec<String> = crate::menus::menu_items(h.state()).into_iter().filter(|m| m.enabled && !skipped(&m.id)).map(|m| m.id).collect();
    let mut bad = Vec::new();
    let mut checked = 0;
    for id in ids {
        if h.state().session.active().is_none() {
            h.state_mut().run("file.new", json!({"width": 800, "height": 600})).unwrap();
        }
        h.state_mut().ui.dialogs.clear();
        let _ = crate::menus::invoke(h.state_mut(), &ctx, &id, json!({}));
        if h.state().ui.dialogs.is_empty() {
            continue;
        }
        h.run_steps(4);
        // Dialogs whose only button closes them (About, alerts) have no Cancel.
        let ok = on_screen(&h, &OK);
        let cancel = on_screen(&h, &CANCEL);
        if let (Err(a), Err(b)) = (&ok, &cancel) {
            bad.push(format!("{id}: {a} / {b}"));
        } else if let (Err(e), Ok(())) | (Ok(()), Err(e)) = (&ok, &cancel)
            && e.contains("off the screen")
        {
            bad.push(format!("{id}: {e}"));
        }
        checked += 1;
        h.state_mut().ui.dialogs.clear();
        h.run_steps(1);
    }
    assert!(checked >= 20, "only {checked} dialogs opened");
    assert!(bad.is_empty(), "{} of {checked} dialogs:\n{}", bad.len(), bad.join("\n"));
}

/// The text colour's Color Picker fits the phone, its OK and Cancel on the screen; a tap outside
/// it, and Esc, close it.
#[test]
fn the_text_color_picker_fits_a_phone_and_closes() {
    let mut h = phone();
    let id = h.state_mut().run("type.create", json!({"x": 100, "y": 200, "text": "Hi", "size": 40})).unwrap()["layer"].as_u64().unwrap();
    h.state_mut().session.select_layer(photocraft_doc::LayerId(id)).ok();
    h.state_mut().ui.tool = crate::state::Tool::Type;
    crate::type_tool::open_color_picker(h.state_mut(), [0.0, 0.0, 0.0]);
    h.run_steps(4);
    assert!(!h.state().ui.dialogs.is_empty());
    on_screen(&h, &["OK"]).unwrap();
    on_screen(&h, &["Cancel"]).unwrap();
    // Esc (Back) cancels.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().ui.dialogs.is_empty(), "Esc closes it");
    // A tap outside cancels too.
    crate::type_tool::open_color_picker(h.state_mut(), [0.0, 0.0, 0.0]);
    h.run_steps(4);
    let outside = egui::pos2(W / 2.0, H - 6.0);
    h.event(egui::Event::PointerMoved(outside));
    h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
    assert!(h.state().ui.dialogs.is_empty(), "a tap outside closes it (the picker at {:?})", h.query_by_label("OK").map(|n| n.rect()));
}

/// The tool options popover on a phone: a form inside the window for every tool, with an ×; the
/// Type tool's has its labels, no home button, and the alignment in one row.
#[test]
fn the_tool_options_popover_is_a_form_that_fits() {
    let mut h = phone();
    for tool in crate::state::Tool::ALL {
        h.state_mut().ui.tool = tool;
        h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("simple-ui-options-open"), true));
        h.run_steps(3);
        let pop = crate::simple_ui::noted_rect(&h.ctx, "options-popover");
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(W, H));
        let pop = pop.unwrap_or_else(|| panic!("{tool:?}: no popover"));
        assert!(window.contains_rect(pop.shrink(0.5)), "{tool:?}: {pop:?} leaves the window");
        if tool == crate::state::Tool::Type {
            for l in ["Font", "Size", "Alignment", "Color"] {
                h.get_by_label(l);
            }
            assert!(h.query_by_label("Home").is_none(), "no home button in the form");
            let ys: Vec<f32> = ["Left align text", "Center text", "Right align text"].iter().map(|l| h.get_by_label(l).rect().center().y).collect();
            assert!(ys.iter().all(|y| (y - ys[0]).abs() < 1.0), "alignment in one row: {ys:?}");
            // The × closes it.
            h.get_by_label("Close").click();
            h.run_steps(2);
            assert!(!crate::simple_ui::options_open(&h.ctx));
        }
        h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("simple-ui-options-open"), false));
        h.run_steps(1);
    }
}
