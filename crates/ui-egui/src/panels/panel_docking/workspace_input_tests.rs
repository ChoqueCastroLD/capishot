//! A custom workspace draws the canvas inside its layout, and tools must work there as they do in
//! the default dock: every pointer move reaches them, clicks are recognised, and a theme switch
//! alone doesn't turn the session into a custom workspace.

use super::*;
use crate::state::Tool;
use egui::{Event, Modifiers, PointerButton, Pos2, ViewportId, pos2, vec2};
use egui_kittest::Harness;

type H = Harness<'static, PhotocraftApp>;

fn full_app(theme: crate::theme::ThemeKind, custom: bool) -> H {
    let mut h = Harness::builder().with_size(vec2(1400.0, 860.0)).with_pixels_per_point(1.0).with_max_steps(200).with_step_dt(1.0 / 60.0).build_eframe(
        move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, theme);
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            app.ui.theme = theme;
            app.run("file.new", json!({"width": 600, "height": 400})).unwrap();
            app.run("layer.new.layer", json!({})).unwrap();
            app.sync_views();
            if custom {
                app.run("window.panel.move", json!({"panel": "swatches", "anchor": "layers", "zone": "top"})).unwrap();
            }
            app
        },
    );
    h.run_steps(8);
    assert_eq!(h.state().ui.docking.is_some(), custom);
    h
}

fn press(h: &mut H, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    h.step();
}

/// Press, `moves` pointer moves a frame each, and the points the tool holds before the release.
fn drag_points(h: &mut H, tool: Tool, moves: usize) -> usize {
    h.state_mut().ui.tool = tool;
    h.run_steps(2);
    let c = h.state().last_canvas_rect.center();
    let at = |i: usize| c + vec2(-150.0 + 6.0 * i as f32, 40.0 * ((i as f32) * 0.5).sin());
    h.event(Event::PointerMoved(at(0)));
    h.step();
    press(h, at(0), true);
    for i in 1..=moves {
        h.event(Event::PointerMoved(at(i)));
        h.step();
    }
    let points = h.state().drag.as_ref().map_or(0, |d| d.points.len());
    press(h, at(moves), false);
    h.run_steps(2);
    points
}

#[test]
fn every_pointer_move_reaches_the_tool_in_a_custom_workspace() {
    for tool in [Tool::Brush, Tool::Lasso] {
        let default = drag_points(&mut full_app(Default::default(), false), tool, 45);
        let custom = drag_points(&mut full_app(Default::default(), true), tool, 45);
        assert!(default >= 45, "{tool:?}: {default} points in the default dock");
        assert_eq!(custom, default, "{tool:?}: points in a custom workspace");
    }
}

#[test]
fn a_marquee_selects_in_a_custom_workspace() {
    let mut h = full_app(Default::default(), true);
    drag_points(&mut h, Tool::RectMarquee, 12);
    assert!(h.state().session.active().unwrap().doc.selection.is_some(), "the drag made a selection");
}

/// A real click moves the pointer a little between press and release.
fn click_with_a_shake(h: &mut H, tool: Tool) {
    h.state_mut().ui.tool = tool;
    h.run_steps(2);
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.step();
    press(h, c, true);
    for offset in [1.0, 2.0, 1.0] {
        h.event(Event::PointerMoved(c + vec2(offset, 0.0)));
        h.step();
    }
    press(h, c + vec2(1.0, 0.0), false);
    h.run_steps(2);
}

#[test]
fn a_click_reaches_the_tools_in_a_custom_workspace() {
    let mut h = full_app(Default::default(), true);
    let zoom = h.state().ui.views[0].zoom;
    click_with_a_shake(&mut h, Tool::Zoom);
    assert_ne!(h.state().ui.views[0].zoom, zoom, "the Zoom tool's click zoomed");
    let downs = h.state().tool_downs;
    for _ in 0..2 {
        click_with_a_shake(&mut h, Tool::BubbleTextRemover);
    }
    assert!(h.state().tool_downs > downs, "the Bubble Text Remover had its click");
    assert!(h.state().ui.notices.iter().all(|n| n.tag.as_deref() != Some("toolDrop")), "no dropped-click notice");
}

/// Waits out what's animating, then every frame must be one pass and ask for no other.
fn assert_goes_idle(h: &mut H) {
    h.event(Event::PointerMoved(pos2(500.0, 400.0)));
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < 20 {
        let _ = h.try_run_realtime();
        h.step();
        if !h.output().viewport_output.get(&ViewportId::ROOT).is_some_and(|v| v.repaint_delay < std::time::Duration::from_secs(1)) {
            break;
        }
    }
    for _ in 0..10 {
        let passes = h.ctx.cumulative_pass_nr();
        h.step();
        assert_eq!(h.ctx.cumulative_pass_nr(), passes + 1, "one pass a frame, no discards");
        let delay = h.output().viewport_output.get(&ViewportId::ROOT).map(|v| v.repaint_delay);
        assert!(delay.is_none_or(|d| d >= std::time::Duration::from_secs(1)), "idle frame asked for another: {:?}", h.ctx.repaint_causes());
    }
}

