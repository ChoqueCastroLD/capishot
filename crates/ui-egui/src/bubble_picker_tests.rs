//! Bubble Type's style picker: every source's styles, recent ones first, and what a click uses.

use egui::Modifiers;
use egui_kittest::kittest::Queryable;
use photocraft_doc::{LayerContent, LayerId};
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, tool_event};

/// A page with an oval bubble at (750, 350), the Bubble Type tool picked.
fn page() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app.run("file.new", json!({"width": 1200, "height": 800, "background": "white"})).unwrap();
    app.run("shape.create", json!({"kind": "ellipse", "rect": [500, 200, 500, 300], "fill": "#ffffff", "stroke": {"width": 4, "color": "#000000"}})).unwrap();
    app.ui.tool = crate::state::Tool::BubbleType;
    app
}

fn typer_style(app: &mut PhotocraftApp) -> String {
    let folder = app.run("typer.folder.save", json!({"folder": {"name": "Serie"}})).unwrap()["id"].clone();
    let mut props = photocraft_typer::library::default_text_props();
    props["layerText"]["textStyleRange"][0]["textStyle"]["fontName"] = json!("JetBrains Mono");
    props["layerText"]["textStyleRange"][0]["textStyle"]["size"] = json!(28);
    let r = app.run("typer.style.save", json!({"style": {"name": "Grito", "folder": folder, "textProps": props}})).unwrap();
    r["id"].as_str().map(str::to_string).unwrap_or_else(|| r["id"].to_string())
}

fn click(app: &mut PhotocraftApp, x: f64, y: f64) -> u64 {
    tool_event(app, ToolEvent::Down { x, y, pressure: 1.0 }, Modifiers::NONE);
    tool_event(app, ToolEvent::Up { x, y }, Modifiers::NONE);
    let id = app.ui.text_edit.as_ref().expect("typing").layer;
    crate::type_tool::insert(app, "Hey");
    crate::type_tool::commit(app);
    id
}

fn font_of(app: &PhotocraftApp, id: u64) -> String {
    match &app.session.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
        LayerContent::Text(t) => t.char_runs()[0].style.font_family.clone(),
        _ => panic!("a type layer"),
    }
}

/// Sections: Automatic, Bubble text, the project's roles, TypeR (folders, searchable); Recent once
/// something was picked.
#[test]
fn the_picker_lists_every_source() {
    let mut app = page();
    let titles = |app: &PhotocraftApp, q: &str| crate::bubble_type_ui::sections(app, q).into_iter().map(|s| s.title).collect::<Vec<_>>();
    assert_eq!(titles(&app, ""), ["Automatic", "Bubble Type"]);
    app.run("fontProjects.create", json!({"name": "Kagura"})).unwrap();
    let id = typer_style(&mut app);
    assert_eq!(titles(&app, ""), ["Automatic", "Bubble Type", "Project: Kagura", "TypeR"]);
    let secs = crate::bubble_type_ui::sections(&app, "");
    assert!(secs[2].items.iter().any(|(k, l)| k == "project:shout" && l == "Shout"));
    assert_eq!(secs[3].items, vec![(format!("typer:{id}"), "Serie / Grito".to_string())]);
    assert!(crate::bubble_type_ui::sections(&app, "nothing").last().unwrap().items.is_empty(), "search filters TypeR");
    crate::bubble_type_ui::pick(&mut app, &format!("typer:{id}"));
    crate::bubble_type_ui::pick(&mut app, "narration");
    let secs = crate::bubble_type_ui::sections(&app, "");
    assert_eq!(secs[0].title, "Recent");
    assert_eq!(secs[0].items.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), ["narration".to_string(), format!("typer:{id}")]);
    // The picker draws (desktop popup) and a row picks.
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(700.0, 700.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            crate::bubble_type_ui::picker_button(app, ui, 150.0);
            crate::bubble_type_ui::picker(app, &ui.ctx().clone());
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx.clone(), Default::default());
    h.run();
    h.get_by_label_contains("Narration text").click();
    h.run();
    assert!(crate::bubble_type_ui::picker_open(&h.ctx), "the button opens it");
    // Searched for, so the row is on screen.
    h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("bubble-picker-search"), "grito".to_string()));
    h.run();
    h.get_by_label("Serie / Grito").click();
    h.run();
    h.run();
    assert_eq!(h.state().ui.tool_options.bubble_pick, format!("typer:{id}"));
}

