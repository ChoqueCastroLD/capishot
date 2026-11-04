//! The whole editor (menus, options bar, toolbar, panels, dialogs, every tool) in a rect of a
//! host app's own window: `capishot-host`'s full mode, next to the canvas-only [`super::frame`].
//!
//! It is the standalone app's own frame (`logic_body` and `ui_body_frame`), with three
//! differences, all scoped to the call:
//! - **Style.** Capishot's style applies to its own drawing only: the context's styles are swapped
//!   for the frame and put back after it (also on a panic), so the host's widgets before and after
//!   keep the host's look. Fonts are the host's (never replaced; named weights fall back to its
//!   proportional font), and the theme preference is left alone.
//! - **Window.** Close, title, maximize, full screen, focus, size and move become
//!   [`FullFrame::window`] (see [`crate::hosted`]); the window close guard and the "fit the
//!   window to the display" step don't run.
//! - **Keys.** The host decides which key events the editor sees ([`super::Keys`]); the rest stay
//!   in egui's input for the host's widgets.

use std::sync::Arc;

use egui::{Rect, UiBuilder};
use photocraft_engine::jobs::JobEvent;

use super::{KeyRouting, Keys, default_filter, install, is_keyboard};
use crate::canvas::ViewXform;
use crate::{PhotocraftApp, shortcuts, theme};

/// The full frame's options.
#[derive(Clone, Debug, Default)]
pub struct FullOptions {
    /// The host's say over the keyboard. [`KeyRouting::WhenActive`] here means: since the last
    /// press inside the editor's rect (or on one of its windows), until a press on the host's
    /// own UI, or while the pointer is over the rect and no host text field has the keyboard.
    pub keys: Keys,
    /// The editor's colours; `None` is the editor's own theme (Preferences › Interface).
    pub tokens: Option<theme::Tokens>,
}

/// What a full frame keeps between frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct FullState {
    /// The editor has the keyboard focus (pressed in last).
    pub focused: bool,
}

/// What [`full_frame`] did.
#[derive(Debug)]
pub struct FullFrame {
    /// Background jobs that ended this frame (the host API reports them).
    pub jobs: Vec<JobEvent>,
    /// The editor saw the keyboard this frame.
    pub took_keys: bool,
    /// What the editor asked of the window (the host's to carry out or ignore), oldest first.
    pub window: Vec<egui::ViewportCommand>,
    /// The active document's canvas mapping, for host overlays.
    pub xform: Option<ViewXform>,
}

fn rect_id() -> egui::Id {
    egui::Id::unique("capishot-hosted-rect")
}

/// The rect of the hosted editor drawn in this pass (`None` outside a full frame).
pub fn hosted_rect(ctx: &egui::Context) -> Option<Rect> {
    crate::hosted::active().then(|| ctx.data(|d| d.get_temp::<Rect>(rect_id()))).flatten()
}

/// Puts the host's styles back when dropped (also when the editor panics).
struct StyleSwap {
    ctx: egui::Context,
    light: Arc<egui::Style>,
    dark: Arc<egui::Style>,
}

impl StyleSwap {
    fn enter(ctx: &egui::Context, style: &Arc<egui::Style>) -> Self {
        let swap = StyleSwap { ctx: ctx.clone(), light: ctx.style_of(egui::Theme::Light), dark: ctx.style_of(egui::Theme::Dark) };
        ctx.set_style_of(egui::Theme::Light, Arc::clone(style));
        ctx.set_style_of(egui::Theme::Dark, Arc::clone(style));
        swap
    }
}

impl Drop for StyleSwap {
    fn drop(&mut self) {
        self.ctx.set_style_of(egui::Theme::Light, Arc::clone(&self.light));
        self.ctx.set_style_of(egui::Theme::Dark, Arc::clone(&self.dark));
    }
}

/// Whether the editor sees the keyboard this frame.
fn takes_keys(app: &PhotocraftApp, ctx: &egui::Context, rect: Rect, routing: KeyRouting, state: &mut FullState) -> bool {
    // A press decides where the keyboard goes: in the rect (on the background layer, under no
    // popup) it's the editor's, on the host's background layer the host's; presses on floating
    // layers (the editor's windows, menus and dialogs, or the host's) leave it where it was.
    if let Some(p) = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None }) {
        let background = ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background);
        if background {
            state.focused = rect.contains(p);
        }
    }
    match routing {
        KeyRouting::Never => false,
        KeyRouting::Always => true,
        KeyRouting::WhenActive => {
            let hovered = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)));
            state.focused || (hovered && !ctx.text_edit_focused()) || app.drag.is_some()
        }
    }
}

/// The whole editor in `rect` (egui points, inside `ui`): the standalone app's frame scoped to
/// the rect, with the host's say over keys and window (see the module docs). `app.session` must
/// hold the host's session for the call.
pub fn full_frame(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, opts: &FullOptions, state: &mut FullState) -> FullFrame {
    let ctx = ui.ctx().clone();
    install(&ctx);
    // Image loaders (the icons are SVGs): adds egui_extras' loaders unless the host has them.
    egui_extras::install_image_loaders(&ctx);
    let hosted = crate::hosted::Hosted::enter();
    ctx.data_mut(|d| d.insert_temp(rect_id(), rect));
    let tokens = opts.tokens.unwrap_or_else(|| theme::Tokens::for_kind(app.ui.theme));
    theme::publish_tokens(&ctx, tokens);
    let style = Arc::new(theme::styled(&tokens, (*ctx.global_style()).clone()));
    let swap = StyleSwap::enter(&ctx, &style);

    // The keyboard the host lets through, read as the standalone app's input hook reads it.
    let take = takes_keys(app, &ctx, rect, opts.keys.routing, state);
    let allow = |e: &egui::Event| take && opts.keys.filter.as_ref().map_or_else(|| default_filter(e), |f| f(e));
    let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
    let (mut mine, withheld): (Vec<egui::Event>, Vec<egui::Event>) = events.into_iter().partition(|e| !is_keyboard(e) || allow(e));
    crate::widgets::cap_text_events(&mut mine);
    let mut raw = egui::RawInput { events: mine, ..Default::default() };
    shortcuts::clipboard_keys(&ctx, ctx.text_edit_focused() || app.ui.text_edit.is_some(), &mut raw);
    ctx.input_mut(|i| i.events = raw.events);

    app.logic_body(&ctx, None);
    app.finish_type_reset_logic(&ctx);
    let clip = rect.intersect(ui.clip_rect());
    let mut editor = ui.new_child(UiBuilder::new().max_rect(rect).id_salt("capishot-full").style(Arc::clone(&style)));
    editor.set_clip_rect(clip);
    app.ui_body_frame(&mut editor, None);
    app.finish_type_reset_pass(&ctx);
    ui.advance_cursor_after_rect(rect);
    let xform = ViewXform::active(app);

    // The keys the editor didn't see, then what it left, for the host's widgets after it.
    ctx.input_mut(|i| {
        let left = std::mem::take(&mut i.events);
        i.events = withheld;
        i.events.extend(left);
    });
    drop(swap);
    let jobs = std::mem::take(&mut app.jobs.hosted_ended);
    let window = hosted.finish().into_iter().filter(|c| !matches!(c, egui::ViewportCommand::RequestPaintWhileHidden)).collect();
    FullFrame { jobs, took_keys: take, window, xform }
}
