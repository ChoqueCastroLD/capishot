//! The one place the example touches the canvas widget (`capishot_host::view`, request #31): how a
//! host creates `CapishotView` on its own wgpu device, draws it in a rect it chooses, and paints
//! its overlays in document coordinates. Everything else in the example uses the engine API.

use capishot_host::view::{CameraChange, CapishotView, Keys, Mode, Shortcut, ViewOutput};
use capishot_host::{DocumentId, Host};

/// The canvas plus what the host draws over it.
pub struct Canvas {
    view: CapishotView,
}

impl Canvas {
    /// The widget on the host's wgpu device (eframe's render state: one device for the whole app).
    /// Its show/prewarm hooks go on the host at the first frame. Changes no global egui style or
    /// fonts.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // The Type tool's IME hooks, from the first frame on.
        CapishotView::install(&cc.egui_ctx);
        let mut view = CapishotView::new(cc.wgpu_render_state.clone());
        // This host keeps the digit keys for itself (Photoshop's opacity keys clash with them).
        view.options_mut().keys = Keys::deny(Shortcut::opacity_digits());
        Canvas { view }
    }

    /// The canvas alone or Capishot's whole interface, on the same open documents.
    pub fn set_mode(&mut self, mode: Mode) {
        self.view.set_mode(mode);
    }

    /// The mode drawn.
    pub fn mode(&self) -> Mode {
        self.view.mode()
    }

    /// Whether Capishot gets the digit keys (Photoshop's opacity keys) or the host keeps them.
    pub fn set_digit_keys(&mut self, capishot: bool) {
        self.view.options_mut().keys = if capishot { Keys::default() } else { Keys::deny(Shortcut::opacity_digits()) };
    }

    /// Draw the shown document in all of `ui`'s remaining space, then the host's overlay: a box
    /// around `highlight` (`[x, y, width, height]` in document pixels). The view polls the host:
    /// its events reach the host's subscribers.
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &mut Host, highlight: Option<[f32; 4]>) -> ViewOutput {
        let out = self.view.ui(ui, host);
        if let (Some(overlay), Some([x, y, w, h])) = (out.overlay(), highlight) {
            let stroke = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
            // The overlay's painter is clipped to the canvas: it never draws over the host's panels.
            overlay.doc_rect_stroke([f64::from(x), f64::from(y), f64::from(w), f64::from(h)], stroke);
        }
        out
    }

    /// Zoom so the whole document fits the canvas (next frame).
    pub fn fit(&mut self, doc: DocumentId) {
        self.view.set_camera(doc, CameraChange::Fit);
    }

    /// The widget draws with the GPU (eframe's wgpu device), not the CPU fallback.
    pub fn gpu_active(&self) -> bool {
        self.view.gpu_active()
    }
}
