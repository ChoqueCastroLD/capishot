//! Bubble geometry types shared by placement (WP1) and line shaping (WP4).
//!
//! WP0 only defines the data; the algorithms (opened selection bounds P7, wand detection D1/D2,
//! the row sampler D3, the neck split D6) land in WP1/WP4 as pure functions on masks here.

use serde::{Deserialize, Serialize};

/// An axis-aligned rectangle in document pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn area(&self) -> f64 {
        self.width.max(0.0) * self.height.max(0.0)
    }
}

/// Selections (opened) smaller than this many pixels are refused (`smallSelection`).
pub const MIN_AREA: f64 = 200.0;

/// One scanline band of a bubble outline, normalized to the bubble's bounding box (0..1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub y: f64,
    pub left: f64,
    pub right: f64,
    pub width: f64,
}

/// A detected bubble (`typer.detectBubble`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleHit {
    pub found: bool,
    /// Bounds of the raw wand selection.
    pub bbox: Rect,
    /// Bounds of the opened shape (tail removed); its centre is where text is centred.
    pub opened: Rect,
    pub center: (f64, f64),
    /// Outline rows for TextShapeR.
    pub rows: Vec<Row>,
    /// Selected pixels.
    pub area: f64,
}

/// A multi-bubble stored selection (M1): bounds plus the line and style captured with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSelection {
    pub bounds: Rect,
    pub line_index: Option<usize>,
    pub style_id: Option<crate::Id>,
}
