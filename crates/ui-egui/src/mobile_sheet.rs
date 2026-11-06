//! The bottom sheet of the simple interface: what a phone shows instead of popups. It rises from
//! the bottom across the width of the window, within reach of a thumb, with a drag handle; a drag
//! down, a tap outside or the phone's Back (Esc, `simple_ui::back_target`) closes it. The whole
//! menu tree lives in one (`menus::mobile_menu`), and on a phone the dropdowns, the long-press
//! menus and the press menus open in one too: their contents are the desktop's, in rows a finger
//! can hit. With the simple interface off every entry point here is the plain egui call.
//!
//! One sheet at a time. It holds its place in egui's popup memory while it shows, so everything
//! that stays out of an open popup's way (dialogs, the canvas, the tool options, `overlay_guard`)
//! does for a sheet too. Closed, it costs one lookup per frame.

use egui::{Align, Color32, CornerRadius, Id, InnerResponse, Layout, Order, Popup, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, UiKind, pos2, vec2};

use crate::theme::Tokens;

/// Height of a row (points): a finger's width, as on the phone's own lists.
pub const ROW: f32 = 48.0;
/// The header's height where a sheet has one (Back and the title, or the title).
pub const HEADER: f32 = 48.0;
/// The strip with the drag handle at the top of every sheet.
const HANDLE: f32 = 22.0;
/// Room under the last row, over the safe area.
const BOTTOM_PAD: f32 = 8.0;
/// A wider window (the simple interface turned on by hand) keeps the sheet this wide, centred.
const MAX_WIDTH: f32 = 640.0;
/// A sheet leaves this much of the window above it, the part to tap to close it.
const TOP_ROOM: f32 = 56.0;
/// The slide up, in seconds. Short, and the next press ends it.
const SLIDE: f64 = 0.14;

/// How a sheet is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    /// Height of the header above the contents (it moves the sheet when dragged, too).
    pub header: f32,
    /// As tall as a sheet gets, whatever its contents, so pages don't resize it. Else it fits them.
    pub tall: bool,
    /// A click on the contents closes it, as a click closes a menu popup.
    pub close_on_click: bool,
}

impl Style {
    /// What a popup menu's contents need.
    pub const MENU: Style = Style { header: 0.0, tall: false, close_on_click: true };
}

#[derive(Clone)]
struct OpenSheet {
    id: Id,
    /// The pass it was last drawn in: a sheet whose owner stopped drawing it is closed.
    seen: u64,
    opened_at: f64,
    /// The press that opened it has ended: taps from now on are meant for the sheet.
    armed: bool,
    /// How far a finger has dragged it down, and how fast (points, points per second).
    drag: f32,
    fling: f32,
    /// Where a press that began on the handle is now.
    grab: Option<f32>,
    /// The height its contents asked for last frame.
    fit: Option<f32>,
}

fn state_id() -> Id {
    Id::unique("mobile-sheet")
}

fn popup_id() -> Id {
    Id::unique("mobile-sheet-popup")
}

fn enabled_id() -> Id {
    Id::unique("mobile-sheet-enabled")
}

fn drawing_id() -> Id {
    Id::unique("mobile-sheet-drawing")
}

fn keep_id() -> Id {
    Id::unique("mobile-sheet-keep")
}

fn scroll_fit_id() -> Id {
    Id::unique("mobile-sheet-scroll-fit")
}

/// Each frame: whether popups become sheets (the simple interface is on).
pub fn set_enabled(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(enabled_id(), on));
}

pub fn enabled(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(enabled_id())).unwrap_or(false)
}

/// Whether what's being drawn in `ui` opens its lists in a sheet. Not inside a popup, which a tap
/// on a sheet would close, nor inside a sheet: one level at a time.
pub fn use_sheet(ui: &Ui) -> bool {
    enabled(ui.ctx()) && !drawing(ui.ctx()) && !ui.stack().iter().any(|f| matches!(f.info.kind, Some(UiKind::Popup | UiKind::Menu)))
}

/// Whether a sheet's contents are being drawn right now.
pub fn drawing(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(drawing_id())).unwrap_or(false)
}

fn current(ctx: &egui::Context) -> Option<OpenSheet> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<OpenSheet>(state_id())).filter(|s| s.seen.saturating_add(2) >= pass)
}

