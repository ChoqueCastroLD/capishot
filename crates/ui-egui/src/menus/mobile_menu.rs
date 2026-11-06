//! The menus on a phone: one Menú button in the top bar opens a bottom sheet
//! (`crate::mobile_sheet`) whose first page lists the menus. A submenu is the next page, with Back
//! and its title on top: one level at a time, nothing over anything. The rows are [`menu_items`]'s,
//! so new commands, enabled states, checks and the dynamic lists (recent files, themes, documents)
//! come by themselves; greyed items stay listed and do nothing. The items are built only while the
//! sheet is open.

use egui::{Align, Id, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use serde_json::{Value, json};

use super::PhotocraftApp;
use super::TOP_MENUS;
use super::bar::run_choice;
use super::items::{ItemCache, MenuItem, menu_items, menu_tint};
use super::render::submenu_enabled;
use super::search::search_items;
use super::state::translated_menu_label;
use crate::mobile_sheet::{self, Mark, Row};
use crate::theme::Tokens;

/// From this many rows a page has the search field on top (the first page always).
const SEARCH_FROM: usize = 12;
/// The slide between pages, in seconds. Short, and the next press ends it.
const PAGE_SLIDE: f64 = 0.12;
/// How far (of the sheet's width) a page slides from.
const PAGE_SLIDE_FROM: f32 = 0.25;

/// A row of a page.
#[derive(Debug)]
pub(crate) enum PageRow<'a> {
    Separator,
    Item(&'a MenuItem),
    /// The next page.
    Sub {
        name: &'a str,
        enabled: bool,
    },
}

/// The rows of the page at `path`: at the top the menus, below it what the menu bar shows at that
/// depth, in its order. Separators are never first, last or doubled.
pub(crate) fn page_rows<'a>(items: &'a [MenuItem], path: &[String]) -> Vec<PageRow<'a>> {
    if path.is_empty() {
        return TOP_MENUS
            .iter()
            .filter(|top| items.iter().any(|i| i.path.first().map(String::as_str) == Some(**top)))
            .map(|top| PageRow::Sub { name: *top, enabled: true })
            .collect();
    }
    let depth = path.len();
    let here: Vec<&MenuItem> = items.iter().filter(|i| i.path.get(..depth) == Some(path)).collect();
    let mut rows = Vec::new();
    let mut subs: Vec<&str> = Vec::new();
    let mut last_was_sep = true;
    for &it in &here {
        match it.path.get(depth) {
            None if it.label == "---" => {
                if !last_was_sep {
                    rows.push(PageRow::Separator);
                    last_was_sep = true;
                }
            }
            None => {
                rows.push(PageRow::Item(it));
                last_was_sep = false;
            }
            Some(name) => {
                if subs.contains(&name.as_str()) {
                    continue;
                }
                subs.push(name);
                let child: Vec<&MenuItem> = here.iter().copied().filter(|c| c.path.get(depth) == Some(name)).collect();
                rows.push(PageRow::Sub { name, enabled: submenu_enabled(&child) });
                last_was_sep = false;
            }
        }
    }
    if matches!(rows.last(), Some(PageRow::Separator)) {
        rows.pop();
    }
    rows
}

/// Where the menu sheet is.
#[derive(Clone, Default)]
struct Page {
    path: Vec<String>,
    query: String,
    /// When the page last changed, and whether by going in (for the slide).
    since: f64,
    forward: bool,
}

fn sheet_id() -> Id {
    Id::unique("mobile-menu-sheet")
}

fn page_id() -> Id {
    Id::unique("mobile-menu-page")
}

fn load(ctx: &egui::Context) -> Page {
    ctx.data(|d| d.get_temp(page_id())).unwrap_or_default()
}

pub fn is_open(ctx: &egui::Context) -> bool {
    mobile_sheet::is_open(ctx, sheet_id())
}

/// Opens the sheet at the first page.
pub fn open(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(page_id(), Page { since: f64::NEG_INFINITY, ..Page::default() }));
    mobile_sheet::open(ctx, sheet_id());
}

/// `ui.inspect`: the sheet's page while it's open.
pub fn inspect(ctx: &egui::Context) -> Value {
    if is_open(ctx) { json!({"path": load(ctx).path}) } else { Value::Null }
}

