use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_doc::{LayerContent, LayerId};
use photocraft_engine::Session;
use photocraft_engine::font_projects as fp;
use serde_json::json;

use crate::PhotocraftApp;

/// `source` with every UTF-16 occurrence of `from` in its names replaced by `to` (same length).
fn renamed(source: &[u8], from: &str, to: &str) -> Vec<u8> {
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

fn sans(name: &str) -> Vec<u8> {
    renamed(&photocraft_text::fonts::INTER_REGULAR, "Inter", name)
}

fn new_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en"}})).unwrap();
    app
}

fn type_layer(app: &mut PhotocraftApp) -> u64 {
    app.run("file.new", json!({"width": 400, "height": 300, "background": "white"})).unwrap();
    app.run("type.create", json!({"x": 40, "y": 100, "text": "HOLA", "size": 20})).unwrap()["layer"].as_u64().unwrap()
}

fn text_of(app: &PhotocraftApp, id: u64) -> photocraft_doc::TextLayer {
    match &app.session.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
        LayerContent::Text(t) => t.clone(),
        _ => panic!("a type layer"),
    }
}

/// Without projects the font menu is as before; with one, it starts with the project's fonts
/// (each with its role) and the recent ones, and a pick from the project applies the role.
#[test]
fn the_font_menu_shows_the_project_then_recent_then_all() {
    let mut app = new_app();
    let layer = type_layer(&mut app);
    let current = String::new();
    {
        let mut h = Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
            |ui, (app, cur): &mut (PhotocraftApp, String)| {
                assert!(super::menu_sections(app, ui, cur).is_none(), "no projects: the menu is as before");
            },
            (new_app(), current.clone()),
        );
        h.run();
    }
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    app.run("fontProjects.addFont", json!({"family": "JetBrains Mono", "role": "shout"})).unwrap();
    app.run("fontProjects.setRole", json!({"role": "shout", "settings": {"sizePt": 44}})).unwrap();
    fp::note_recent(&mut app.session, "Inter");
    let mut h = Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
        |ui, (app, cur): &mut (PhotocraftApp, String)| {
            if let Some(role) = super::menu_sections(app, ui, cur) {
                super::picked(app, cur, role.as_deref());
            }
        },
        (app, current),
    );
    h.run();
    h.get_by_label("Current project");
    h.get_by_label("Recent fonts");
    h.get_by_label("All fonts");
    h.get_by_label_contains("JetBrains Mono").click();
    h.run();
    let (app, current) = h.into_state();
    assert_eq!(current, "JetBrains Mono");
    assert_eq!(fp::Projects::of(&app.session).recent.first().map(String::as_str), Some("JetBrains Mono"), "remembered as recent");
    let _ = layer;
}

/// The role's settings go on the type target when its font is picked from the project.
#[test]
fn picking_a_project_font_applies_its_role() {
    let mut app = new_app();
    let layer = type_layer(&mut app);
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    app.run("fontProjects.addFont", json!({"family": "JetBrains Mono", "role": "shout"})).unwrap();
    app.run("fontProjects.setRole", json!({"role": "shout", "settings": {"sizePt": 44, "stroke": {"width": 3, "color": "#ffffff", "position": "outside"}}}))
        .unwrap();
    app.run("layer.select", json!({"id": layer})).ok();
    super::picked(&mut app, "JetBrains Mono", Some("shout"));
    let t = text_of(&app, layer);
    assert_eq!(t.char_runs()[0].style.size_pt, 44.0);
    // Off: only the font (set by the menu's caller).
    app.font_projects.keep_settings = false;
    app.run("type.setStyle", json!({"layer": layer, "size": 12})).unwrap();
    super::picked(&mut app, "JetBrains Mono", Some("shout"));
    assert_eq!(text_of(&app, layer).char_runs()[0].style.size_pt, 12.0);
}

/// A font's context menu adds it to the active project with a role.
#[test]
fn a_font_row_adds_to_the_project() {
    let mut app = new_app();
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    let ctx = egui::Context::default();
    super::offer_roles(&app, &ctx);
    let roles: Vec<(String, String)> = ctx.data(|d| d.get_temp(egui::Id::new("font-row-roles"))).unwrap();
    assert_eq!(roles.len(), 6);
    assert_eq!(roles[1].1, "Shout");
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("font-row-add"), ("Inter".to_string(), "narration".to_string())));
    super::take_add_request(&mut app, &ctx);
    let p = fp::Projects::of(&app.session);
    assert_eq!(p.active().unwrap().role_of("Inter"), Some("narration"));
}

