//! A canvas press a tool dropped says why, and the statistics count it (`toolDrop.<reason>.<tool>`).
//!
//! The tools here do their work on a click or a drag egui has recognised, or after their release,
//! so a press can end without anything having happened: the user sees a tool that doesn't work and
//! nothing says so. Each press of those tools is followed until the button is up; if it ran no
//! command, moved no view and opened or said nothing, [`classify`] names what took it. A key that
//! seems stuck tells at once (its tap is the way out); the others on the second drop in a row, as
//! one quiet press can be a click that had nothing to do (Zoom at its limit is told apart here).

use crate::PhotocraftApp;
use crate::hold_keys::Temporary;
use crate::state::Tool;

/// Seconds a temporary tool's key must have been down for a press to doubt it.
const STALE_SECS: f64 = 15.0;
/// Tags the notice (one at a time).
const NOTICE_TAG: &str = "toolDrop";

/// What took a press. [`Reason::code`] is what the statistics carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reason {
    /// A menu or popup is open and stayed open (`overlay_guard` swallows presses for it).
    Overlay,
    Dialog,
    /// A Free Transform box is open and owns the pointer.
    Transform,
    /// A temporary tool's key has been down so long it may be stuck.
    HandKey,
    ZoomKey,
    MoveKey,
    /// A tool letter has been down so long it may be stuck (`spring_tools`).
    ToolKey,
    /// egui recognised neither a click nor a drag.
    NoClick,
    /// A click or drag reached the canvas but no tool event came of it.
    NoDown,
    /// The tool had its event and nothing came of it.
    NoEffect,
}

impl Reason {
    pub(super) fn code(self) -> &'static str {
        match self {
            Reason::Overlay => "overlay",
            Reason::Dialog => "dialog",
            Reason::Transform => "transform",
            Reason::HandKey => "hand_key",
            Reason::ZoomKey => "zoom_key",
            Reason::MoveKey => "move_key",
            Reason::ToolKey => "tool_key",
            Reason::NoClick => "no_click",
            Reason::NoDown => "no_down",
            Reason::NoEffect => "no_effect",
        }
    }

    /// The notice (English, translated where it is shown).
    pub(super) fn message(self) -> &'static str {
        match self {
            Reason::Overlay => "An open menu took that click. Press Esc to close it.",
            Reason::Dialog => "A dialog is open, so the tool waits. Close it to go on.",
            Reason::Transform => "Free Transform took that click. Press Enter to apply it or Esc to cancel it.",
            Reason::HandKey => "The Space key seems stuck, so the canvas pans. Tap Space to release it.",
            Reason::ZoomKey => "A zoom key seems stuck, so the canvas zooms. Tap Space to release it.",
            Reason::MoveKey => "Ctrl or Command seems stuck, so the Move tool is active. Tap it to release it.",
            Reason::ToolKey => "A tool key seems stuck. Tap it to release it.",
            Reason::NoClick | Reason::NoDown | Reason::NoEffect if cfg!(target_arch = "wasm32") => {
                "That click did nothing with this tool. If it keeps happening, reload the page."
            }
            Reason::NoClick | Reason::NoDown | Reason::NoEffect => "That click did nothing with this tool. If it keeps happening, restart the app.",
        }
    }

    fn immediate(self) -> bool {
        matches!(self, Reason::HandKey | Reason::ZoomKey | Reason::MoveKey | Reason::ToolKey)
    }
}

/// What is known once a press has ended.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Facts {
    pub stale: Option<Reason>,
    /// The press ran a command, moved the view, opened or said something, or changed the tool.
    pub acted: bool,
    /// It closed the overlay it landed on: that was its whole job.
    pub dismissed: bool,
    pub overlay_open: bool,
    pub dialog: bool,
    pub transform: bool,
    pub recognised: bool,
    pub reached: bool,
}

