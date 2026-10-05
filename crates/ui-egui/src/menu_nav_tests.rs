//! Menus on small displays (#138): every row of the longest menus is reachable by keyboard, wheel
//! and the scroll arrows, and the keyboard drives submenus and runs commands.

use super::*;
use crate::PhotocraftApp;
use egui_kittest::{Harness, kittest::Queryable};
use serde_json::json;

/// Window size in pixels and UI scale: the small displays of #138.
const DISPLAYS: [(f32, f32, f32); 4] = [(1280.0, 720.0, 1.0), (1024.0, 600.0, 1.0), (1280.0, 720.0, 1.5), (1024.0, 600.0, 1.5)];
/// The tallest menus.
const LONGEST: [&str; 4] = ["Filter", "Layer", "Image", "Edit"];

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
    app.sync_views();
    app
}

fn harness((w, h, scale): (f32, f32, f32)) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(w / scale, h / scale)).with_pixels_per_point(scale).build_ui_state(
        |ui, app| {
            crate::menus::menu_bar(app, ui);
        },
        app(),
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.ctx.all_styles_mut(|s| s.scroll_animation = egui::style::ScrollAnimation::none());
    h.run_steps(3);
    h
}

fn open(h: &mut Harness<'static, PhotocraftApp>, top: &str) {
    h.get_by_label(top).click();
    h.run_steps(4);
    assert!(Nav::current(&h.ctx).rows.first().is_some_and(|r| !r.is_empty()), "{top} did not open");
}

/// Is row `i` of `level` entirely inside its level's visible (scrolled) area, on screen?
fn visible(h: &Harness<'static, PhotocraftApp>, level: usize, i: usize) -> bool {
    let nav = Nav::current(&h.ctx);
    let (Some(row), Some(view)) = (nav.rows.get(level).and_then(|r| r.get(i)), nav.views.get(level)) else { return false };
    view.expand(0.5).contains_rect(row.rect) && h.ctx.content_rect().expand(0.5).contains_rect(*view)
}

fn rows(h: &Harness<'static, PhotocraftApp>, level: usize) -> Vec<Row> {
    Nav::current(&h.ctx).rows.get(level).cloned().unwrap_or_default()
}

fn key(h: &mut Harness<'static, PhotocraftApp>, k: egui::Key) {
    h.key_press(k);
    h.run_steps(3);
}

#[test]
fn every_row_of_the_longest_menus_is_reachable_with_the_arrow_keys() {
    for display in DISPLAYS {
        for top in LONGEST {
            let mut h = harness(display);
            open(&mut h, top);
            let all = rows(&h, 0);
            let enabled: Vec<usize> = all.iter().enumerate().filter(|(_, r)| r.enabled).map(|(i, _)| i).collect();
            assert!(enabled.len() > 5, "{top}: {} enabled rows", enabled.len());
            let mut seen = Vec::new();
            for _ in 0..enabled.len() {
                key(&mut h, egui::Key::ArrowDown);
                let i = Nav::current(&h.ctx).highlighted(0).expect("a highlighted row");
                assert!(
                    visible(&h, 0, i),
                    "{display:?} {top}: highlighted row {i} ({:?}) is scrolled out of view",
                    rows(&h, 0).get(i).map(|r| r.command.clone())
                );
                seen.push(i);
            }
            seen.sort_unstable();
            assert_eq!(seen, enabled, "{display:?} {top}: ↓ visits every enabled row once");
            // ↓ wraps to the first row (scrolling back up); ↑ wraps to the last.
            key(&mut h, egui::Key::ArrowDown);
            assert_eq!(Nav::current(&h.ctx).highlighted(0), enabled.first().copied());
            assert!(visible(&h, 0, enabled[0]));
            key(&mut h, egui::Key::ArrowUp);
            let last = *enabled.last().unwrap();
            assert_eq!(Nav::current(&h.ctx).highlighted(0), Some(last));
            assert!(visible(&h, 0, last), "{display:?} {top}: the last row is reachable");
        }
    }
}

#[test]
fn small_displays_overflow_and_the_menu_stays_in_the_window() {
    // 1024 × 600 at 150 %: 683 × 400 points, far shorter than the Filter menu.
    let mut h = harness((1024.0, 600.0, 1.5));
    open(&mut h, "Filter");
    let all = rows(&h, 0);
    let last = all.len() - 1;
    assert!(!visible(&h, 0, last), "the Filter menu must overflow at 683 × 400 pt for this test to mean anything");
    let nav = Nav::current(&h.ctx);
    let screen = h.ctx.content_rect();
    assert!(nav.views[0].bottom() <= screen.bottom() && nav.views[0].top() >= screen.top());
    let bar = h.get_by_label("Filter").rect();
    assert!(nav.views[0].top() >= bar.bottom(), "the menu hangs below the menu bar instead of sliding over it: {:?} vs {bar:?}", nav.views[0]);
    // Scroll arrows show when the rows overflow.
    assert!(h.query_by_label("Scroll menu down").is_some() && h.query_by_label("Scroll menu up").is_some());
}