/// A TypeR style picked, then a project role picked: each click's text uses it.
#[test]
fn a_click_uses_the_picked_style() {
    let mut app = page();
    let id = typer_style(&mut app);
    crate::bubble_type_ui::pick(&mut app, &format!("typer:{id}"));
    let layer = click(&mut app, 750.0, 350.0);
    assert_eq!(font_of(&app, layer), "JetBrains Mono");
    assert_eq!(crate::bubble_type_ui::flashing(&app).map(|f| f.0), Some("Grito".into()), "its name by the cursor");
    app.run("fontProjects.create", json!({"name": "Kagura"})).unwrap();
    app.run("fontProjects.addFont", json!({"family": "Inter", "role": "thought"})).unwrap();
    app.run("fontProjects.setRole", json!({"role": "thought", "settings": {"sizePt": 21}})).unwrap();
    crate::bubble_type_ui::pick(&mut app, "project:thought");
    app.run("layer.select", json!({"id": 1})).ok();
    let layer = click(&mut app, 30.0, 760.0);
    let LayerContent::Text(t) = &app.session.active().unwrap().doc.layer(LayerId(layer)).unwrap().content else { panic!() };
    assert_eq!(t.char_runs()[0].style.size_pt, 21.0);
}

/// "Remember by shape" in Auto: bubbles map to a TypeR style (the tool's own mapping with no
/// project, the project's with one); a deleted style falls back to the built-in.
#[test]
fn auto_remembers_a_style_per_shape() {
    let mut app = page();
    let id = typer_style(&mut app);
    crate::bubble_type_ui::set_shape_style(&mut app, "bubble", &format!("typer:{id}"));
    assert_eq!(crate::bubble_type_ui::shape_style(&app, "bubble"), format!("typer:{id}"));
    let layer = click(&mut app, 750.0, 350.0);
    assert_eq!(font_of(&app, layer), "JetBrains Mono");
    app.run("fontProjects.create", json!({"name": "Kagura"})).unwrap();
    assert_eq!(crate::bubble_type_ui::shape_style(&app, "bubble"), "project:dialogue", "the project's own mapping");
    crate::bubble_type_ui::set_shape_style(&mut app, "bubble", &format!("typer:{id}"));
    assert_eq!(photocraft_engine::font_projects::active(&app.session).unwrap().bubble["bubble"], format!("typer:{id}"), "stored in the project");
    app.run("typer.style.delete", json!({"id": id})).unwrap();
    app.ui.tool_options.bubble_styles.bubble.font = "Inter".into();
    app.run("shape.create", json!({"kind": "ellipse", "rect": [60, 520, 340, 220], "fill": "#ffffff", "stroke": {"width": 4, "color": "#000000"}})).unwrap();
    let layer = click(&mut app, 230.0, 630.0);
    assert_eq!(font_of(&app, layer), "Inter", "the built-in bubble style");
    assert!(app.ui.notices.iter().any(|n| n.title.contains("is gone")), "said once");
}

/// On a phone (320 pt wide) the type bar has a Bubble style button (44 pt) that opens the picker
/// as a sheet whose rows are 44 pt tall.
#[test]
fn the_phone_type_bar_opens_the_picker_as_a_sheet() {
    let mut app = page();
    tool_event(&mut app, ToolEvent::Down { x: 750.0, y: 350.0, pressure: 1.0 }, Modifiers::NONE);
    tool_event(&mut app, ToolEvent::Up { x: 750.0, y: 350.0 }, Modifiers::NONE);
    assert!(app.ui.text_edit.is_some(), "typing in the bubble");
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(320.0, 640.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            // The app's fonts are set up after the harness's first frame and bound a frame later.
            let frames = egui::Id::new("frames-before-fonts");
            let n = ui.data(|d| d.get_temp::<u32>(frames).unwrap_or(0));
            if n < 2 {
                if n == 0 {
                    PhotocraftApp::setup_context(ui.ctx(), Default::default());
                }
                ui.data_mut(|d| d.insert_temp(frames, n + 1));
                ui.ctx().request_repaint();
                return;
            }
            ui.add_space(500.0);
            ui.horizontal(|ui| crate::type_tool::touch_bar(app, ui));
            crate::bubble_type_ui::picker(app, &ui.ctx().clone());
        },
        app,
    );
    h.run_steps(4);
    let b = crate::touch_ui::noted_rect(&h.ctx, "typeBubbleStyle").expect("the style button");
    assert!(b.width() >= 44.0 && b.height() >= 44.0 && b.right() <= 320.0, "{b:?}");
    h.get_by_label("Bubble style").click();
    h.run_steps(3);
    assert!(crate::bubble_type_ui::picker_open(&h.ctx));
    let row = crate::touch_ui::noted_rect(&h.ctx, "bubbleStyle:narration").expect("a row");
    assert!(row.height() >= 44.0 && row.width() > 250.0, "{row:?}");
    h.get_by_label("Narration text").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.tool_options.bubble_pick, "narration");
    assert!(!crate::bubble_type_ui::picker_open(&h.ctx), "a pick closes the sheet");
}