/// Why this press came to nothing, if it did.
pub(super) fn classify(f: Facts) -> Option<Reason> {
    if f.stale.is_some() {
        return f.stale;
    }
    if f.acted || f.dismissed {
        return None;
    }
    Some(if f.overlay_open {
        Reason::Overlay
    } else if f.dialog {
        Reason::Dialog
    } else if f.transform {
        Reason::Transform
    } else if !f.recognised {
        Reason::NoClick
    } else if !f.reached {
        Reason::NoDown
    } else {
        Reason::NoEffect
    })
}

/// The tools followed. Their whole effect comes after the press, unlike the live painting tools.
fn watched(tool: Tool) -> bool {
    matches!(tool, Tool::Zoom | Tool::Remove | Tool::RemoveAi | Tool::BubbleTextRemover)
}

/// What this frame's canvas knows ahead of the tools.
pub(super) struct Before {
    pub temporary: Option<Temporary>,
    /// `overlay_guard` (or a like dismissal) took this frame's pointer input.
    pub dismissal: bool,
    pub zoom: f32,
    pub center: [f32; 2],
}

#[derive(Clone, Debug)]
struct Press {
    tool: Tool,
    stale: Option<Reason>,
    dismissal: bool,
    command: Option<f64>,
    zoom: f32,
    center: [f32; 2],
    status: String,
    dialogs: usize,
    notice: Option<u64>,
    downs: u64,
    recognised: bool,
}

#[derive(Clone, Debug, Default)]
struct Watch {
    press: Option<Press>,
    /// The temporary tool in effect and when it began (input time).
    temporary: Option<(Temporary, f64)>,
    /// The last press's drop and how many in a row.
    streak: Option<(Reason, u32)>,
}

fn id() -> egui::Id {
    egui::Id::unique("pc-press-watch")
}

fn stale_key(app: &PhotocraftApp, temporary: Option<(Temporary, f64)>, now: f64) -> Option<Reason> {
    if let Some((t, since)) = temporary
        && now - since >= STALE_SECS
    {
        return Some(match t {
            Temporary::Hand => Reason::HandKey,
            Temporary::ZoomIn | Temporary::ZoomOut => Reason::ZoomKey,
            Temporary::Move => Reason::MoveKey,
        });
    }
    app.ui.spring.as_ref().filter(|s| !s.released && now - s.since >= STALE_SECS).map(|_| Reason::ToolKey)
}

/// Ahead of the tools: a press on the canvas starts being followed, as things stand before it.
pub(super) fn before(app: &PhotocraftApp, ctx: &egui::Context, response: &egui::Response, c: &Before) {
    let (pressed, now) = ctx.input(|i| (i.pointer.primary_pressed(), i.time));
    let mut w = ctx.data(|d| d.get_temp::<Watch>(id())).unwrap_or_default();
    w.temporary = match (c.temporary, w.temporary) {
        (Some(t), Some((held, since))) if t == held => Some((t, since)),
        (Some(t), _) => Some((t, now)),
        (None, _) => None,
    };
    if pressed {
        let stale = stale_key(app, w.temporary, now);
        // A temporary tool held for a moment is what the user asked for, not the tool underneath.
        let follow = response.contains_pointer() && (stale.is_some() || (watched(app.ui.tool) && c.temporary.is_none()));
        w.press = follow.then(|| Press {
            tool: app.ui.tool,
            stale,
            dismissal: c.dismissal,
            command: app.last_command.as_ref().map(|(_, at)| *at),
            zoom: c.zoom,
            center: c.center,
            status: app.ui.status.clone(),
            dialogs: app.ui.dialogs.len(),
            notice: app.ui.notices.last().map(|n| n.id),
            downs: app.tool_downs,
            recognised: false,
        });
    }
    ctx.data_mut(|d| d.insert_temp(id(), w));
}