/// Is a sheet open? (Shortcuts leave the keys to it, the phone's Back closes it.)
pub fn any_open(ctx: &egui::Context) -> bool {
    current(ctx).is_some()
}

pub fn is_open(ctx: &egui::Context, id: Id) -> bool {
    current(ctx).is_some_and(|s| s.id == id)
}

/// Open the sheet `id` (it must be drawn from now on with [`show`]). Already open, nothing changes.
pub fn open(ctx: &egui::Context, id: Id) {
    if is_open(ctx, id) {
        return;
    }
    let instant = ctx.global_style().animation_time <= 0.0;
    let opened_at = if instant { f64::NEG_INFINITY } else { ctx.input(|i| i.time) };
    let sheet = OpenSheet { id, seen: ctx.cumulative_pass_nr(), opened_at, armed: false, drag: 0.0, fling: 0.0, grab: None, fit: None };
    ctx.data_mut(|d| d.insert_temp(state_id(), sheet));
    ctx.request_repaint();
}

pub fn close(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<OpenSheet>(state_id()));
    Popup::close_id(ctx, popup_id());
    ctx.request_repaint();
}

/// This frame's click on the contents isn't a choice (a [`submenu`] row folding open): the sheet
/// stays.
pub fn keep_open(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(keep_id(), true));
}

/// Whether a drag that let go `drag` points down, at `fling` points per second, closes a sheet of
/// `height`: a third of the way, or a flick.
pub fn should_dismiss(drag: f32, height: f32, fling: f32) -> bool {
    drag > height * 0.33 || (drag > 12.0 && fling > 700.0)
}

/// Where the slide up is at `t` seconds in (0 to 1), easing out.
pub fn slide_progress(t: f64) -> f32 {
    let x = (t / SLIDE).clamp(0.0, 1.0) as f32;
    1.0 - (1.0 - x).powi(3)
}