/// The top bar's button; returns its right edge.
pub fn button(ui: &mut Ui) -> f32 {
    let t = Tokens::get(ui.ctx());
    let label = tl!("Menu");
    let galley = ui.painter().layout_no_wrap(label.to_string(), crate::theme::medium(14.0), t.text);
    let bars = 16.0;
    let size = vec2(8.0 + bars + 6.0 + galley.size().x + 10.0, 40.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let open_now = is_open(ui.ctx());
    if open_now || resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, t.radius_sm, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover);
    }
    let x = rect.left() + 8.0;
    for dy in [-5.0, 0.0, 5.0] {
        let y = rect.center().y + dy;
        ui.painter().line_segment([pos2(x, y), pos2(x + bars, y)], egui::Stroke::new(1.8, t.text));
    }
    let at = pos2(x + bars + 6.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(at, galley, t.text);
    crate::simple_ui::note(ui.ctx(), "menu:open", rect);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.clicked() {
        open(ui.ctx());
    }
    rect.right()
}

/// What a tap on the sheet asks for.
enum Tap {
    Run(String),
    In(String),
    Back,
}

/// The sheet, while it's open.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !is_open(ctx) {
        app.menu_cache = None;
        return;
    }
    let state = crate::native_menu::state_hash(app, None);
    let input = crate::native_menu::had_input(ctx);
    let items = app.menu_cache.take().and_then(|c| c.reuse(state, input)).unwrap_or_else(|| menu_items(app));
    let mut page = load(ctx);
    let now = ctx.input(|i| i.time);
    // Back goes up a page, and from the first page closes the sheet ([`mobile_sheet::show`]).
    if !page.path.is_empty() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        go_back(&mut page, now);
    }
    let mut tap: Option<Tap> = None;
    let lang = crate::i18n::current();
    let title = page.path.last().map_or_else(|| tl!("Menu").to_string(), |name| crate::i18n::tr(lang, name).to_string());
    let style = mobile_sheet::Style { header: mobile_sheet::HEADER, tall: true, close_on_click: false };
    let at_top = page.path.is_empty();
    let header_tap = std::cell::Cell::new(false);
    let shown = mobile_sheet::show(
        ctx,
        sheet_id(),
        style,
        |ui| header(ui, &title, at_top, &header_tap),
        |ui| {
            if ui.input(|i| i.pointer.any_pressed()) {
                page.since = f64::NEG_INFINITY;
            }
            let dx = page_offset(ui.ctx(), now - page.since, page.forward, ui.available_width());
            let rect = ui.max_rect().translate(vec2(dx, 0.0));
            ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::top_down_justified(Align::Min)), |ui| body(ui, &items, &mut page, &mut tap));
        },
    );
    if header_tap.get() {
        tap = Some(Tap::Back);
    }
    match tap {
        Some(Tap::Run(id)) => {
            mobile_sheet::close(ctx);
            app.menu_cache = None;
            run_choice(app, ctx, id);
            return;
        }
        Some(Tap::In(name)) => {
            page.path.push(name);
            page.query.clear();
            page.since = now;
            page.forward = true;
            ctx.request_repaint();
        }
        Some(Tap::Back) => go_back(&mut page, now),
        None => {}
    }
    if shown.is_none() || !is_open(ctx) {
        app.menu_cache = None;
        return;
    }
    ctx.data_mut(|d| d.insert_temp(page_id(), page));
    app.menu_cache = Some(ItemCache { state, items });
}

fn go_back(page: &mut Page, now: f64) {
    page.path.pop();
    page.query.clear();
    page.since = now;
    page.forward = false;
}

/// How far the page is from its place, `t` seconds after it changed; asks for the next frame while
/// it moves. Instant with the UI's animations off.
fn page_offset(ctx: &egui::Context, t: f64, forward: bool, width: f32) -> f32 {
    if ctx.global_style().animation_time <= 0.0 || t >= PAGE_SLIDE {
        return 0.0;
    }
    ctx.request_repaint();
    let x = (t / PAGE_SLIDE).clamp(0.0, 1.0) as f32;
    let left = (1.0 - x).powi(3) * PAGE_SLIDE_FROM * width;
    // Going in, the page comes from the far side.
    if forward != crate::direction::is_rtl() { left } else { -left }
}

