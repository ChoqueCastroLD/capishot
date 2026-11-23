//! Waking the window from a worker when what it made is ready.
//!
//! On the desktop a worker asks egui for a frame. In the browser it must not: egui's context and
//! its repaint callback belong to the page, and a worker that takes the context's lock while a
//! frame holds it has to park, which the browser build's lock can't do ("Parking not supported on
//! this platform": the panic aborts the whole page). There the frame that finds the work still
//! running asks for another frame shortly after ([`poll`]), and that frame picks the result up.

/// What a worker carries to wake the window: the context on the desktop, nothing in the browser.
#[derive(Clone)]
pub(crate) struct WindowWake(#[cfg(not(target_arch = "wasm32"))] egui::Context);

impl WindowWake {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        Self(ctx.clone())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn new(_: &egui::Context) -> Self {
        Self()
    }

    /// Ask for a frame (from any thread). In the browser the waiting frame polls instead.
    pub(crate) fn wake(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.0.request_repaint();
    }
}

/// Called by a frame that leaves work running on a worker: in the browser, come back in a moment
/// to look for its result (the worker can't wake the page). Nothing on the desktop, where the
/// worker wakes the window itself.
pub(crate) fn poll(ctx: &egui::Context) {
    #[cfg(target_arch = "wasm32")]
    ctx.request_repaint_after(std::time::Duration::from_millis(16));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = ctx;
}
