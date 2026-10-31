//! The font menu's Custom tab and its folders, and the Sistema · Personalizadas · Favoritas tabs.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use crate::PhotocraftApp;
use crate::fonts_ui::PickerTab;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app
}

/// The Custom tab alone, `width` wide (under 600: a phone's), picks going to `picked`.
fn tab_harness(app: PhotocraftApp, width: f32) -> Harness<'static, (PhotocraftApp, Option<(String, Option<String>)>)> {
    let mut h = Harness::builder().with_size(egui::vec2(width, 700.0)).with_max_steps(8).build_ui_state(
        |ui, (app, picked): &mut (PhotocraftApp, Option<(String, Option<String>)>)| {
            if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                super::move_panel(app, ui);
                if let Some(p) = super::custom_tab(app, ui, "Inter") {
                    *picked = Some(p);
                }
            }
        },
        (app, None),
    );
    crate::PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(3);
    h
}

fn folders(app: &PhotocraftApp) -> Vec<(String, Option<String>, Vec<String>)> {
    app.session.prefs().type_.font_folders.iter().map(|f| (f.name.clone(), f.parent.clone(), f.fonts.clone())).collect()
}

fn filed() -> PhotocraftApp {
    let mut app = app();
    app.run("type.fontFolder.create", json!({"name": "Manga"})).unwrap();
    app.run("type.fontFolder.create", json!({"name": "SFX", "parent": "Manga"})).unwrap();
    app.run("type.fontFolder.move", json!({"family": "Bangers", "folder": "Manga"})).unwrap();
    app.run("type.fontFolder.move", json!({"family": "Komika", "folder": "SFX"})).unwrap();
    app
}

#[test]
fn the_custom_tab_lists_folders_subfolders_projects_and_unfiled() {
    let mut app = filed();
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    app.run("fontProjects.addFont", json!({"family": "Wild Words", "role": "dialogue"})).unwrap();
    let h = tab_harness(app, 1000.0);
    for label in ["Manga", "SFX", "Bangers", "Komika", "Serie (project)", "Wild Words", "Unfiled"] {
        assert!(h.query_all_by_label_contains(label).next().is_some(), "{label}");
    }
}

#[test]
fn a_row_picks_its_font_and_a_project_row_its_role() {
    let mut app = filed();
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    app.run("fontProjects.addFont", json!({"family": "Wild Words", "role": "dialogue"})).unwrap();
    let mut h = tab_harness(app, 1000.0);
    h.get_by_label("Bangers").click();
    h.run();
    assert_eq!(h.state().1, Some(("Bangers".into(), None)));
    h.get_by_label("Wild Words").click();
    h.run();
    assert_eq!(h.state().1, Some(("Wild Words".into(), Some("dialogue".into()))), "the active project's role");
}

#[test]
fn new_folder_rename_subfolder_and_delete() {
    let mut h = tab_harness(app(), 1000.0);
    h.get_by_label("New Folder…").click();
    h.run();
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Cómic");
    h.run();
    h.get_by_label("Create").click();
    h.run_steps(3);
    assert_eq!(folders(&h.state().0), [("Cómic".to_string(), None, vec![])]);
    // Its actions: a subfolder, a new name, and deleting it.
    h.get_by_label("Folder actions Cómic").click();
    h.run();
    h.get_by_label("New Subfolder…").click();
    h.run();
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Gritos");
    h.run();
    h.get_by_label("Create").click();
    h.run_steps(3);
    let f = folders(&h.state().0);
    assert_eq!(f.get(1), Some(&("Gritos".to_string(), Some("Cómic".to_string()), vec![])), "{f:?} status {}", h.state().0.ui.status);
    h.state_mut().0.run("type.fontFolder.move", json!({"family": "Bangers", "folder": "Cómic"})).unwrap();
    h.get_by_label("Folder actions Cómic").click();
    h.run();
    h.get_by_label("Rename…").click();
    h.run_steps(2);
    h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.event(egui::Event::Text("Manga".into()));
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(folders(&h.state().0)[0].0, "Manga");
    assert_eq!(folders(&h.state().0)[1].1.as_deref(), Some("Manga"), "the subfolder follows");
    h.get_by_label("Folder actions Manga").click();
    h.run();
    h.get_by_label("Delete Folder").click();
    h.run_steps(3);
    assert_eq!(folders(&h.state().0), [("Gritos".to_string(), None, vec![])], "its fonts stay (unfiled), its subfolder moves up");
}

#[test]
fn move_to_from_a_rows_menu_and_by_dragging() {
    let mut h = tab_harness(filed(), 1000.0);
    // Right-click (a phone: press and hold) → Move Bangers to… → SFX.
    let row = h.get_by_label("Bangers").rect();
    h.event(egui::Event::PointerMoved(row.center()));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: row.center(), button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
    }
    h.run_steps(2);
    assert!(h.query_by_label_contains("Move Bangers to").is_some());
    // The panel's SFX button (the first SFX node is the panel's: it is drawn above the list).
    h.query_all_by_label("↳ SFX").next().expect("the panel lists the subfolder").click();
    h.run_steps(2);
    assert_eq!(folders(&h.state().0)[1].2, ["Komika", "Bangers"]);
    assert!(h.query_by_label_contains("Move Bangers to").is_none(), "the panel closes");
    // A mouse drag from a row onto a folder's header files it there.
    let from = h.get_by_label("Komika").rect().center();
    let to = h.query_all_by_label_contains("Manga").next().unwrap().rect().center();
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    for k in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
        h.run();
    }
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
    let f = folders(&h.state().0);
    assert_eq!((f[0].2.as_slice(), f[1].2.as_slice()), (&["Komika".to_string()][..], &["Bangers".to_string()][..]), "dragged onto Manga");
}