/// `‹ Atrás` and the title, or the first page's title.
fn header(ui: &mut Ui, title: &str, at_top: bool, back: &std::cell::Cell<bool>) {
    if at_top {
        mobile_sheet::sheet_title(ui, title);
        return;
    }
    let t = Tokens::get(ui.ctx());
    let label = tl!("Back");
    let galley = ui.painter().layout_no_wrap(label.to_string(), crate::theme::medium(15.0), t.accent);
    let (rect, resp) = ui.allocate_exact_size(vec2(12.0 + 20.0 + 2.0 + galley.size().x + 14.0, mobile_sheet::HEADER), Sense::click());
    if resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, t.radius_sm, t.pressed);
    }
    let rtl = crate::direction::is_rtl();
    let chevron = Rect::from_min_size(pos2(rect.left() + 12.0, rect.center().y - 10.0), vec2(20.0, 20.0));
    crate::icons::paint(ui, chevron, if rtl { "chevron-right" } else { "chevron-left" }, 18.0, t.accent);
    let at = pos2(chevron.right() + 2.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(at, galley, t.accent);
    crate::simple_ui::note(ui.ctx(), "sheet:back", rect);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.clicked() {
        back.set(true);
    }
    ui.add_space(4.0);
    let mut job = egui::text::LayoutJob::simple_singleline(title.to_string(), crate::theme::semibold(15.0), t.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width((ui.available_width() - 16.0).max(0.0));
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, t.text);
}

/// The search field, when the page has one, and the rows or the search's results.
fn body(ui: &mut Ui, items: &[MenuItem], page: &mut Page, tap: &mut Option<Tap>) {
    let lang = crate::i18n::current();
    let rows = page_rows(items, &page.path);
    let mut hits = Vec::new();
    if page.path.is_empty() || rows.len() >= SEARCH_FROM {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let field = egui::TextEdit::singleline(&mut page.query)
                .id(Id::unique("mobile-menu-search"))
                .hint_text(tl!("Search menus"))
                .font(crate::theme::medium(15.0))
                .margin(vec2(12.0, 11.0))
                .desired_width((ui.available_width() - 12.0).max(40.0));
            let field = ui.add(field);
            crate::simple_ui::note(ui.ctx(), "sheet:search", field.rect);
            hits = search_items(items, &page.query, lang);
            if field.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Some(it) = hits.iter().find(|it| it.enabled)
            {
                *tap = Some(Tap::Run(it.id.clone()));
            }
        });
        ui.add_space(6.0);
    }
    let searching = !page.query.trim().is_empty();
    let salt = ("mobile-menu-rows", page.path.join("\u{1f}"), searching);
    mobile_sheet::scroll(ui, salt, |ui| {
        if searching {
            if hits.is_empty() {
                ui.add_space(14.0);
                ui.vertical_centered(|ui| ui.weak(tl!("No matching commands")));
            }
            for it in hits {
                let mut trail: Vec<String> = it.path.iter().map(|p| crate::i18n::tr(lang, p).to_string()).collect();
                trail.push(translated_menu_label(lang, it));
                item_row(ui, it, &trail.join(" › "), false, tap);
            }
            return;
        }
        if rows.is_empty() {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| ui.weak(tl!("(coming soon)")));
        }
        let gutter = rows.iter().any(|r| matches!(r, PageRow::Item(it) if crate::simple_ui::command_icon(&it.id).is_some()));
        for row in &rows {
            match row {
                PageRow::Separator => {
                    ui.add(egui::Separator::default().spacing(9.0));
                }
                PageRow::Item(it) => item_row(ui, it, &translated_menu_label(lang, it), gutter, tap),
                PageRow::Sub { name, enabled } => {
                    let label = crate::i18n::tr(lang, name);
                    let resp = mobile_sheet::row(ui, &Row { gutter, mark: Mark::Chevron, enabled: *enabled, ..Row::new(label) });
                    crate::simple_ui::note(ui.ctx(), &format!("sheet:sub:{name}"), resp.rect);
                    if resp.clicked() {
                        *tap = Some(Tap::In((*name).to_string()));
                    }
                }
            }
        }
    });
}

fn item_row(ui: &mut Ui, it: &MenuItem, label: &str, gutter: bool, tap: &mut Option<Tap>) {
    let active = it.checked == Some(true);
    let spec = Row {
        icon: crate::simple_ui::command_icon(&it.id),
        gutter,
        mark: if active { Mark::Check } else { Mark::None },
        enabled: it.enabled,
        tint: it.color.as_deref().and_then(menu_tint),
        ..Row::new(label)
    };
    let resp = mobile_sheet::row(ui, &spec);
    crate::simple_ui::note(ui.ctx(), &format!("sheet:item:{}", it.id), resp.rect);
    if active {
        crate::simple_ui::note(ui.ctx(), &format!("sheet:check:{}", it.id), resp.rect);
    }
    if it.enabled && resp.clicked() {
        *tap = Some(Tap::Run(it.id.clone()));
    }
}
