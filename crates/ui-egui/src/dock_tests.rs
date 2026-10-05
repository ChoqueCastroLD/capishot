//! Dock layout tests (#88): fixed group heights, splitters, collapse, reorder, persistence.

use egui::{Modifiers, PointerButton, Pos2, Rect, vec2};
use egui_kittest::Harness;
use serde_json::json;

use super::*;
use crate::theme::ThemeKind;

const ESSENTIALS: [Group; 3] = [Group::Color, Group::Properties, Group::Layers];

#[test]
fn last_expanded_group_fills_the_column() {
    let l = DockLayout::default();
    let hs = l.heights_for(&ESSENTIALS, 800.0, 28.0);
    assert_eq!(hs[0], (Group::Color, Group::Color.default_height()));
    assert_eq!(hs[1], (Group::Properties, Group::Properties.default_height()));
    let total: f32 = hs.iter().map(|(_, h)| h).sum::<f32>() + 2.0 * GAP;
    assert!((total - 800.0).abs() < 1e-3, "{hs:?}");
    // Collapsed groups shrink to the tab strip; the one above Layers keeps its height.
    let mut l = DockLayout::default();
    l.set_collapsed(Group::Color, true);
    let hs = l.heights_for(&ESSENTIALS, 800.0, 28.0);
    assert_eq!(hs[0].1, 28.0);
    assert_eq!(hs[1].1, Group::Properties.default_height());
    // Layers collapsed: Properties becomes the filler.
    l.set_collapsed(Group::Layers, true);
    let hs = l.heights_for(&ESSENTIALS, 800.0, 28.0);
    assert_eq!(hs[2].1, 28.0);
    assert!((hs[1].1 - (800.0 - 56.0 - 2.0 * GAP)).abs() < 1e-3);
}

#[test]
fn short_columns_squeeze_groups_down_to_their_minimum_and_never_go_negative() {
    let l = DockLayout::default();
    let hs = l.heights_for(&ESSENTIALS, 500.0, 28.0);
    assert_eq!(hs[2].1, Group::Layers.min_height(), "Layers keeps its minimum: {hs:?}");
    assert_eq!(hs[0].1, Group::Color.default_height(), "the group farthest from Layers gives way last");
    assert!(hs[1].1 < Group::Properties.default_height());
    for avail in [0.0, -50.0, 1.0, f32::NAN, f32::INFINITY, 1e9] {
        for (g, h) in l.heights_for(&ESSENTIALS, avail, 28.0) {
            assert!(h.is_finite() && h >= 0.0, "{g:?} at {avail}: {h}");
        }
    }
    assert!(l.heights_for(&[], 500.0, 28.0).is_empty());
}

#[test]
fn bad_stored_values_are_sanitised() {
    let mut l = DockLayout { order: vec![Group::Layers, Group::Layers, Group::Color], ..Default::default() };
    assert_eq!(l.order(), vec![Group::Layers, Group::Color, Group::Properties, Group::Navigator, Group::History]);
    for bad in [f32::NAN, -10.0, f32::INFINITY, 1e12] {
        l.heights.insert(Group::Color, bad);
        let h = l.height(Group::Color);
        assert!(h.is_finite() && h >= Group::Color.min_height() && h <= MAX_HEIGHT, "{bad} -> {h}");
    }
    // Unknown fields, wrong types and old UI state all load.
    let back: DockLayout = serde_json::from_value(json!({"order": ["layers"], "bogus": 1})).unwrap();
    assert_eq!(back.order(), vec![Group::Layers, Group::Color, Group::Properties, Group::Navigator, Group::History]);
    let mut ui = serde_json::to_value(crate::state::UiState::default()).unwrap();
    ui.as_object_mut().unwrap().remove("dock");
    let ui: crate::state::UiState = serde_json::from_value(ui).unwrap();
    assert_eq!(ui.dock, DockLayout::default());
}

#[test]
fn move_group_reorders() {
    let mut l = DockLayout::default();
    l.move_group(Group::Layers, Some(Group::Color));
    assert_eq!(l.order()[0], Group::Layers);
    l.move_group(Group::Layers, None);
    assert_eq!(l.order().last(), Some(&Group::Layers));
    l.move_group(Group::Color, Some(Group::Color));
    assert_eq!(l.order()[0], Group::Color);
}