#[test]
fn the_mouse_wheel_scrolls_a_long_menu() {
    for display in DISPLAYS {
        let mut h = harness(display);
        open(&mut h, "Filter");
        let last = rows(&h, 0).len() - 1;
        let view = Nav::current(&h.ctx).views[0];
        h.hover_at(view.center());
        h.run_steps(2);
        for _ in 0..60 {
            if visible(&h, 0, last) {
                break;
            }
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -60.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            h.run_steps(2);
        }
        assert!(visible(&h, 0, last), "{display:?}: the wheel reaches the last Filter row");
        assert!(rows(&h, 0).len() == last + 1, "the menu stays open while scrolling");
    }
}

#[test]
fn resting_on_the_scroll_arrows_scrolls() {
    let mut h = harness((1024.0, 600.0, 1.5));
    open(&mut h, "Filter");
    let last = rows(&h, 0).len() - 1;
    assert!(visible(&h, 0, 0) && !visible(&h, 0, last));
    let view = Nav::current(&h.ctx).views[0];
    h.hover_at(egui::pos2(view.center().x, view.bottom() + ARROW / 2.0));
    for _ in 0..240 {
        h.run_steps(1);
        if visible(&h, 0, last) {
            break;
        }
    }
    assert!(visible(&h, 0, last), "resting on ▼ scrolls to the end");
    assert!(!visible(&h, 0, 0));
    let view = Nav::current(&h.ctx).views[0];
    h.hover_at(egui::pos2(view.center().x, view.top() - ARROW / 2.0));
    for _ in 0..240 {
        h.run_steps(1);
        if visible(&h, 0, 0) {
            break;
        }
    }
    assert!(visible(&h, 0, 0), "resting on ▲ scrolls back to the top");
}

#[test]
fn arrows_open_and_close_submenus_and_enter_runs_a_command() {
    let mut h = harness((1024.0, 600.0, 1.5));
    open(&mut h, "Filter");
    // Walk down to the first submenu (Blur, Distort, …) and open it with →.
    for _ in 0..rows(&h, 0).len() {
        key(&mut h, egui::Key::ArrowDown);
        let nav = Nav::current(&h.ctx);
        if nav.highlighted(0).and_then(|i| nav.rows[0].get(i)).is_some_and(|r| r.command.is_none()) {
            break;
        }
    }
    key(&mut h, egui::Key::ArrowRight);
    h.run_steps(2);
    let nav = Nav::current(&h.ctx);
    assert_eq!(nav.level, 1, "→ moves into the submenu");
    assert!(nav.rows.get(1).is_some_and(|r| !r.is_empty()), "the submenu is open");
    let first = nav.rows[1].iter().position(|r| r.enabled);
    assert_eq!(nav.highlighted(1), first, "its first enabled row is highlighted");
    assert!(visible(&h, 1, first.unwrap()));
    key(&mut h, egui::Key::ArrowDown);
    assert_eq!(Nav::current(&h.ctx).level, 1);
    // ← closes it again.
    key(&mut h, egui::Key::ArrowLeft);
    h.run_steps(2);
    let nav = Nav::current(&h.ctx);
    assert_eq!((nav.level, nav.rows.len()), (0, 1), "← closes the submenu");
    // ↩ runs the highlighted command and closes the menu: Select › All.
    key(&mut h, egui::Key::Escape);
    open(&mut h, "Select");
    for _ in 0..rows(&h, 0).len() {
        let nav = Nav::current(&h.ctx);
        if nav.highlighted(0).and_then(|i| nav.rows[0].get(i)).is_some_and(|r| r.command.as_deref() == Some("select.all")) {
            break;
        }
        key(&mut h, egui::Key::ArrowDown);
    }
    assert!(h.state().session.active().unwrap().doc.selection.is_none());
    key(&mut h, egui::Key::Enter);
    assert!(h.state().session.active().unwrap().doc.selection.is_some(), "↩ ran Select › All");
    assert!(!is_open(&h.ctx), "and closed the menu");
}

#[test]
fn left_and_right_switch_menus_and_shortcuts_wait_while_one_is_open() {
    let mut h = harness((1280.0, 720.0, 1.0));
    assert!(!is_open(&h.ctx));
    open(&mut h, "File");
    assert!(is_open(&h.ctx), "shortcuts leave the keys to an open menu");
    key(&mut h, egui::Key::ArrowRight);
    h.run_steps(2);
    assert!(h.query_by_label_contains("Open…").is_none(), "→ leaves File");
    assert!(h.query_by_label_contains("Undo").is_some(), "→ opens Edit");
    key(&mut h, egui::Key::ArrowLeft);
    h.run_steps(2);
    assert!(h.query_by_label_contains("Open…").is_some(), "← back to File");
    key(&mut h, egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label_contains("Open…").is_none(), "Esc closes the menu");
    assert!(!is_open(&h.ctx), "shortcuts work again");
}
