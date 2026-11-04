//! Host overlays above the canvas, in document coordinates.

use egui::{Painter, Pos2, Rect, Shape, Stroke};
use photocraft_ui_egui::canvas::ViewXform;

use crate::DocumentId;

/// Document ↔ screen mapping of the canvas as it was drawn this frame. Screen positions are egui
/// points in the host's context; document positions are document pixels (`[x, y]`, `0,0` the
/// top-left corner). Accounts for zoom, pan, rotation, View › Flip Horizontal and pixel aspect.
#[derive(Clone, Copy, Debug)]
pub struct CanvasTransform {
    xf: ViewXform,
    document: DocumentId,
    size: [u32; 2],
}

impl CanvasTransform {
    pub(crate) fn new(xf: ViewXform, document: DocumentId, size: [u32; 2]) -> Self {
        CanvasTransform { xf, document, size }
    }

    /// The document drawn.
    pub fn document(&self) -> DocumentId {
        self.document
    }

    /// Its size in document pixels.
    pub fn document_size(&self) -> [u32; 2] {
        self.size
    }

    /// The canvas area (inside the rulers when they show), in screen points.
    pub fn canvas_rect(&self) -> Rect {
        self.xf.rect
    }

    /// Screen points per document pixel (vertically; a non-square pixel aspect stretches x).
    pub fn zoom(&self) -> f32 {
        self.xf.zoom
    }

    /// The view's rotation in degrees (clockwise positive).
    pub fn rotation(&self) -> f32 {
        self.xf.rotation
    }

    /// Where document point `p` is on screen.
    pub fn to_screen(&self, p: [f64; 2]) -> Pos2 {
        self.xf.to_screen(p[0] as f32, p[1] as f32)
    }

    /// The document point under screen position `p` (may be outside the document).
    pub fn to_doc(&self, p: Pos2) -> [f64; 2] {
        self.xf.to_doc(p)
    }

    /// The document point under `p`, `None` outside the canvas area.
    pub fn doc_at(&self, p: Pos2) -> Option<[f64; 2]> {
        self.xf.rect.contains(p).then(|| self.xf.to_doc(p))
    }

    /// The four screen corners of document rect `[x, y, width, height]`, clockwise from its
    /// top-left (a rotated view turns them).
    pub fn doc_quad(&self, r: [f64; 4]) -> [Pos2; 4] {
        let [x, y, w, h] = r;
        [self.to_screen([x, y]), self.to_screen([x + w, y]), self.to_screen([x + w, y + h]), self.to_screen([x, y + h])]
    }

    /// The screen bounds of document rect `[x, y, width, height]`.
    pub fn doc_rect(&self, r: [f64; 4]) -> Rect {
        let q = self.doc_quad(r);
        Rect::from_points(&q)
    }

    /// The image's on-screen bounds.
    pub fn image_rect(&self) -> Rect {
        self.xf.image_bounds(self.size)
    }
}

/// A painter over the canvas (clipped to it, above the document and the editor's own canvas
/// overlays) with the canvas mapping: hosts draw their item boxes, links and other people's
/// cursors here, in document coordinates.
#[derive(Clone)]
pub struct Overlay {
    painter: Painter,
    transform: CanvasTransform,
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overlay").field("transform", &self.transform).finish()
    }
}

impl Overlay {
    pub(crate) fn new(painter: Painter, transform: CanvasTransform) -> Self {
        Overlay { painter, transform }
    }

    /// The painter (screen coordinates, clipped to the canvas).
    pub fn painter(&self) -> &Painter {
        &self.painter
    }

    /// The canvas mapping.
    pub fn transform(&self) -> &CanvasTransform {
        &self.transform
    }

    /// Outline document rect `[x, y, width, height]` (turned with the view).
    pub fn doc_rect_stroke(&self, r: [f64; 4], stroke: impl Into<Stroke>) {
        let q = self.transform.doc_quad(r);
        self.painter.add(Shape::closed_line(q.to_vec(), stroke.into()));
    }

    /// Fill document rect `[x, y, width, height]` (turned with the view).
    pub fn doc_rect_filled(&self, r: [f64; 4], fill: egui::Color32) {
        let q = self.transform.doc_quad(r);
        self.painter.add(Shape::convex_polygon(q.to_vec(), fill, Stroke::NONE));
    }

    /// A line between two document points.
    pub fn doc_line(&self, a: [f64; 2], b: [f64; 2], stroke: impl Into<Stroke>) {
        self.painter.line_segment([self.transform.to_screen(a), self.transform.to_screen(b)], stroke);
    }

    /// A circle of `radius` screen points at document point `at` (a cursor, a pin).
    pub fn doc_circle(&self, at: [f64; 2], radius: f32, fill: egui::Color32, stroke: impl Into<Stroke>) {
        self.painter.circle(self.transform.to_screen(at), radius, fill, stroke);
    }

    /// Text at document point `at` in screen-sized type (`font` from the host's fonts).
    pub fn doc_text(&self, at: [f64; 2], anchor: egui::Align2, text: impl ToString, font: egui::FontId, color: egui::Color32) -> Rect {
        self.painter.text(self.transform.to_screen(at), anchor, text, font, color)
    }
}