/// The sheet `id`, if it's open. `header` draws in the strip of [`Style::header`]; `add` the
/// contents, which scroll with [`scroll`].
pub fn show<R>(ctx: &egui::Context, id: Id, style: Style, header: impl FnOnce(&mut Ui), add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let mut st = current(ctx).filter(|s| s.id == id)?;
    st.seen = ctx.cumulative_pass_nr();
    // A popup opened from inside the sheet has the place in egui's memory (there is one) while it
    // lasts, and the keys.
    let nested = Popup::is_any_open(ctx) && !Popup::is_id_open(ctx, popup_id());
    if !nested {
        Popup::open_id(ctx, popup_id());
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close(ctx);
            return None;
        }
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let inset = crate::safe_area::get(ctx);
    let now = ctx.input(|i| i.time);
    if st.armed && ctx.input(|i| i.pointer.any_pressed()) {
        st.opened_at = f64::NEG_INFINITY;
    }
    let up = slide_progress(now - st.opened_at);
    if up < 1.0 {
        ctx.request_repaint();
    }
    let most = (screen.height() - inset.top - TOP_ROOM).max(HANDLE + ROW);
    let height = if style.tall { most } else { st.fit.unwrap_or(most).clamp((HANDLE + style.header + ROW).min(most), most) };
    let width = screen.width().min(MAX_WIDTH);
    let top = screen.bottom() - height + (1.0 - up) * height + st.drag;
    let panel = Rect::from_min_size(pos2(screen.center().x - width / 2.0, top), vec2(width, height));
    let area = id.with("mobile-sheet-area");
    let bottom_pad = BOTTOM_PAD + inset.bottom;
    let mut closing = false;
    let shown = egui::Area::new(area).order(Order::Foreground).fixed_pos(screen.min).constrain(false).fade_in(false).show(ctx, |ui| {
        // One layer: the dim window (a tap there closes the sheet), then the sheet.
        let scrim = ui.allocate_rect(screen, Sense::click());
        ui.painter().rect_filled(screen, 0.0, t.scrim.gamma_multiply(up));
        if scrim.clicked() && st.armed {
            closing = true;
        }
        // Taps on the sheet's own blanks stop here.
        let _ = ui.interact(panel, area.with("panel"), Sense::click());
        let r = t.radius_lg.clamp(0.0, 24.0) as u8;
        let radius = CornerRadius { nw: r, ne: r, sw: 0, se: 0 };
        ui.painter().rect_filled(panel, radius, t.card);
        ui.painter().rect_stroke(panel, radius, Stroke::new(1.0, t.card_border), StrokeKind::Inside);
        // The handle and the header drag the sheet; their buttons, drawn over, keep their taps.
        let head = Rect::from_min_size(panel.min, vec2(width, HANDLE + style.header));
        // Followed by hand, from where the press landed: on a slow phone a flick can leave the strip
        // in the frame the press arrives, and egui would give it to what's under the finger then.
        let (pressed, origin, down, at, dt) = ctx.input(|i| {
            (i.pointer.primary_pressed(), i.pointer.press_origin(), i.pointer.primary_down(), i.pointer.interact_pos(), i.stable_dt.max(1.0 / 240.0))
        });
        if pressed && origin.is_some_and(|o| head.contains(o)) {
            st.grab = origin.map(|o| o.y);
        }
        if let Some(last) = st.grab {
            if down {
                if let Some(y) = at.map(|p| p.y) {
                    let dy = y - last;
                    st.drag = (st.drag + dy).max(0.0);
                    st.fling = st.fling * 0.6 + dy / dt * 0.4;
                    st.grab = Some(y);
                }
            } else {
                closing |= should_dismiss(st.drag, height, st.fling);
                st.drag = 0.0;
                st.fling = 0.0;
                st.grab = None;
            }
        }
        let pill = Rect::from_center_size(pos2(panel.center().x, panel.top() + HANDLE / 2.0), vec2(40.0, 4.0));
        ui.painter().rect_filled(pill, 2.0, t.text_faint);
        crate::simple_ui::note(ctx, "sheet:handle", head);
        let clip = panel.intersect(screen);
        if style.header > 0.0 {
            let strip = Rect::from_min_size(panel.min + vec2(0.0, HANDLE), vec2(width, style.header));
            let mut h = ui.new_child(UiBuilder::new().max_rect(strip).layout(Layout::left_to_right(Align::Center)));
            h.set_clip_rect(clip);
            header(&mut h);
        }
        let body_rect = Rect::from_min_max(panel.min + vec2(0.0, HANDLE + style.header), pos2(panel.right(), panel.bottom() - bottom_pad));
        let mut body = ui.new_child(UiBuilder::new().max_rect(body_rect).layout(Layout::top_down_justified(Align::Min)).closable());
        body.set_clip_rect(clip);
        finger_style(&mut body);
        ctx.data_mut(|d| d.insert_temp(drawing_id(), true));
        let inner = add(&mut body);
        ctx.data_mut(|d| d.insert_temp(drawing_id(), false));
        if let Some((scroll_top, content)) = ctx.data_mut(|d| d.remove_temp::<(f32, f32)>(scroll_fit_id())) {
            st.fit = Some(scroll_top - panel.top() + content + bottom_pad);
        }
        let kept = ctx.data_mut(|d| d.remove_temp::<bool>(keep_id())).unwrap_or(false);
        let clicked = ctx.input(|i| i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| body_rect.contains(p)));
        closing |= body.should_close() || (style.close_on_click && st.armed && clicked && !kept);
        inner
    });
    // The press that opened it ends when no button or finger is down.
    st.armed |= !ctx.input(|i| i.pointer.any_down());
    if closing {
        close(ctx);
    } else {
        ctx.data_mut(|d| d.insert_temp(state_id(), st));
        // Over a dialog or a panel sheet it was opened from, and over the Capi button.
        if !nested {
            ctx.move_to_top(egui::LayerId::new(Order::Foreground, area));
        }
    }
    Some(shown.inner)
}

/// Rows a finger can hit, for the plain widgets of popup contents drawn in a sheet.
fn finger_style(ui: &mut Ui) {
    let s = ui.spacing_mut();
    s.button_padding = vec2(16.0, 15.0);
    s.interact_size.y = ROW;
    s.item_spacing.y = 0.0;
    let st = ui.style_mut();
    st.wrap_mode = Some(egui::TextWrapMode::Truncate);
    for text in [egui::TextStyle::Button, egui::TextStyle::Body] {
        st.text_styles.insert(text, crate::theme::medium(15.0));
    }
    let v = &mut st.visuals.widgets.inactive;
    v.weak_bg_fill = Color32::TRANSPARENT;
    v.bg_stroke = Stroke::NONE;
}

