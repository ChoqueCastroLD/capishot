//! UI tests for the Color Lookup LUT browser: the whole app with a LUT library attached, a Color
//! Lookup layer selected, and an installed pack chosen through real pointer input.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_cms::lutfile::{LutFile, write_cube};
use photocraft_doc::{Adjustment, LayerContent};
use photocraft_engine::lut_library::LutLibrary;
use serde_json::json;

use crate::PhotocraftApp;

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("photocraft-lutui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn harness(library: LutLibrary) -> Harness<'static, PhotocraftApp> {
    harness_sized(library, 560.0)
}

fn harness_sized(library: LutLibrary, group_h: f32) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
        let layer = s.execute("layer.newAdjustmentLayer.colorLookup", json!({})).unwrap();
        // No LUT yet, so the dropdown reads "Load 3D LUT…".
        s.execute("layer.setAdjustment", json!({"layer": layer["layer"], "lut": "none"})).unwrap();
        s.lut_library = Some(library);
        let mut app = PhotocraftApp::new(s, crate::Services::default());
        app.ui.dock.heights.insert(crate::dock::Group::Properties, group_h);
        app
    });
    h.run_steps(8);
    h
}

fn lut_size(h: &Harness<'_, PhotocraftApp>) -> u32 {
    let st = h.state().session.active().unwrap();
    match &st.doc.layer(st.active_layer.unwrap()).unwrap().content {
        LayerContent::Adjustment(Adjustment::ColorLookup { size, .. }) => *size,
        other => panic!("{other:?}"),
    }
}

#[test]
fn installing_needs_a_library() {
    let h = harness(LutLibrary::new(temp("empty")));
    assert!(h.query_by_label("Install Pack…").is_some());
}

fn pack_source(name: &str) -> std::path::PathBuf {
    let src = temp(name);
    std::fs::write(src.join("Small.cube"), write_cube(&LutFile::identity(3))).unwrap();
    std::fs::write(src.join("Medium.cube"), write_cube(&LutFile::identity(5))).unwrap();
    std::fs::create_dir_all(src.join("Film")).unwrap();
    std::fs::write(src.join("Film").join("Large.cube"), write_cube(&LutFile::identity(7))).unwrap();
    src
}

#[test]
fn installed_luts_sit_in_the_list_by_pack_and_folder_with_plain_names() {
    let library = LutLibrary::new(temp("pick-lib"));
    library.install(&pack_source("pick-src"), Some("Packs"), true, &mut |_, _| true).unwrap();
    let mut h = harness(library);
    assert_ne!(lut_size(&h), 7);
    // Sections start closed: only the pack header shows.
    assert!(h.query_by_label("Small").is_none());
    h.get_by_label_contains("Packs (3)").click();
    h.run_steps(4);
    // File names only, no pack name beside them.
    assert!(h.query_by_label("Small").is_some());
    assert!(h.query_by_label_contains("(Packs)").is_none());
    h.get_by_label("Film").click();
    h.run_steps(4);
    h.get_by_label("Large").click();
    h.run_steps(4);
    assert_eq!(lut_size(&h), 7, "the chosen LUT's table is in the layer");
}

#[test]
fn up_and_down_step_through_the_luts() {
    let library = LutLibrary::new(temp("keys-lib"));
    library.install(&pack_source("keys-src"), Some("Packs"), true, &mut |_, _| true).unwrap();
    let mut h = harness(library);
    h.get_by_label_contains("Packs (3)").click();
    h.run_steps(4);
    h.get_by_label("Medium").click();
    h.run_steps(4);
    assert_eq!(lut_size(&h), 5);
    // Arrows move a cursor and only preview; Enter applies it.
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(4);
    assert_eq!(lut_size(&h), 5, "Down previews without changing the layer");
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(lut_size(&h), 3, "Enter applies the LUT Down moved to (Small)");
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(lut_size(&h), 5, "Up and Enter go back");
}

#[test]
fn the_list_grows_with_the_properties_group() {
    let visible_looks = |group_h: f32| {
        let h = harness_sized(LutLibrary::new(temp("height-lib")), group_h);
        photocraft_engine::adjust_cmds::LOOKS.iter().filter(|(_, label)| h.query_by_label(label).is_some()).count()
    };
    let (short, tall) = (visible_looks(300.0), visible_looks(760.0));
    assert!(short < tall, "a taller group shows more rows ({short} vs {tall})");
    assert!(tall >= 5, "a tall group shows most of the built-in section ({tall})");
}

#[test]
fn a_picked_lut_shows_up_under_recent_and_a_starred_one_under_favorites() {
    let library = LutLibrary::new(temp("recent-lib"));
    library.install(&pack_source("recent-src"), Some("Packs"), true, &mut |_, _| true).unwrap();
    let mut h = harness(library);
    assert!(h.query_by_label_contains("Recent").is_none());
    h.get_by_label_contains("Packs (3)").click();
    h.run_steps(4);
    h.get_by_label("Small").click();
    h.run_steps(4);
    assert!(h.query_by_label("Recent (1)").is_some(), "the LUT just applied is listed under Recent");
    h.state_mut().run("lut.favorite", json!({"id": "Packs/Medium.cube"})).unwrap();
    h.run_steps(4);
    assert!(h.query_by_label("Favorites (1)").is_some());
}

#[test]
fn a_folder_dropped_on_the_window_installs_as_a_pack() {
    let library = LutLibrary::new(temp("drop-lib"));
    let h = harness(library);
    let dir = pack_source("drop-src");
    assert!(crate::lut_library_ui::is_pack_drop(h.state(), &dir));
    assert!(!crate::lut_library_ui::is_pack_drop(h.state(), &dir.join("Small.cube")));
}
