//! The editor drawn inside another app's window (`capishot-host`, embedded or full mode): what
//! would act on the window (close, title, maximize, full screen, focus, size, move) is collected
//! for the host instead of reaching the OS window, which is the host's.
//!
//! While a [`Hosted`] guard lives, [`send`] queues window commands for the host and passes the
//! rest (clipboard requests, screenshots, the pointer's position, IME) to egui as usual. Without
//! one (the standalone app) every command goes to egui.

use std::cell::RefCell;

thread_local! {
    /// The window commands of the hosted frame under way (`None`: not hosted).
    static SINK: RefCell<Option<Vec<egui::ViewportCommand>>> = const { RefCell::new(None) };
}

/// While it lives, this thread's window commands go to the host. Restores the previous state when
/// dropped (also on a panic); [`Hosted::finish`] returns what was collected.
pub struct Hosted {
    /// The state before ([`None`] once [`Hosted::finish`] put it back).
    previous: Option<Option<Vec<egui::ViewportCommand>>>,
    _fonts: crate::theme::HostFonts,
}

impl Hosted {
    /// Start a hosted frame: window commands are collected, and the editor draws with the host's
    /// fonts (it never replaces the context's fonts).
    pub fn enter() -> Self {
        let previous = SINK.with(|s| s.replace(Some(Vec::new())));
        Hosted { previous: Some(previous), _fonts: crate::theme::HostFonts::enter() }
    }

    /// End the frame: the window commands it sent, oldest first.
    pub fn finish(mut self) -> Vec<egui::ViewportCommand> {
        let previous = self.previous.take().flatten();
        SINK.with(|s| s.replace(previous)).unwrap_or_default()
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        // `finish` already put the previous state back.
        if let Some(previous) = self.previous.take() {
            SINK.with(|s| s.replace(previous));
        }
    }
}

/// Whether this thread is drawing a hosted frame.
pub fn active() -> bool {
    SINK.with(|s| s.borrow().is_some())
}

/// Commands that act on the editor's own drawing or the clipboard, not on the window: they reach
/// egui (the host's integration carries them out) also when hosted.
fn passes_through(cmd: &egui::ViewportCommand) -> bool {
    use egui::ViewportCommand as C;
    matches!(
        cmd,
        C::RequestCut
            | C::RequestCopy
            | C::RequestPaste
            | C::Screenshot(_)
            | C::CursorPosition(_)
            | C::CursorGrab(_)
            | C::CursorVisible(_)
            | C::IMEAllowed(_)
            | C::IMERect(_)
            | C::IMEPurpose(_)
    )
}

/// `ctx.send_viewport_cmd(cmd)`, except that a window command in a hosted frame goes to the host.
pub fn send(ctx: &egui::Context, cmd: egui::ViewportCommand) {
    if passes_through(&cmd) {
        ctx.send_viewport_cmd(cmd);
        return;
    }
    let rest = SINK.with(|s| match s.borrow_mut().as_mut() {
        Some(queue) => {
            queue.push(cmd);
            None
        }
        None => Some(cmd),
    });
    if let Some(cmd) = rest {
        ctx.send_viewport_cmd(cmd);
    }
}

/// `ctx.send_viewport_cmd_to(id, cmd)` for the root window: the host's when hosted.
pub fn send_to(ctx: &egui::Context, id: egui::ViewportId, cmd: egui::ViewportCommand) {
    if id == egui::ViewportId::ROOT && active() && !passes_through(&cmd) {
        send(ctx, cmd);
    } else {
        ctx.send_viewport_cmd_to(id, cmd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::ViewportCommand as C;

    #[test]
    fn window_commands_go_to_the_host_and_the_rest_to_egui() {
        let ctx = egui::Context::default();
        assert!(!active());
        let h = Hosted::enter();
        assert!(active());
        send(&ctx, C::Title("x".into()));
        send(&ctx, C::Close);
        send(&ctx, C::RequestPaste);
        let got = h.finish();
        assert_eq!(got, vec![C::Title("x".into()), C::Close]);
        assert!(!active());
    }

    #[test]
    fn nested_and_dropped_guards_restore_the_state() {
        {
            let _outer = Hosted::enter();
            {
                let _inner = Hosted::enter();
            }
            assert!(active(), "the outer frame is still hosted");
            let inner = Hosted::enter();
            assert!(inner.finish().is_empty());
            assert!(active(), "finish restores the outer frame too");
        }
        assert!(!active());
    }
}