/// After the tools: a press whose button came up is judged.
pub(super) fn after(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    response: &egui::Response,
    under_dialog: bool,
    zoom: f32,
    center: [f32; 2],
    doc_size: [u32; 2],
) {
    let mut w = ctx.data(|d| d.get_temp::<Watch>(id())).unwrap_or_default();
    let Some(mut press) = w.press.take() else { return };
    press.recognised |= response.clicked() || response.drag_started();
    if ctx.input(|i| i.pointer.primary_down()) {
        w.press = Some(press);
        ctx.data_mut(|d| d.insert_temp(id(), w));
        return;
    }
    // Zoom at either end of its range has nothing to do on a click.
    let zoom_limit = press.tool == Tool::Zoom && [1, -1].iter().any(|dir| crate::zoom_levels::step(press.zoom, *dir, doc_size).to_bits() == press.zoom.to_bits());
    let acted = zoom_limit
        || app.last_command.as_ref().map(|(_, at)| *at) != press.command
        || zoom.to_bits() != press.zoom.to_bits()
        || center != press.center
        || app.ui.tool != press.tool
        || app.ui.dialogs.len() > press.dialogs
        || app.ui.status != press.status
        || app.ui.notices.last().map(|n| n.id) != press.notice
        || crate::remove_ai_ui::is_open(app);
    let overlay_open = crate::overlay_guard::overlay_open(app, ctx);
    let facts = Facts {
        stale: press.stale,
        acted,
        dismissed: press.dismissal && !overlay_open,
        overlay_open: press.dismissal && overlay_open,
        dialog: under_dialog,
        transform: app.ui.transform.is_some(),
        recognised: press.recognised,
        // The Zoom tool works the view itself, without tool events.
        reached: press.tool == Tool::Zoom || app.tool_downs != press.downs,
    };
    match classify(facts) {
        Some(reason) => report(app, &mut w, reason, press.tool),
        None => {
            w.streak = None;
            app.ui.notices.retain(|n| n.tag.as_deref() != Some(NOTICE_TAG));
        }
    }
    ctx.data_mut(|d| d.insert_temp(id(), w));
}

