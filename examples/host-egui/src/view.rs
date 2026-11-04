//! The one place the example touches the canvas widget (`capishot_host::view`, request #31): how a
//! host creates `CapishotView` on its own wgpu device, draws it in a rect it chooses, and paints
//! its overlays in document coordinates. Everything else in the example uses the engine API.

use capishot_host::Host;
use capishot_host::view::CapishotView;

/// The canvas plus what the host draws over it.
pub struct Canvas {
    view: CapishotView,
}

impl Canvas {
    /// The widget on the host's wgpu device (eframe's render state: one device for the whole app).
    /// Installs the widget's show/prewarm hooks on `host`. Changes no global egui style or fonts.
    pub fn new(cc: &eframe::CreationContext<'_>, host: &mut Host) -> capishot_host::Result<Self> {
        let view = CapishotView::new(&cc.egui_ctx, cc.wgpu_render_state.as_ref(), host)?;
        Ok(Canvas { view })
    }

    /// Draw the shown document in all of `ui`'s remaining space, then the host's overlay: a box
    /// around `highlight` (`[x, y, width, height]` in document pixels).
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &mut Host, highlight: Option<[f32; 4]>) -> egui::Response {
        let response = self.view.ui(ui, host);
        if let Some([x, y, w, h]) = highlight {
            let a = self.view.doc_to_screen(egui::pos2(x, y));
            let b = self.view.doc_to_screen(egui::pos2(x + w, y + h));
            let stroke = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
            // Clipped to the canvas: the overlay never draws over the host's panels.
            ui.painter_at(response.rect).rect_stroke(egui::Rect::from_two_pos(a, b), 0.0, stroke, egui::StrokeKind::Outside);
        }
        response
    }

    /// Zoom so the whole document fits the canvas.
    pub fn fit(&mut self) {
        self.view.fit();
    }
}
