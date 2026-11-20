use egui_kittest::kittest::Queryable;
use photocraft_doc::Color;
use photocraft_engine::presets::gradients::GradientPreset;

use super::*;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.run("layer.layerStyle.gradientOverlay", json!({"from": "#ff0000", "to": "#0000ff", "opacityStops": [[0, 100], [1, 40]]})).unwrap();
    // A three-colour preset, as an imported .grd adds one to the Gradients panel.
    app.session.presets.gradients[0].items.push(GradientPreset::new("Tri", &["#ff0000", "#00ff00", "#0000ff"]));
    app
}

fn effect(app: &PhotocraftApp) -> &Effect {
    let state = app.session.active().unwrap();
    &state.doc.layer(state.active_layer.unwrap()).unwrap().effects.items[0]
}

fn fields(app: &PhotocraftApp) -> Map<String, Value> {
    let state = app.session.active().unwrap();
    initial_fields(state.doc.layer(state.active_layer.unwrap()).unwrap(), Some("gradientOverlay"), 120.0)
}

fn overlay_stops(e: &Effect) -> (Vec<(f32, Color)>, Vec<(f32, f32)>) {
    let Effect::GradientOverlay { gradient, interpolation, .. } = e else { panic!("overlay: {e:?}") };
    (interpolation.as_ref().map_or(gradient.stops.clone(), |s| s.stops.clone()), gradient.opacity_stops.clone())
}

fn tri() -> Vec<(f32, Color)> {
    vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.5, Color::rgb(0.0, 1.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))]
}

#[test]
fn picking_a_preset_sets_its_stops_previews_and_cancel_or_undo_restores() {
    let mut app = app();
    let original = effect(&app).clone();
    assert_eq!(overlay_stops(&original).0.len(), 2);
    let mut f = fields(&app);
    assert!(gradient::pick_preset(&app, &mut f, "fx1", "Tri"));
    assert_eq!(gradient::stops(&f, "fx1").unwrap().0, tri());
    // The live preview renders the picked gradient; the document is untouched until OK, so
    // Cancel (dropping the fields) restores the previous gradient.
    let doc = app.session.active().unwrap().doc.clone();
    let preview = preview_document(&doc, &app.session.patterns, &f).unwrap();
    let layer = preview.layer(app.session.active().unwrap().active_layer.unwrap()).unwrap();
    assert_eq!(overlay_stops(&layer.effects.items[0]).0, tri());
    assert_eq!(effect(&app), &original, "cancel leaves the overlay as it was");
    confirm(&mut app, &f).unwrap();
    let (stops, opacity) = overlay_stops(effect(&app));
    assert_eq!(stops, tri());
    assert_eq!(opacity, vec![(0.0, 1.0), (1.0, 0.4)], "the overlay keeps its opacity stops");
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(effect(&app), &original, "undo restores the previous gradient");
}

#[test]
fn from_and_to_edit_the_ends_without_flattening_and_reopening_keeps_three_stops() {
    let mut app = app();
    let mut f = fields(&app);
    gradient::pick_preset(&app, &mut f, "fx1", "Tri");
    set_param(&mut f, "fx1", "from", json!("#ffffff"));
    confirm(&mut app, &f).unwrap();
    let mut want = tri();
    want[0].1 = Color::rgb(1.0, 1.0, 1.0);
    assert_eq!(overlay_stops(effect(&app)).0, want);
    let saved = effect(&app).clone();
    // Reopened and confirmed unchanged: still three exact stops.
    let reopened = fields(&app);
    assert_eq!(gradient::stops(&reopened, "fx1").unwrap().0, want);
    confirm(&mut app, &reopened).unwrap();
    assert_eq!(effect(&app), &saved);
}

#[test]
fn the_stop_strip_and_stop_color_edit_the_overlay() {
    let mut app = app();
    let mut f = fields(&app);
    let fill = gradient::fill_for(&f, "fx1").unwrap();
    gradient::apply_edit(&mut f, "fx1", &fill, &json!({"action": "add", "location": 0.5}), ([0.0, 0.0, 0.0, 1.0], [1.0; 4]));
    assert_eq!(gradient::stops(&f, "fx1").unwrap().0.len(), 3);
    gradient::set_color_field(&mut f, "fx1", "stop:1", "#00ff00");
    let fill = gradient::fill_for(&f, "fx1").unwrap();
    gradient::apply_edit(&mut f, "fx1", &fill, &json!({"action": "move", "index": 1, "location": 0.25}), ([0.0, 0.0, 0.0, 1.0], [1.0; 4]));
    confirm(&mut app, &f).unwrap();
    let (stops, opacity) = overlay_stops(effect(&app));
    assert_eq!(stops.len(), 3);
    assert_eq!(stops[1], (0.25, Color::rgb(0.0, 1.0, 0.0)));
    assert_eq!(opacity, vec![(0.0, 1.0), (1.0, 0.4)]);
    // Bad stop fields are ignored, not a panic.
    gradient::set_color_field(&mut f, "fx1", "stop:99", "#123456");
    gradient::set_color_field(&mut f, "fx1", "stop:x", "#123456");
    assert!(gradient::stop_color(&f, "fx1", "stop:99").unwrap().is_err());
}

#[test]
fn the_gradient_swatch_opens_the_preset_picker() {
    let app = app();
    let f = fields(&app);
    let mut harness = egui_kittest::Harness::builder().with_size(vec2(1000.0, 1000.0)).build_ui_state(
        |ui, state: &mut (PhotocraftApp, Map<String, Value>)| {
            if ui.ctx().fonts(|fonts| fonts.families().contains(&egui::FontFamily::Name("semibold".into()))) {
                body(&mut state.0, ui, &mut state.1);
            }
        },
        (app, f),
    );
    PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
    harness.run_steps(3);
    harness.get_by_label("Gradient picker").click();
    harness.run_steps(3);
    assert_eq!(harness.state().1.get(gradient::PICKER), Some(&json!("fx1")));
    harness.get_by_label("Import Gradients…");
}