/// Opening a page that carries a project (in its PSD) makes that project active.
#[test]
fn opening_a_page_activates_its_project() {
    let services = crate::Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
            photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(Session::new(), services);
    type_layer(&mut app);
    let id = app.run("fontProjects.create", json!({"name": "Kagura"})).unwrap()["id"].as_str().unwrap().to_string();
    app.run("fontProjects.activate", json!({"id": id})).unwrap();
    let psd = photocraft_io::export(&app.session.active().unwrap().doc, "p.psd", &photocraft_io::ExportOptions::default()).unwrap().bytes;
    app.run("fontProjects.activate", json!({"id": null, "remember": false})).unwrap();
    assert!(fp::Projects::of(&app.session).active.is_none());
    app.open_bytes("kagura_p01.psd", &psd).unwrap();
    for _ in 0..50 {
        crate::jobs_ui::tick(&mut app, &egui::Context::default());
        if app.session.documents().len() == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(fp::Projects::of(&app.session).active.as_deref(), Some(id.as_str()));
}

/// Bubble Type's styles take the font and settings of the roles the project maps them to.
#[test]
fn bubble_type_uses_the_projects_roles() {
    let mut app = new_app();
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    app.run("fontProjects.addFont", json!({"family": "JetBrains Mono", "role": "dialogue"})).unwrap();
    app.run("fontProjects.setRole", json!({"role": "dialogue", "settings": {"sizePt": 19, "caps": "all", "stroke": {"width": 2, "color": "#ffffff"}}}))
        .unwrap();
    let styles = app.ui.tool_options.bubble_styles.clone();
    let v = super::bubble_styles(&app, &styles);
    assert_eq!(v["bubble"]["font"], "JetBrains Mono", "Globo → Diálogo");
    assert_eq!(v["bubble"]["size"], 19.0);
    assert_eq!(v["bubble"]["caps"], "all");
    assert_eq!(v["bubble"]["stroke"]["width"], 2.0);
    assert_ne!(v["floating"]["font"], "JetBrains Mono", "Flotante → SFX, which has no font here");
    // On a page: floating text (no bubble) in an SFX-mapped... and a bubble style used for real.
    app.run("fontProjects.setMapping", json!({"bubble": {"floating": "dialogue"}})).unwrap();
    app.run("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
    let v = super::bubble_styles(&app, &styles);
    let r = app.run("type.createInBubble", json!({"x": 150, "y": 100, "style": "floating", "text": "Hey", "styles": v})).unwrap();
    let t = text_of(&app, r["layer"].as_u64().unwrap());
    assert_eq!(t.font_family, "JetBrains Mono");
}

/// A `.capiproj` opened installs its fonts among the user's (once, however often it's opened)
/// and adds the project.
#[test]
fn opening_a_capiproj_installs_its_fonts_once() {
    let font = sans("Prjqz");
    let mut p = fp::Project::new("prj", "Proyecto compartido");
    p.fonts = vec![fp::ProjectFont { family: "Prjqz".into(), role: "dialogue".into(), sha256: None }];
    let out = fp::export(
        &p,
        &[fp::FontFile { name: "Prjqz-Regular.ttf".into(), families: vec!["Prjqz".into()], bytes: Arc::new(font.clone()) }],
        &[],
        false,
        None,
        None,
    )
    .unwrap();
    let mut app = new_app();
    for _ in 0..2 {
        app.open_bytes("Proyecto.capiproj", &out.bytes).unwrap();
        crate::user_fonts_ui::tick(&mut app, None);
    }
    let sha = photocraft_text::sha256::sha256_hex(&font);
    let store = photocraft_text::user_fonts::store();
    assert_eq!(store.entries().iter().filter(|e| e.id == sha).count(), 1, "installed once");
    drop(store);
    let all = fp::Projects::of(&app.session);
    assert_eq!(all.projects.len(), 1, "re-opening updates the same project");
    assert_eq!(all.active().unwrap().name, "Proyecto compartido");
    assert!(app.ui.notices.iter().any(|n| n.title.contains("Proyecto compartido")));
}

/// Window › Font Projects…: new, a font, a role's settings and its preview, without a crash.
#[test]
fn the_projects_window_draws_and_creates() {
    let mut app = new_app();
    type_layer(&mut app);
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 800.0)).with_max_steps(32).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(2);
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, super::COMMAND, json!({})).unwrap();
    h.run_steps(3);
    h.get_by_label("New").click();
    h.run_steps(3);
    let all = fp::Projects::of(&h.state().session);
    assert_eq!(all.projects.len(), 1);
    h.get_by_label("Fonts by role");
    h.get_by_label("Bubble Type styles");
    h.get_by_label("TypeR tags");
}

/// A font held or right-clicked in the menu: the menu's top asks which role it joins as.
#[test]
fn a_held_font_is_offered_to_the_project_in_the_menu() {
    let mut app = new_app();
    app.run("fontProjects.create", json!({"name": "Serie"})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
        |ui, (app, cur): &mut (PhotocraftApp, String)| {
            super::menu_sections(app, ui, cur);
            super::take_add_request(app, ui.ctx());
        },
        (app, String::new()),
    );
    h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("font-row-adding"), "Inter".to_string()));
    h.run();
    h.get_by_label("Add Inter to the current project as…");
    h.get_by_label("Thought").click();
    h.run();
    h.run();
    let (app, _) = h.into_state();
    assert_eq!(fp::Projects::of(&app.session).active().unwrap().role_of("Inter"), Some("thought"));
}
