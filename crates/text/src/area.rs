//! Type in a path (Photoshop's area type in a shape, [`TextShape::InPath`]): the closed path as
//! polygons, and for each line band the span of the path it fits in.
//!
//! A line's glyphs fill the band from its baseline minus the ascent to its baseline plus the
//! descent; the line may use the part of the path that is inside it across the whole band (the
//! inside intervals at the band's top, middle and bottom, intersected). Where the path has several
//! intervals at that height (a concave balloon), the widest common one is used. Lines are placed
//! from the path's top down, a leading apart; a band too narrow for text is skipped.

use photocraft_doc::text::TextShape;
use photocraft_doc::vector::Subpath;

/// The flattened closed path a type layer's lines wrap inside.
#[derive(Clone, Debug)]
pub struct Area {
    /// Polygon edges `(x0, y0, x1, y1)`.
    edges: Vec<(f32, f32, f32, f32)>,
    /// Bounds: left, top, right, bottom.
    pub bounds: [f32; 4],
}

/// Line segments per cubic segment when flattening.
const STEPS: usize = 24;

impl Area {
    /// The area of `shape` when it is type in a path.
    pub fn of(shape: &TextShape) -> Option<Self> {
        match shape {
            TextShape::InPath { path } => Self::new(path),
            _ => None,
        }
    }

    /// The closed `path` (an open one is closed by its last segment back to the start), `None`
    /// when it has no area.
    pub fn new(path: &Subpath) -> Option<Self> {
        let closed = Subpath { closed: true, ..path.clone() };
        let mut pts: Vec<(f32, f32)> = Vec::new();
        for [p0, c1, c2, p1] in closed.segments() {
            for i in 0..STEPS {
                let t = i as f64 / STEPS as f64;
                let u = 1.0 - t;
                let x = u * u * u * p0.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * p1.x;
                let y = u * u * u * p0.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * p1.y;
                pts.push((x as f32, y as f32));
            }
        }
        if pts.len() < 3 || pts.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
            return None;
        }
        let mut bounds = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for &(x, y) in &pts {
            bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
        }
        if bounds[2] - bounds[0] < 1.0 || bounds[3] - bounds[1] < 1.0 {
            return None;
        }
        let edges = pts.iter().zip(pts.iter().cycle().skip(1)).map(|(a, b)| (a.0, a.1, b.0, b.1)).collect();
        Some(Area { edges, bounds })
    }

    /// The inside intervals of the path at height `y` (even-odd), left to right.
    fn intervals(&self, y: f32) -> Vec<(f32, f32)> {
        let mut xs: Vec<f32> = self
            .edges
            .iter()
            .filter(|(_, y0, _, y1)| (*y0 <= y && y < *y1) || (*y1 <= y && y < *y0))
            .map(|(x0, y0, x1, y1)| x0 + (y - y0) / (y1 - y0) * (x1 - x0))
            .collect();
        xs.sort_by(f32::total_cmp);
        xs.as_chunks::<2>().0.iter().map(|c| (c[0], c[1])).collect()
    }

    /// The widest horizontal span inside the path across the whole band `top..bottom`, if any.
    pub fn span(&self, top: f32, bottom: f32) -> Option<(f32, f32)> {
        let samples = [top, (top + bottom) * 0.5, bottom];
        let mut common: Vec<(f32, f32)> = self.intervals(samples[0]);
        for &y in &samples[1..] {
            let next = self.intervals(y);
            common = common
                .iter()
                .flat_map(|a| next.iter().map(move |b| (a.0.max(b.0), a.1.min(b.1))))
                .filter(|(l, r)| r > l)
                .collect();
        }
        common.into_iter().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
    }
}

/// One line's place inside the area: its baseline, left edge and width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub baseline: f32,
    pub x: f32,
    pub width: f32,
}

/// Lines placed top to bottom inside an [`Area`] ([`Slots::next`]).
#[derive(Clone, Debug)]
pub struct Slots {
    /// Where the next line's band may start (its top).
    pub top: f32,
    /// The previous line's baseline.
    pub last: Option<f32>,
}

impl Slots {
    pub fn new(area: &Area) -> Self {
        Slots { top: area.bounds[1], last: None }
    }

    /// The next line: `ascent`/`descent` of its glyphs, `leading` from the previous baseline,
    /// `extra` space before it (paragraph spacing), and the narrowest `min_width` it may have.
    /// `None` once no band fits above the path's bottom (overflow).
    pub fn next(&mut self, area: &Area, ascent: f32, descent: f32, leading: f32, extra: f32, min_width: f32) -> Option<Slot> {
        let mut baseline = match self.last {
            Some(b) => b + leading.max(1.0) + extra,
            None => self.top + ascent + extra,
        };
        let step = (leading.max(1.0) * 0.25).max(1.0);
        while baseline + descent <= area.bounds[3] + 0.5 {
            if let Some((l, r)) = area.span(baseline - ascent, baseline + descent)
                && r - l >= min_width
            {
                self.last = Some(baseline);
                return Some(Slot { baseline, x: l, width: r - l });
            }
            baseline += step;
        }
        None
    }
}

/// An ellipse of radii `rx`, `ry` centred on (`cx`, `cy`), as four Bézier quarters (tests).
#[cfg(test)]
pub(crate) fn tests_ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Subpath {
    const K: f64 = 0.552_284_75;
    let knot = |x: f64, y: f64, dx: f64, dy: f64| photocraft_doc::vector::Knot {
        anchor: photocraft_geom::Point::new(x, y),
        in_ctrl: photocraft_geom::Point::new(x - dx, y - dy),
        out_ctrl: photocraft_geom::Point::new(x + dx, y + dy),
        smooth: true,
    };
    Subpath {
        closed: true,
        knots: vec![
            knot(cx, cy - ry, rx * K, 0.0),
            knot(cx + rx, cy, 0.0, ry * K),
            knot(cx, cy + ry, -rx * K, 0.0),
            knot(cx - rx, cy, 0.0, -ry * K),
        ],
        op: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Subpath {
        super::tests_ellipse(cx, cy, rx, ry)
    }

    #[test]
    fn spans_follow_the_ellipse() {
        let a = Area::new(&ellipse(100.0, 100.0, 80.0, 50.0)).unwrap();
        let (l, r) = a.span(98.0, 102.0).unwrap();
        assert!((l - 20.0).abs() < 1.0 && (r - 180.0).abs() < 1.0, "{l} {r}");
        let narrow = a.span(55.0, 60.0).unwrap();
        assert!(narrow.1 - narrow.0 < 100.0, "{narrow:?}");
        assert!(a.span(40.0, 45.0).is_none(), "above the ellipse");
    }

    #[test]
    fn slots_skip_bands_too_narrow_and_stop_at_the_bottom() {
        let a = Area::new(&ellipse(100.0, 100.0, 80.0, 50.0)).unwrap();
        let mut s = Slots::new(&a);
        let mut widths = Vec::new();
        while let Some(slot) = s.next(&a, 12.0, 4.0, 18.0, 0.0, 30.0) {
            widths.push(slot.width);
        }
        assert!(widths.len() >= 4, "{widths:?}");
        assert!(widths.iter().all(|w| *w >= 30.0));
        let mid = widths.iter().copied().fold(0.0f32, f32::max);
        assert!(widths[0] < mid && widths[widths.len() - 1] < mid, "narrower at the top and bottom: {widths:?}");
    }
}