fn report(app: &mut PhotocraftApp, w: &mut Watch, reason: Reason, tool: Tool) {
    crate::telemetry::with(|r| r.tool_drop(reason.code(), &crate::telemetry::tool_id(tool)));
    let streak = match w.streak {
        Some((last, n)) if last == reason => n.saturating_add(1),
        _ => 1,
    };
    w.streak = Some((reason, streak));
    if !reason.immediate() && streak < 2 {
        return;
    }
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(NOTICE_TAG));
    let notice = crate::notices::post(app, reason.message(), Vec::new(), true, None);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == notice) {
        n.tag = Some(NOTICE_TAG.into());
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, Key, Modifiers, PointerButton, vec2};
    use egui_kittest::Harness;
    use serde_json::json;

    use super::*;
    use crate::canvas::ViewXform;

    const DT: f32 = 1.0 / 10.0;

    fn harness(tool: Tool) -> Harness<'static, PhotocraftApp> {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
        app.sync_views();
        app.ui.extras.rulers = false;
        app.ui.tool = tool;
        let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).with_step_dt(DT).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                crate::shortcuts::handle(app, &ctx);
                egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        let v = &mut h.state_mut().ui.views[0];
        v.zoom = 1.0;
        v.center = [200.0, 150.0];
        v.fit_pending = false;
        h.run_steps(2);
        h
    }

    fn click(h: &mut Harness<'static, PhotocraftApp>) {
        let app = h.state();
        let v = &app.ui.views[0];
        let xf = ViewXform {
            rect: crate::rulers::content_rect(app, app.last_canvas_rect),
            zoom: v.zoom,
            center: v.center,
            flip: app.ui.view.flip_horizontal,
            rotation: v.rotation,
            aspect: app.ui.view.display_aspect(),
        };
        let p = xf.to_screen(200.0, 150.0);
        h.event(Event::PointerMoved(p));
        h.run_steps(1);
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.run_steps(1);
        }
        h.run_steps(1);
    }

    fn drop_notice(h: &Harness<'static, PhotocraftApp>) -> Option<String> {
        h.state().ui.notices.iter().find(|n| n.tag.as_deref() == Some(NOTICE_TAG)).map(|n| n.title.clone())
    }

    #[test]
    fn a_press_that_acted_is_no_drop() {
        assert_eq!(classify(Facts { acted: true, recognised: true, reached: true, ..Facts::default() }), None);
    }

    #[test]
    fn a_dismissing_press_is_no_drop_but_an_overlay_that_stays_is() {
        assert_eq!(classify(Facts { dismissed: true, ..Facts::default() }), None);
        assert_eq!(classify(Facts { overlay_open: true, ..Facts::default() }), Some(Reason::Overlay));
    }

    #[test]
    fn reasons_go_from_the_outermost_cause_in() {
        assert_eq!(classify(Facts { dialog: true, overlay_open: true, ..Facts::default() }), Some(Reason::Overlay));
        assert_eq!(classify(Facts { dialog: true, transform: true, ..Facts::default() }), Some(Reason::Dialog));
        assert_eq!(classify(Facts { transform: true, recognised: true, reached: true, ..Facts::default() }), Some(Reason::Transform));
        assert_eq!(classify(Facts::default()), Some(Reason::NoClick));
        assert_eq!(classify(Facts { recognised: true, ..Facts::default() }), Some(Reason::NoDown));
        assert_eq!(classify(Facts { recognised: true, reached: true, ..Facts::default() }), Some(Reason::NoEffect));
    }

    #[test]
    fn a_stuck_key_counts_even_though_the_press_panned() {
        assert_eq!(classify(Facts { stale: Some(Reason::HandKey), acted: true, ..Facts::default() }), Some(Reason::HandKey));
    }

    #[test]
    fn codes_are_collector_safe_and_distinct() {
        let all = [
            Reason::Overlay,
            Reason::Dialog,
            Reason::Transform,
            Reason::HandKey,
            Reason::ZoomKey,
            Reason::MoveKey,
            Reason::ToolKey,
            Reason::NoClick,
            Reason::NoDown,
            Reason::NoEffect,
        ];
        let codes: std::collections::BTreeSet<_> = all.iter().map(|r| r.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert!(codes.iter().all(|c| c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')), "{codes:?}");
    }

    #[test]
    fn messages_are_translated() {
        let catalogs = [include_str!("../i18n/es.tsv"), include_str!("../i18n/pt-br.tsv"), include_str!("../i18n/pt-pt.tsv")];
        for r in [Reason::Overlay, Reason::Dialog, Reason::Transform, Reason::HandKey, Reason::ZoomKey, Reason::MoveKey, Reason::ToolKey, Reason::NoEffect] {
            for c in catalogs {
                assert!(c.lines().any(|l| l.split('\t').nth(1) == Some(r.message())), "{:?}", r.message());
            }
        }
    }

    #[test]
    fn a_zoom_click_is_no_drop() {
        let mut h = harness(Tool::Zoom);
        click(&mut h);
        click(&mut h);
        assert_ne!(h.state().ui.views[0].zoom, 1.0, "the clicks zoomed");
        assert_eq!(drop_notice(&h), None);
    }

    #[test]
    fn a_bubble_click_says_only_its_own_refusal() {
        let mut h = harness(Tool::BubbleTextRemover);
        click(&mut h);
        click(&mut h);
        assert_eq!(drop_notice(&h), None);
    }

    #[test]
    fn a_space_held_for_long_is_doubted_at_the_press_and_blur_releases_it() {
        let mut h = harness(Tool::Zoom);
        h.event(Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(1);
        // Short of the threshold it is just the Hand.
        click(&mut h);
        assert_eq!(drop_notice(&h), None);
        h.run_steps((STALE_SECS / f64::from(DT)).ceil() as usize + 2);
        click(&mut h);
        assert_eq!(drop_notice(&h).as_deref(), Some(Reason::HandKey.message()));
        // The window losing focus forgets the key: the Zoom tool is back.
        h.event(Event::WindowFocused(false));
        h.run_steps(2);
        let ctx = h.ctx.clone();
        assert_eq!(crate::hold_keys::held_tool(h.state(), &ctx), None);
        let z = h.state().ui.views[0].zoom;
        click(&mut h);
        assert_ne!(h.state().ui.views[0].zoom, z, "the Zoom tool clicks again");
        assert_eq!(drop_notice(&h), None, "a press that worked takes the notice away");
    }
}