fn app_with_layers() -> (PhotocraftApp, photocraft_doc::LayerId, photocraft_doc::LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 200, "height": 150})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    let pixel = app.session.active().unwrap().active_layer.unwrap();
    app.run("layer.newAdjustmentLayer.curves", json!({})).unwrap();
    let adj = app.session.active().unwrap().active_layer.unwrap();
    assert_ne!(pixel, adj);
    app.sync_views();
    (app, pixel, adj)
}

fn harness(app: PhotocraftApp, size: egui::Vec2, theme: ThemeKind) -> Harness<'static, PhotocraftApp> {
    // 60 fps steps, so two clicks a frame apart count as a double-click.
    let mut h = Harness::builder().with_size(size).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::panels::right_dock(app, ui);
            egui::CentralPanel::default().show(ui, |_| {});
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, theme);
    h.state_mut().ui.theme = theme;
    h.run_steps(4);
    h
}

fn rect_of(h: &Harness<'static, PhotocraftApp>, g: Group) -> Rect {
    last_rects(&h.ctx).into_iter().find(|(x, _)| *x == g).map(|(_, r)| r).unwrap_or_else(|| panic!("{g:?} not drawn"))
}

fn drag(h: &mut Harness<'static, PhotocraftApp>, from: Pos2, to: Pos2) {
    h.event(egui::Event::PointerMoved(from));
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=6 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn switching_layer_kinds_keeps_the_layers_panel_still() {
    for theme in [ThemeKind::ProMedium, ThemeKind::Studio] {
        let (app, pixel, adj) = app_with_layers();
        let mut h = harness(app, vec2(1200.0, 800.0), theme);
        let rects = |h: &Harness<'static, PhotocraftApp>| last_rects(&h.ctx);
        let with_adj = rects(&h);
        assert!(with_adj.iter().any(|(g, _)| *g == Group::Layers));
        h.state_mut().run("layer.select", json!({"layer": pixel.0})).unwrap();
        h.run_steps(4);
        assert_eq!(rects(&h), with_adj, "{theme:?}: selecting a pixel layer moved the dock groups");
        h.state_mut().run("layer.select", json!({"layer": adj.0})).unwrap();
        h.run_steps(4);
        assert_eq!(rects(&h), with_adj, "{theme:?}: selecting an adjustment layer moved the dock groups");
        // Properties ↔ Adjustments tabs don't move anything either.
        h.state_mut().ui.dock_tabs.properties = 1;
        h.run_steps(3);
        assert_eq!(rects(&h), with_adj);
    }
}

#[test]
fn dragging_the_splitter_resizes_and_survives_a_ui_state_round_trip() {
    let (app, _, _) = app_with_layers();
    let mut h = harness(app, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    let props = rect_of(&h, Group::Properties);
    let layers = rect_of(&h, Group::Layers);
    let split = Pos2::new(props.center().x, props.bottom() + GAP / 2.0);
    drag(&mut h, split, split - vec2(0.0, 90.0));
    let props2 = rect_of(&h, Group::Properties);
    let layers2 = rect_of(&h, Group::Layers);
    assert!((props2.height() - (props.height() - 90.0)).abs() < 2.0, "{props:?} -> {props2:?}");
    assert!((layers2.top() - (layers.top() - 90.0)).abs() < 2.0, "{layers:?} -> {layers2:?}");
    assert_eq!(layers2.bottom(), layers.bottom());
    let stored = h.state().ui.dock.heights.get(&Group::Properties).copied().unwrap();
    // Dragging past the minimum stops at it.
    let split = Pos2::new(props2.center().x, props2.bottom() + GAP / 2.0);
    drag(&mut h, split, split - vec2(0.0, 600.0));
    assert_eq!(rect_of(&h, Group::Properties).height(), Group::Properties.min_height());
    let from = Pos2::new(split.x, rect_of(&h, Group::Properties).bottom() + GAP / 2.0);
    drag(&mut h, from, split);
    assert!((h.state().ui.dock.heights[&Group::Properties] - stored).abs() < 2.0);

    // Save and restore the UI state (what `ui.inspect` / `ui.set` and workspaces carry).
    let saved = serde_json::to_value(&h.state().ui).unwrap();
    let (mut app2, _, _) = app_with_layers();
    app2.ui = serde_json::from_value(saved).unwrap();
    let h2 = harness(app2, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    assert_eq!(last_rects(&h2.ctx), last_rects(&h.ctx));

    // Remembered in the preferences once the mouse is up, and restored at the next launch.
    let prefs = h.state().session.prefs_to_json();
    let mut s2 = photocraft_engine::Session::new();
    s2.load_prefs_json(&prefs).unwrap();
    let mut app3 = PhotocraftApp::new(s2, crate::Services::default());
    restore(&mut app3);
    assert_eq!(app3.ui.dock, h.state().ui.dock);
    // …unless Remember Workspace Changes is off.
    let mut s3 = photocraft_engine::Session::new();
    s3.load_prefs_json(&prefs).unwrap();
    s3.prefs.edit(|p| p.workspace.remember_workspace_changes = false);
    let mut app4 = PhotocraftApp::new(s3, crate::Services::default());
    restore(&mut app4);
    assert_eq!(app4.ui.dock, DockLayout::default());
}

#[test]
fn reset_workspace_restores_the_default_layout_and_new_workspaces_keep_theirs() {
    let (app, _, _) = app_with_layers();
    let mut h = harness(app, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    let ctx = h.ctx.clone();
    let default = last_rects(&h.ctx);
    h.state_mut().ui.dock.heights.insert(Group::Properties, 120.0);
    h.state_mut().ui.dock.set_collapsed(Group::Color, true);
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.newWorkspace", json!({"name": "Tall Layers"})).unwrap();
    let mine = h.state().ui.dock.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.essentials", json!({})).unwrap();
    assert_eq!(h.state().ui.dock, DockLayout::default());
    h.state_mut().ui.dock.heights.insert(Group::Color, 300.0);
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.resetWorkspace", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(last_rects(&h.ctx), default);
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.select", json!({"name": "Tall Layers"})).unwrap();
    assert_eq!(h.state().ui.dock, mine);
}

#[test]
fn double_clicking_a_tab_collapses_and_dragging_a_strip_reorders() {
    let (app, _, _) = app_with_layers();
    let mut h = harness(app, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    let color = rect_of(&h, Group::Color);
    let tab = color.left_top() + vec2(20.0, 13.0);
    h.event(egui::Event::PointerMoved(tab));
    h.run_steps(1);
    for _ in 0..2 {
        h.event(egui::Event::PointerButton { pos: tab, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerButton { pos: tab, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(2);
    assert!(h.state().ui.dock.is_collapsed(Group::Color));
    assert!(rect_of(&h, Group::Color).height() < 40.0);
    h.state_mut().ui.dock.set_collapsed(Group::Color, false);
    h.run_steps(3);
    // Drag the Layers tab strip (right of its tabs) above Color.
    let layers = rect_of(&h, Group::Layers);
    let strip = Pos2::new(layers.right() - 60.0, layers.top() + 13.0);
    let to = rect_of(&h, Group::Color).left_top() + vec2(120.0, 10.0);
    drag(&mut h, strip, to);
    assert_eq!(h.state().ui.dock.order().first(), Some(&Group::Layers));
    // A locked workspace keeps the order.
    h.state_mut().session.prefs.edit(|p| p.workspace_locked = true);
    let layers = rect_of(&h, Group::Layers);
    drag(&mut h, Pos2::new(layers.right() - 60.0, layers.top() + 13.0), Pos2::new(layers.right() - 60.0, 790.0));
    assert_eq!(h.state().ui.dock.order().first(), Some(&Group::Layers));
}

#[test]
fn tiny_windows_do_not_panic() {
    for theme in [ThemeKind::ProMedium, ThemeKind::Studio] {
        for (w, ht) in [(40.0, 30.0), (300.0, 80.0), (600.0, 200.0), (2000.0, 120.0)] {
            let (mut app, _, _) = app_with_layers();
            app.ui.panels.history = true;
            app.ui.panels.navigator = true;
            let mut h = harness(app, vec2(w, ht), theme);
            for (_, r) in last_rects(&h.ctx) {
                assert!(r.height() >= 0.0 && r.is_finite());
            }
            // Drag a splitter way off-screen.
            if let Some((_, r)) = last_rects(&h.ctx).first().copied() {
                let p = Pos2::new(r.center().x, r.bottom() + GAP / 2.0);
                drag(&mut h, p, p + vec2(0.0, 5000.0));
                drag(&mut h, p, p - vec2(0.0, 5000.0));
            }
        }
    }
}

#[test]
fn the_rail_and_window_menu_never_lose_a_panel() {
    let (mut app, _, _) = app_with_layers();
    let ctx = egui::Context::default();
    // The rail collapses and expands a docked group; it never hides it (#129).
    rail_click(&mut app, Group::Layers, true);
    assert!(app.ui.panels.layers && app.ui.dock.is_collapsed(Group::Layers));
    rail_click(&mut app, Group::Layers, true);
    assert!(app.ui.panels.layers && !app.ui.dock.is_collapsed(Group::Layers));
    // Window › Layers on a collapsed group expands it instead of hiding it.
    app.ui.dock.set_collapsed(Group::Layers, true);
    crate::menus::invoke(&mut app, &ctx, "window.panel.layers", json!({})).unwrap();
    assert!(app.ui.panels.layers && !app.ui.dock.is_collapsed(Group::Layers));
    app.ui.dock.set_collapsed(Group::Color, true);
    crate::menus::invoke(&mut app, &ctx, "window.toggle.color", json!({})).unwrap();
    assert!(app.ui.panels.color && !app.ui.dock.is_collapsed(Group::Color));
    app.ui.dock.set_collapsed(Group::Color, true);
    crate::menus::invoke(&mut app, &ctx, "window.panel.gradients", json!({})).unwrap();
    assert!(app.ui.panels.color && !app.ui.dock.is_collapsed(Group::Color) && app.ui.dock_tabs.color == 2);
    // Hidden panels come back from the Window menu, expanded, on their tab.
    app.ui.panels.history = false;
    app.ui.dock.set_collapsed(Group::History, true);
    crate::menus::invoke(&mut app, &ctx, "window.panel.actions", json!({})).unwrap();
    assert!(app.ui.panels.history && !app.ui.dock.is_collapsed(Group::History) && app.ui.dock_tabs.history == 1);
    // Reset Workspace brings back everything the preset shows.
    app.ui.panels = crate::state::Panels { layers: false, properties: false, color: false, ..Default::default() };
    app.ui.dock_tabs.layers = 2;
    app.ui.dock.set_collapsed(Group::Properties, true);
    crate::menus::invoke(&mut app, &ctx, "window.workspace.resetWorkspace", json!({})).unwrap();
    assert!(app.ui.panels.layers && app.ui.panels.properties && app.ui.panels.color);
    assert_eq!(app.ui.dock, DockLayout::default());
    assert_eq!(app.ui.dock_tabs.layers, 0);
}

/// #129: clicking around the whole UI (layers, canvas, tools, options bar) never switches,
/// hides or moves a dock panel.
#[test]
fn clicking_around_the_ui_keeps_the_panels_put() {
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let (app, _, _) = app_with_layers();
        app
    });
    h.run_steps(8);
    let panels = h.state().ui.panels.clone();
    let tabs = h.state().ui.dock_tabs;
    let rects = last_rects(&h.ctx);
    assert!(rects.iter().any(|(g, _)| *g == Group::Layers), "{rects:?}");
    let layers = rect_of(&h, Group::Layers);
    let canvas = h.state().last_canvas_rect;
    let mut points: Vec<Pos2> = Vec::new();
    // Layer rows (eyes, thumbnails, names) and the Layers footer.
    for dy in [118.0, 140.0, 150.0, 170.0, 182.0, 200.0] {
        for x in [layers.left() + 22.0, layers.left() + 50.0, layers.center().x] {
            points.push(Pos2::new(x, layers.top() + dy));
        }
    }
    // The canvas, the toolbar column and the options bar.
    points.extend([canvas.center(), canvas.left_top() + vec2(40.0, 40.0), canvas.right_bottom() - vec2(30.0, 30.0)]);
    for y in (110..460).step_by(33) {
        points.push(Pos2::new(20.0, y as f32));
    }
    for x in (40..1100).step_by(70) {
        points.push(Pos2::new(x as f32, 50.0));
    }
    for p in points {
        h.hover_at(p);
        h.run_steps(1);
        h.drag_at(p);
        h.run_steps(1);
        h.drop_at(p);
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        // Popups and dialogs opened by a click are fine; panels changing aren't.
        let app = h.state();
        assert_eq!(app.ui.panels.layers, panels.layers, "click at {p:?} hid/showed Layers");
        assert_eq!(app.ui.panels.properties, panels.properties, "click at {p:?} hid/showed Properties");
        assert_eq!(app.ui.panels.color, panels.color, "click at {p:?} hid/showed Color");
        assert_eq!(app.ui.dock_tabs, tabs, "click at {p:?} switched a dock tab");
        assert!(app.ui.dock.collapsed.is_empty(), "click at {p:?} collapsed a group");
        if app.ui.dialogs.is_empty() && app.ui.shell.dialog.is_none() {
            assert_eq!(last_rects(&h.ctx), rects, "click at {p:?} moved the dock groups");
        }
        h.state_mut().ui.dialogs.clear();
    }
}