#[test]
fn a_rows_star_toggles_the_favourite_without_picking() {
    let mut h = tab_harness(filed(), 1000.0);
    h.query_all_by_label("Add to Favorites").next().unwrap().click();
    h.run_steps(2);
    assert_eq!(photocraft_engine::font_library_cmds::favorite_fonts(&h.state().0.session).len(), 1);
    assert_eq!(h.state().1, None, "the star doesn't pick the font");
}

#[test]
fn phone_rows_are_48_points_tall() {
    let h = tab_harness(filed(), 390.0);
    let row = h.get_by_label("Bangers").rect();
    assert!((row.height() - super::PHONE_ROW).abs() < 0.5, "{row:?}");
    let desk = tab_harness(filed(), 1000.0);
    assert!(desk.get_by_label("Bangers").rect().height() < 30.0);
}

/// The whole font menu: the tabs filter the list while nothing is typed; a search spans them all.
#[test]
fn the_tabs_filter_the_list_and_a_search_spans_every_font() {
    let mut app = app();
    app.run("type.font.favorite", json!({"family": "Inter"})).unwrap();
    app.ui.fonts.tab = PickerTab::Favorites;
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).with_max_steps(8).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                let mut current = "Inter".to_string();
                let _ = crate::type_tool::font_picker(app, ui, &mut current, 200.0);
            }
        },
        app,
    );
    crate::PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(3);
    let families = crate::type_tool::families();
    let other = families.iter().find(|f| *f != "Inter").expect("more than one font").clone();
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(3);
    let rows = |h: &Harness<'_, PhotocraftApp>, f: &str| h.query_all_by_label(f).count();
    assert!(rows(&h, "Inter") > 0 && rows(&h, &other) == 0, "Favorites lists the starred fonts only");
    h.get_by_label("System").click();
    h.run_steps(3);
    assert!(rows(&h, &other) > 0, "System lists the installed fonts");
    h.get_by_label("Favorites").click();
    h.run_steps(2);
    let q: String = other.chars().take(4).collect();
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.event(egui::Event::Text(q.clone()));
    h.run_steps(3);
    assert!(rows(&h, &other) > 0, "a search lists every font, whatever the tab");
}

#[test]
fn a_press_held_on_a_row_asks_move_to_and_picks_nothing() {
    let mut h = tab_harness(filed(), 390.0);
    let at = h.get_by_label("Bangers").rect().center();
    h.event(egui::Event::PointerMoved(at));
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    for _ in 0..50 {
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
    assert!(h.query_by_label_contains("Move Bangers to").is_some(), "the hold asks where to move it");
    assert_eq!(h.state().1, None, "the release is no pick");
    // A tap afterwards picks as usual.
    h.query_all_by_label("Cancel").next().unwrap().click();
    h.run_steps(2);
    h.get_by_label("Bangers").click();
    h.run();
    assert_eq!(h.state().1, Some(("Bangers".into(), None)));
}

/// On phones (320–412 pt) every tab of the font menu fits in the window: nothing is cut at either
/// edge, the classes are chips above the list, and the rows are a finger's.
#[test]
fn every_tab_of_the_font_menu_fits_a_phone() {
    for width in [320.0f32, 360.0, 390.0, 412.0] {
        for tab in [PickerTab::Fonts, PickerTab::Custom, PickerTab::Favorites, PickerTab::FindMore] {
            let mut app = filed();
            app.run("type.font.favorite", json!({"family": "Inter"})).unwrap();
            app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
            app.run("fontProjects.addFont", json!({"family": "Inter", "role": "dialogue"})).unwrap();
            app.ui.fonts.tab = tab;
            let mut h = Harness::builder().with_size(egui::vec2(width, 800.0)).with_max_steps(8).build_ui_state(
                |ui, app: &mut PhotocraftApp| {
                    if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                        ui.add_space(500.0);
                        let mut current = "Inter".to_string();
                        let _ = crate::type_tool::font_picker(app, ui, &mut current, 200.0);
                    }
                },
                app,
            );
            crate::PhotocraftApp::setup_context(&h.ctx, Default::default());
            h.run_steps(3);
            h.get_by_role(egui::accesskit::Role::TextInput).click();
            h.run_steps(4);
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
            let areas: Vec<(egui::LayerId, egui::Rect)> = h.ctx.memory(|m| m.areas().visible_layer_ids().into_iter().filter_map(|l| Some((l, m.area_rect(l.id)?))).collect());
            assert!(areas.iter().any(|(l, _)| l.order == egui::Order::Foreground), "{tab:?} at {width}: the menu is open");
            for (l, r) in &areas {
                assert!(window.expand(0.5).contains_rect(*r), "{tab:?} at {width}: {l:?} {r:?} leaves the window");
            }
            // Every tab's label shows (the tabs wrap rather than run off).
            for label in ["System", "Custom", "Favorites", "Find More"] {
                let r = h.get_by_label(label).rect();
                assert!(window.contains_rect(r), "{tab:?} at {width}: tab {label} {r:?}");
            }
            if tab == PickerTab::Fonts {
                assert!(h.query_by_label("Serif").is_some_and(|n| n.rect().height() >= 36.0), "classes are chips at {width}");
                let row = h.query_all_by_label("JetBrains Mono").map(|n| n.rect()).find(|r| r.height() > 30.0);
                assert!(row.is_some_and(|r| r.height() >= 47.5 && window.contains_rect(r)), "{width}: a font row {row:?}");
            }
        }
    }
}