/// The part of a sheet that scrolls (a finger's drag, gliding on after it lets go). What's in it
/// sizes a sheet that fits its contents.
pub fn scroll<R>(ui: &mut Ui, salt: impl egui::AsIdSalt, add: impl FnOnce(&mut Ui) -> R) -> R {
    let top = ui.cursor().top();
    let height = ui.available_height();
    let out = egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, true]).max_height(height).show(ui, |ui| {
        ui.with_layout(Layout::top_down_justified(Align::Min), add).inner
    });
    let content = out.content_size.y;
    ui.ctx().data_mut(|d| d.insert_temp(scroll_fit_id(), (top, content)));
    out.inner
}

/// What a [`row`] shows at its far end.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mark {
    #[default]
    None,
    /// An active item.
    Check,
    /// Goes to a page.
    Chevron,
    /// A section folded open or closed ([`submenu`]).
    Fold(bool),
}

/// One row of a sheet.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub label: &'a str,
    pub icon: Option<&'a str>,
    /// Leave room for an icon, so the labels of a list that has some line up.
    pub gutter: bool,
    pub mark: Mark,
    pub enabled: bool,
    /// Edit › Menus colour.
    pub tint: Option<Color32>,
}

impl<'a> Row<'a> {
    pub fn new(label: &'a str) -> Row<'a> {
        Row { label, icon: None, gutter: false, mark: Mark::None, enabled: true, tint: None }
    }
}

/// Draws a row [`ROW`] tall across the width. A disabled one is greyed and takes no taps.
pub fn row(ui: &mut Ui, spec: &Row<'_>) -> Response {
    let t = Tokens::get(ui.ctx());
    let sense = if spec.enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    let label = spec.label.to_string();
    let selected = spec.mark == Mark::Check;
    let enabled = spec.enabled;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, &label));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    // A finger dragging the list isn't pressing the row it started on.
    let dragging = ui.input(|i| i.pointer.is_decidedly_dragging());
    if let Some(tint) = spec.tint {
        ui.painter().rect_filled(rect, 0.0, tint.gamma_multiply(0.55));
    }
    if spec.enabled && !dragging && resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 0.0, t.pressed);
    } else if spec.enabled && !dragging && resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.hover);
    }
    let color = if spec.enabled { t.text } else { t.text_faint };
    let rtl = crate::direction::is_rtl();
    // From the near edge: margin, icon, label; from the far edge: margin, mark.
    let side = |from_near: f32, w: f32| {
        let x = if rtl { rect.right() - from_near - w } else { rect.left() + from_near };
        Rect::from_min_size(pos2(x, rect.top() + (ROW - w) / 2.0), vec2(w, w))
    };
    let far = |from_far: f32, w: f32| {
        let x = if rtl { rect.left() + from_far } else { rect.right() - from_far - w };
        Rect::from_min_size(pos2(x, rect.top() + (ROW - w) / 2.0), vec2(w, w))
    };
    let mut start = 16.0;
    if let Some(icon) = spec.icon {
        crate::icons::paint(ui, side(start, 22.0), icon, 20.0, color);
    }
    if spec.icon.is_some() || spec.gutter {
        start += 22.0 + 12.0;
    }
    let mark = match spec.mark {
        Mark::None => None,
        Mark::Check => Some(("check", t.accent)),
        Mark::Chevron if rtl => Some(("chevron-left", t.text_faint)),
        Mark::Chevron => Some(("chevron-right", t.text_faint)),
        Mark::Fold(true) => Some(("chevron-down", t.text_faint)),
        Mark::Fold(false) if rtl => Some(("chevron-left", t.text_faint)),
        Mark::Fold(false) => Some(("chevron-right", t.text_faint)),
    };
    if let Some((icon, tint)) = mark {
        crate::icons::paint(ui, far(14.0, 20.0), icon, 18.0, if spec.enabled { tint } else { t.text_faint });
    }
    let room = (rect.width() - start - if mark.is_some() { 14.0 + 20.0 + 8.0 } else { 16.0 }).max(0.0);
    let mut job = egui::text::LayoutJob::simple_singleline(label, crate::theme::medium(15.0), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let galley = ui.painter().layout_job(job);
    let x = if rtl { rect.right() - start - galley.size().x } else { rect.left() + start };
    ui.painter().galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

/// `ui.menu_button` for contents that may be drawn in a sheet, where a flyout would cover it: the
/// row folds its items open below it instead. Anywhere else it is the plain submenu.
pub fn submenu<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<Option<R>> {
    if !drawing(ui.ctx()) {
        return ui.menu_button(title, add);
    }
    let id = ui.make_persistent_id(("sheet-submenu", title));
    let open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let resp = row(ui, &Row { mark: Mark::Fold(open), ..Row::new(title) });
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(id, !open));
        keep_open(ui.ctx());
    }
    let inner = open.then(|| ui.indent(id, add).inner);
    InnerResponse::new(inner, resp)
}

