//! The phone's menus in the real app: one Menu button, a bottom sheet of pages, one level at a
//! time, greyed items that stay listed and do nothing, checks, dynamic lists, every way to close.

use egui::{Event, Pos2, Rect, TouchDeviceId, TouchId, TouchPhase, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::menus::mobile_menu;
use crate::menus::{TOP_MENUS, menu_items};
use crate::mobile_sheet;
use crate::simple_ui::noted_rect;

type H = Harness<'static, PhotocraftApp>;

const W: f32 = 360.0;
const HEIGHT: f32 = 780.0;

fn phone() -> H {
    phone_with(|_| {})
}

fn phone_with(setup: impl FnOnce(&mut PhotocraftApp)) -> H {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    // No slides: rows are where they stay.
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "on", "interface.enableAnimations": false}})).unwrap();
    app.run("file.new", json!({"width": 600, "height": 800, "background": "white"})).unwrap();
    setup(&mut app);
    app.sync_views();
    let mut h = Harness::builder().with_size(vec2(W, HEIGHT)).with_max_steps(64).with_step_dt(1.0 / 30.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    h
}

fn frame(h: &mut H, events: Vec<Event>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    raw.events.extend(events);
    let t = ctx.input(|i| i.time) + 1.0 / 30.0;
    raw.time = Some(t);
    crate::touch_ui::route(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn touch(phase: TouchPhase, p: Pos2) -> Event {
    Event::Touch { device_id: TouchDeviceId(1), id: TouchId(7), phase, pos: p, force: None }
}

fn tap_at(h: &mut H, p: Pos2) {
    frame(h, vec![touch(TouchPhase::Start, p)]);
    frame(h, vec![touch(TouchPhase::End, p)]);
    h.run_steps(8);
}

fn rect(h: &H, label: &str) -> Rect {
    noted_rect(&h.ctx, label).unwrap_or_else(|| panic!("no {label}"))
}

fn tap(h: &mut H, label: &str) {
    let at = rect(h, label).center();
    tap_at(h, at);
}

fn open_menu(h: &mut H) {
    tap(h, "menu:open");
    assert!(mobile_menu::is_open(&h.ctx), "the Menu button opens the sheet");
}

fn path(h: &H) -> Vec<String> {
    serde_json::from_value(mobile_menu::inspect(&h.ctx)["path"].clone()).unwrap_or_default()
}

#[test]
fn menu_opens_a_full_width_sheet_listing_every_menu() {
    let mut h = phone();
    assert!(!mobile_sheet::any_open(&h.ctx));
    assert!(h.state().menu_cache.is_none(), "closed, the items aren't built");
    open_menu(&mut h);
    assert!(h.state().menu_cache.is_some(), "open, they are kept between frames");
    let handle = rect(&h, "sheet:handle");
    assert!(handle.width() >= W - 0.5 && handle.left().abs() < 0.5, "full width: {handle:?}");
    for top in TOP_MENUS {
        let row = rect(&h, &format!("sheet:sub:{top}"));
        assert!(row.height() >= 48.0, "{top}: {row:?}");
        assert!(row.top() >= handle.bottom() && row.bottom() <= HEIGHT, "{top} on screen: {row:?}");
    }
    assert!(noted_rect(&h.ctx, "sheet:search").is_some(), "the first page has the search");
    assert!(noted_rect(&h.ctx, "sheet:back").is_none(), "and no way further back");
}

#[test]
fn submenus_are_pages_with_back_and_one_level_shows_at_a_time() {
    let mut h = phone();
    open_menu(&mut h);
    tap(&mut h, "sheet:sub:File");
    assert_eq!(path(&h), ["File"]);
    assert!(noted_rect(&h.ctx, "sheet:sub:Edit").is_none(), "the first page is gone");
    assert!(noted_rect(&h.ctx, "sheet:item:file.new").is_some());
    tap(&mut h, "sheet:sub:Export");
    assert_eq!(path(&h), ["File", "Export"]);
    assert!(noted_rect(&h.ctx, "sheet:item:file.new").is_none(), "File's rows are gone");
    assert!(noted_rect(&h.ctx, "sheet:item:file.export.exportAs").is_some());
    tap(&mut h, "sheet:back");
    assert_eq!(path(&h), ["File"]);
    tap(&mut h, "sheet:back");
    assert!(path(&h).is_empty());
    assert!(noted_rect(&h.ctx, "sheet:sub:Help").is_some());
    assert!(mobile_menu::is_open(&h.ctx), "Back never closes from a page");
}

#[test]
fn a_disabled_row_is_listed_and_does_nothing_an_enabled_one_runs_and_closes() {
    let mut h = phone();
    // Nothing selected: Select has greyed items near its top.
    let items = menu_items(h.state());
    let off = items.iter().find(|i| i.path == ["Select"] && !i.enabled && i.label != "---").expect("a greyed item in Select");
    let row = format!("sheet:item:{}", off.id);
    open_menu(&mut h);
    tap(&mut h, "sheet:sub:Select");
    let at = rect(&h, &row);
    assert!(at.bottom() < HEIGHT, "{row} is on screen: {at:?}");
    let revision = h.state().session.active().unwrap().doc.name.clone();
    tap(&mut h, &row);
    assert!(mobile_menu::is_open(&h.ctx), "the sheet stays");
    assert_eq!(path(&h), ["Select"], "on its page");
    assert_eq!(h.state().session.active().unwrap().doc.name, revision);
    assert!(h.state().ui.dialogs.is_empty(), "no dialog either");
    // An enabled row runs and closes.
    tap(&mut h, "sheet:back");
    tap(&mut h, "sheet:sub:Layer");
    tap(&mut h, "sheet:sub:New");
    tap(&mut h, "sheet:item:layer.new.layer");
    assert!(!mobile_menu::is_open(&h.ctx), "a choice closes the sheet");
    assert!(h.state().menu_cache.is_none());
}

#[test]
fn active_items_show_their_check() {
    let mut h = phone();
    let items = menu_items(h.state());
    let on = items.iter().find(|i| i.path == ["Window"] && i.checked == Some(true)).expect("the active document in Window");
    let id = on.id.clone();
    open_menu(&mut h);
    tap(&mut h, "sheet:sub:Window");
    assert!(noted_rect(&h.ctx, &format!("sheet:check:{id}")).is_some(), "{id} is checked");
}

#[test]
fn dynamic_submenus_come_from_the_items() {
    let mut h = phone_with(|app| {
        app.push_recent("/tmp/uno.png");
        app.push_recent("/tmp/dos.psd");
    });
    open_menu(&mut h);
    tap(&mut h, "sheet:sub:File");
    tap(&mut h, "sheet:sub:Open Recent");
    assert_eq!(path(&h), ["File", "Open Recent"]);
    for k in 0..2 {
        assert!(noted_rect(&h.ctx, &format!("sheet:item:file.openRecent.{k}")).is_some());
    }
    assert!(noted_rect(&h.ctx, "sheet:item:file.clearRecent").is_some());
}

#[test]
fn a_tap_outside_closes_the_sheet_and_paints_nothing() {
    let mut h = phone();
    h.state_mut().ui.tool = crate::state::Tool::Brush;
    open_menu(&mut h);
    let edited = h.state().session.active().unwrap().history.can_undo();
    tap_at(&mut h, pos2(W / 2.0, 20.0));
    assert!(!mobile_sheet::any_open(&h.ctx), "the tap outside closed it");
    assert!(h.state().menu_cache.is_none(), "and dropped the items");
    assert_eq!(h.state().session.active().unwrap().history.can_undo(), edited, "the tap never reached the canvas");
}

#[test]
fn dragging_the_handle_down_closes_a_small_drag_springs_back() {
    let mut h = phone();
    open_menu(&mut h);
    let from = rect(&h, "sheet:handle").center();
    frame(&mut h, vec![touch(TouchPhase::Start, from)]);
    frame(&mut h, vec![touch(TouchPhase::Move, from + vec2(0.0, 20.0))]);
    frame(&mut h, vec![touch(TouchPhase::End, from + vec2(0.0, 20.0))]);
    h.run_steps(4);
    assert!(mobile_menu::is_open(&h.ctx), "a small slow drag springs back");
    assert!((rect(&h, "sheet:handle").center().y - from.y).abs() < 1.0);
    frame(&mut h, vec![touch(TouchPhase::Start, from)]);
    for k in 1..=8 {
        frame(&mut h, vec![touch(TouchPhase::Move, from + vec2(0.0, 40.0 * k as f32))]);
    }
    frame(&mut h, vec![touch(TouchPhase::End, from + vec2(0.0, 320.0))]);
    h.run_steps(4);
    assert!(!mobile_menu::is_open(&h.ctx), "a long drag closes it");
}

fn press_esc(h: &mut H) {
    for pressed in [true, false] {
        frame(h, vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
    }
    h.run_steps(2);
}

/// The phone's Back arrives as Esc: up a page, and from the first page closed.
#[test]
fn back_goes_up_a_page_then_closes() {
    let mut h = phone();
    open_menu(&mut h);
    assert!(crate::simple_ui::back_target(h.state(), &h.ctx), "the web shell keeps a history entry for it");
    tap(&mut h, "sheet:sub:File");
    press_esc(&mut h);
    assert!(mobile_menu::is_open(&h.ctx));
    assert!(path(&h).is_empty());
    press_esc(&mut h);
    assert!(!mobile_menu::is_open(&h.ctx));
    assert!(!crate::simple_ui::back_target(h.state(), &h.ctx));
}

#[test]
fn single_key_shortcuts_stay_off_while_the_sheet_is_open() {
    let mut h = phone();
    assert!(!crate::shortcut_dispatch::keyboard_captured_by_text(h.state(), &h.ctx));
    open_menu(&mut h);
    assert!(crate::shortcut_dispatch::keyboard_captured_by_text(h.state(), &h.ctx));
    assert!(crate::overlay_guard::overlay_open(h.state(), &h.ctx), "a press outside it only dismisses it");
}

#[test]
fn the_search_finds_commands_by_name() {
    let mut h = phone();
    open_menu(&mut h);
    tap(&mut h, "sheet:search");
    h.input_mut().events.push(Event::Text("gaussian".into()));
    h.run_steps(4);
    let id = "filter.blur.gaussianBlur";
    assert!(noted_rect(&h.ctx, &format!("sheet:item:{id}")).is_some(), "the search lists {id}");
    assert!(noted_rect(&h.ctx, "sheet:sub:File").is_none(), "instead of the page");
}

/// Separators are never first, last or doubled, and every menu of the tree has a page.
#[test]
fn pages_keep_the_trees_order_and_separators_tidy() {
    use crate::menus::mobile_menu::{PageRow, page_rows};
    let h = phone();
    let items = menu_items(h.state());
    let root = page_rows(&items, &[]);
    assert_eq!(root.len(), TOP_MENUS.len());
    for top in TOP_MENUS {
        let rows = page_rows(&items, &[top.to_string()]);
        assert!(!rows.is_empty(), "{top}");
        assert!(!matches!(rows.first(), Some(PageRow::Separator)), "{top} starts with a separator");
        assert!(!matches!(rows.last(), Some(PageRow::Separator)), "{top} ends with one");
        assert!(!rows.windows(2).any(|w| matches!(w, [PageRow::Separator, PageRow::Separator])), "{top} doubles one");
    }
    // A submenu whose items are all greyed is itself greyed.
    let greyed = |i: &&crate::menus::MenuItem| i.path == ["File", "Open Recent"];
    assert!(items.iter().filter(greyed).all(|i| !i.enabled || i.label == "---"), "nothing recent in a new session");
    let file = page_rows(&items, &["File".to_string()]);
    assert!(file.iter().any(|r| matches!(r, PageRow::Sub { name: "Open Recent", enabled: false })));
}

/// The desktop keeps its menu bar and popups.
#[test]
fn the_desktop_is_unchanged() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("prefs.set", json!({"values": {"interface.language": "en", "interface.simpleUi": "off"}})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(4);
    assert!(!mobile_sheet::enabled(&h.ctx));
    assert!(noted_rect(&h.ctx, "menu:open").is_none());
    use egui_kittest::kittest::Queryable;
    for top in TOP_MENUS {
        assert!(h.query_by_label(top).is_some(), "{top}");
    }
}