#[test]
fn a_custom_workspace_is_idle_when_nothing_happens() {
    assert_goes_idle(&mut full_app(Default::default(), true));
}

/// Dragging the edge of the default dock makes a custom workspace: it must work as well.
#[test]
fn a_resized_dock_keeps_the_tools_working_and_goes_idle() {
    let mut h = full_app(Default::default(), false);
    let pro = h.state().ui.theme.is_pro();
    let edge = pos2(1400.0 - default_width(pro), 400.0);
    h.event(Event::PointerMoved(edge));
    h.step();
    press(&mut h, edge, true);
    for x in 1..=6 {
        h.event(Event::PointerMoved(edge - vec2(10.0 * x as f32, 0.0)));
        h.step();
    }
    press(&mut h, edge - vec2(60.0, 0.0), false);
    h.run_steps(4);
    assert!(h.state().ui.docking.is_some(), "the resize made a custom workspace");
    assert!(drag_points(&mut h, Tool::Brush, 45) >= 45, "every move reached the Brush");
    assert_goes_idle(&mut h);
}

#[test]
fn a_theme_switch_keeps_the_default_dock() {
    use crate::theme::ThemeKind::{Adwaita, ProLightGray, ProMedium, Studio};
    for (from, to) in [(ProLightGray, Adwaita), (ProMedium, Studio), (Studio, ProMedium), (Adwaita, ProLightGray)] {
        let mut h = full_app(from, false);
        let ctx = h.ctx.clone();
        h.state_mut().set_theme(&ctx, to);
        h.run_steps(6);
        assert!(h.state().ui.docking.is_none(), "{from:?} -> {to:?} made a custom workspace");
        assert_eq!(h.state().session.prefs().panel_layout["docking"], Value::Null, "{from:?} -> {to:?}");
    }
}

/// What 0.13.9 saved after a switch from a Pro theme to Adwaita (the column at Pro's 290).
fn saved_by_theme_switch() -> Value {
    let group = |panels: &[&str], active: usize| json!({"panels": panels, "active": active, "height": null, "minimized": false});
    json!({
        "docking": {
            "version": 2, "hidden": [], "locked": false, "saved": null,
            "layout": {
                "floating": [{"id": PROPERTIES_WINDOW, "active": 0, "panels": ["properties", "adjustments"], "rect": [783.0, 138.0, 320.0, 440.0]}],
                "root": {"Split": {
                    "axis": "Horizontal",
                    "size": {"FixedSecond": 300.0},
                    "first": {"Tabs": {"active": 0, "panels": ["canvas"]}},
                    "second": {"Dock": {"side": "End", "columns": [{
                        "iconic": false, "iconic_width": null, "width": 290.0,
                        "groups": [group(&["swatches", "color", "gradients", "patterns"], 1), group(&["layers", "channels", "paths"], 0)]
                    }]}}
                }}
            }
        },
        "dockingReturns": {},
        "dockingCollapsed": [],
        "dock": {"hidden_tabs": {"history": ["History", "Actions", "Layer Comps"], "navigator": ["Navigator", "Histogram", "Info"]}}
    })
}

#[test]
fn a_layout_only_a_theme_switch_made_is_dropped_on_load() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("window.panel.close", json!({"panel": "history"})).unwrap_or_default();
    crate::dock::apply(&mut app, &saved_by_theme_switch());
    assert!(app.ui.docking.is_none(), "back to the default dock");
    app.ui.panels.history = true;
    assert!(!app.ui.dock.visible_tabs(Group::History, true).is_empty(), "a group opened later has its tabs");
}

#[test]
fn a_layout_the_user_arranged_is_kept_on_load() {
    let edits: [(&str, Value); 5] = [
        ("/docking/layout/root/Split/second/Dock/columns/0/width", json!(350.0)),
        ("/docking/layout/floating/0/rect/2", json!(400.0)),
        ("/docking/layout/root/Split/second/Dock/columns/0/groups/0/height", json!(222.0)),
        ("/docking/layout/root/Split/second/Dock/columns/0/groups/1/panels/0", json!("history")),
        ("/docking/layout/floating/0/id", json!(77)),
    ];
    for (path, value) in edits {
        let mut saved = saved_by_theme_switch();
        *saved.pointer_mut(path).unwrap() = value;
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        crate::dock::apply(&mut app, &saved);
        assert!(app.ui.docking.is_some(), "{path} changed: the user's layout stays");
    }
}