/// `ComboBox::show_ui`: where [`use_sheet`] allows, the box opens its list in a sheet (`title` on
/// top, if it has one), with `add`'s rows finger-sized.
pub fn combo<R>(ui: &mut Ui, combo: craft_ui::ComboBox<'_>, title: &str, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<Option<R>> {
    if !use_sheet(ui) {
        return combo.show_ui(ui, add);
    }
    let shown = combo_button(ui, combo);
    let id = shown.id.with("mobile-sheet");
    if craft_ui::ComboBox::is_open(ui.ctx(), shown.id) {
        close_combo_popup(ui.ctx(), shown.id);
        open(ui.ctx(), id);
    }
    let title = title.to_string();
    let style = Style { header: if title.is_empty() { 0.0 } else { HEADER }, ..Style::MENU };
    let inner = show(ui.ctx(), id, style, |ui| sheet_title(ui, &title), |ui| scroll(ui, id, add));
    InnerResponse::new(inner, shown)
}

/// The box alone: its own list, never shown, opens the sheet instead. A list opens unseen on its
/// first frame (egui sizes it), and [`close_combo_popup`] closes it in that frame.
pub(crate) fn combo_button(ui: &mut Ui, combo: craft_ui::ComboBox<'_>) -> Response {
    combo.show_ui(ui, |_| {}).response
}

/// Closes the egui list of the combo box whose response has this id (`craft_ui`'s `popup_id`).
pub(crate) fn close_combo_popup(ctx: &egui::Context, button: Id) {
    Popup::close_id(ctx, button.with("popup"));
}

/// A header with only a title.
pub fn sheet_title(ui: &mut Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(16.0);
    let mut job = egui::text::LayoutJob::simple_singleline(title.to_string(), crate::theme::semibold(15.0), t.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width((ui.available_width() - 16.0).max(0.0));
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, t.text);
}

/// [`egui::Response::context_menu`], in a sheet on a phone.
pub trait SheetMenu {
    fn sheet_menu(&self, add: impl FnOnce(&mut Ui)) -> Option<InnerResponse<()>>;
    fn sheet_menu_opened(&self) -> bool;
}

impl SheetMenu for Response {
    fn sheet_menu(&self, add: impl FnOnce(&mut Ui)) -> Option<InnerResponse<()>> {
        let ctx = &self.ctx;
        // Inside a popup (open now, and not the sheet) the menu stays one too.
        if !enabled(ctx) || drawing(ctx) || (Popup::is_any_open(ctx) && !any_open(ctx)) {
            return self.context_menu(add);
        }
        let id = Popup::default_response_id(self).with("mobile-sheet");
        if self.secondary_clicked() {
            open(ctx, id);
        }
        show(ctx, id, Style::MENU, |_| {}, |ui| scroll(ui, id, add)).map(|()| InnerResponse::new((), self.clone()))
    }

    fn sheet_menu_opened(&self) -> bool {
        is_open(&self.ctx, Popup::default_response_id(self).with("mobile-sheet")) || self.context_menu_opened()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2};

    #[test]
    fn a_long_enough_or_fast_enough_drag_dismisses() {
        assert!(!should_dismiss(0.0, 600.0, 0.0));
        assert!(!should_dismiss(100.0, 600.0, 100.0), "a small slow drag springs back");
        assert!(should_dismiss(250.0, 600.0, 0.0), "a third of the way");
        assert!(should_dismiss(30.0, 600.0, 900.0), "a flick");
        assert!(!should_dismiss(5.0, 600.0, 900.0), "not a jitter");
    }

    #[test]
    fn the_slide_is_short_and_ends_at_rest() {
        assert_eq!(slide_progress(0.0), 0.0);
        assert_eq!(slide_progress(SLIDE), 1.0);
        assert_eq!(slide_progress(f64::INFINITY), 1.0, "skipped");
        assert!(SLIDE <= 0.2);
    }

    /// A bare context with a dropdown of three options.
    struct Rig {
        ctx: egui::Context,
        value: usize,
        phone: bool,
    }

    impl Rig {
        fn new(phone: bool) -> Rig {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            let mut rig = Rig { ctx, value: 0, phone };
            rig.frame(Vec::new());
            rig.frame(Vec::new());
            rig
        }

        fn frame(&mut self, events: Vec<Event>) -> bool {
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(360.0, 780.0));
            let (mut value, phone) = (self.value, self.phone);
            let mut changed = false;
            let mut out = self.ctx.clone().run_ui(egui::RawInput { screen_rect: Some(rect), events, ..Default::default() }, |ui| {
                set_enabled(ui.ctx(), phone);
                ui.add_space(20.0);
                changed = crate::widgets::dropdown(ui, "mode", "Mode", &mut value, &[(0, "One"), (1, "Two"), (2, "Three")], 120.0);
            });
            out.textures_delta.clear();
            self.value = value;
            changed
        }

        fn tap(&mut self, pos: Pos2) -> bool {
            self.frame(vec![Event::PointerMoved(pos)]);
            let a = self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
            let b = self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
            self.frame(Vec::new());
            self.frame(Vec::new());
            a || b
        }
    }

    #[test]
    fn on_a_phone_a_dropdown_opens_a_sheet_and_a_tap_chooses() {
        let mut rig = Rig::new(true);
        let at = pos2(40.0, 30.0);
        rig.tap(at);
        assert!(any_open(&rig.ctx), "a sheet");
        assert!(Popup::is_any_open(&rig.ctx), "holding egui's popup place, so dialogs and the canvas keep out");
        // The sheet's rows from the bottom of the window: One, Two, Three; the last is Three.
        let three = pos2(180.0, 780.0 - BOTTOM_PAD - ROW / 2.0);
        assert!(rig.tap(three), "the tap chose");
        assert_eq!(rig.value, 2);
        assert!(!any_open(&rig.ctx), "and closed the sheet");
        rig.frame(Vec::new());
        rig.frame(Vec::new());
        assert!(!Popup::is_any_open(&rig.ctx), "nothing left open");
    }

    #[test]
    fn a_tap_outside_closes_a_dropdown_sheet_and_changes_nothing() {
        let mut rig = Rig::new(true);
        rig.tap(pos2(40.0, 30.0));
        assert!(any_open(&rig.ctx));
        assert!(!rig.tap(pos2(180.0, 200.0)), "the tap on the dim window chose nothing");
        assert_eq!(rig.value, 0);
        assert!(!any_open(&rig.ctx));
    }

    #[test]
    fn esc_closes_a_sheet() {
        let mut rig = Rig::new(true);
        rig.tap(pos2(40.0, 30.0));
        assert!(any_open(&rig.ctx));
        for pressed in [true, false] {
            rig.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        assert!(!any_open(&rig.ctx));
    }

    #[test]
    fn a_sheet_nobody_draws_any_more_is_closed() {
        let ctx = egui::Context::default();
        let id = Id::new("gone");
        for frame in 0..4 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                if frame == 0 {
                    open(ui.ctx(), id);
                    assert!(any_open(ui.ctx()));
                }
            });
            out.textures_delta.clear();
        }
        assert!(!any_open(&ctx));
    }

    #[test]
    fn off_the_simple_interface_a_dropdown_is_the_plain_popup() {
        let mut rig = Rig::new(false);
        rig.tap(pos2(40.0, 30.0));
        assert!(!any_open(&rig.ctx), "no sheet on the desktop");
        assert!(Popup::is_any_open(&rig.ctx), "its list");
    }
}
