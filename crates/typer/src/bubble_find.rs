//! The speech bubble under a click (the Bubble Type tool, `type.createInBubble`): the light
//! interior around the point, found on the page without its type layers.
//!
//! A balloon's outline is rarely closed: a thin stroke breaks, the tail meets the art, a
//! lettering pass leaves a gap. So the flood doesn't walk the light pixels themselves but those
//! with no dark pixel within [`GAP_RADIUS`] (a morphological close of the outline, computed only
//! where the flood goes): a gap narrower than twice the radius stays shut. The flooded core is
//! then grown back by the same radius, inside the light pixels, to reach the outline again.
//!
//! What isn't a bubble: a dark seed, a region touching the page edge (the page background, a
//! panel gutter), one larger than [`MAX_PAGE_SHARE`] of the page, one smaller than a line of text
//! fits ([`crate::bubble::MIN_SIDE`], [`crate::bubble::MIN_AREA`]), one whose edge mostly meets no
//! dark outline ([`crate::bubble::MIN_OUTLINE_DARK`]: a light patch of art), or one that fills
//! too little of its box. The body the text goes in drops the tail with TypeR's adaptive opening
//! ([`crate::bubble::opening_radius`]).
//!
//! The pixel sizes ([`GAP_RADIUS`], [`REACH`], [`WINDOWS`]) are a 1x page's (about 1500 px wide).
//! A page scanned or upscaled larger has thicker ink and wider gaps: [`Opts::auto`] measures the
//! ink's stroke width around the click ([`ink_scale`]) and scales them by it (resolution metadata
//! is often wrong and isn't read). [`Opts`] also sets them outright, and bounds the search to the
//! windows around the click (a miss then costs a window, not the page).

use photocraft_geom::Rect;

use crate::bubble::{self, Mask};

/// Outline gaps up to about twice this wide (px, at 1x) don't let the flood out.
pub const GAP_RADIUS: i32 = 2;
/// A light pixel is at most this much darker (luma, 0–255) than the clicked one.
pub const LIGHT_TOLERANCE: f64 = 60.0;
/// How far from the click (px, at 1x) a light pixel to start from is looked for.
pub const REACH: i32 = 24;
/// The darkest paper (luma) a bubble may have: a light grey tone still counts.
pub const MIN_PAPER: f64 = 96.0;
/// Starts tried at most (each on another side of a letter or the outline)…
const MAX_STARTS: usize = 6;
/// …at least this far (px, at 1x) from each other.
const SPREAD: i32 = 6;
/// The largest share of the page a bubble may cover.
pub const MAX_PAGE_SHARE: f64 = 0.4;
/// The flood stops after this many pixels (a bubble this large is the page, not a balloon).
pub const MAX_PIXELS: usize = 6_000_000;
/// Half the sides (px, at 1x) of the windows around the click searched first, smallest first.
pub const WINDOWS: [i32; 2] = [384, 1024];
/// The ink's stroke width (px) on a 1x page, as [`ink_scale`] measures it (lettering and outlines
/// about 3 px wide, crossed at an angle).
pub const STROKE_1X: f64 = 3.5;
/// A stroke width below this is a 1x page (bold lettering, a thick outline)…
pub const STROKE_SCALED: f64 = 5.0;
/// …or below this on a page whose short side is at least [`LARGE_PAGE`] px (a 1x manga page is
/// 1000–1700 px wide: on a larger one, ink this thick is more likely upscaled thin ink).
pub const STROKE_SCALED_LARGE: f64 = 4.0;
/// See [`STROKE_SCALED_LARGE`].
pub const LARGE_PAGE: i32 = 2200;
/// A bounded search's largest window is no wider than this share of the page's short side (where
/// that is more than a 1x window: a bubble is not wider than the page).
pub const MAX_WINDOW_SHARE: f64 = 0.7;
/// The largest scale against a 1x page.
pub const MAX_SCALE: f64 = 8.0;
/// The largest gap radius (px) a caller may ask for.
pub const MAX_GAP_RADIUS: i32 = 32;
/// The largest reach (px) a caller may ask for.
pub const MAX_REACH: i32 = 512;
/// A pixel darker than this (luma) is ink, for [`ink_scale`].
const INK: f64 = 128.0;
/// [`ink_scale`] needs at least this many ink runs to measure…
const MIN_RUNS: usize = 24;
/// …and ignores runs longer than this (px): a solid fill, a panel line along its length.
const MAX_RUN: u32 = 32;

/// Why there is no bubble at a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The point is outside the page or in a dark area.
    Dark,
    /// The light region reaches the page edge.
    Edge,
    /// Larger than a bubble can be.
    TooBig,
    /// Too small for a line of text.
    TooSmall,
    /// Light, closed and sized like a bubble, but without a bubble's outline or shape.
    NotBubble,
    /// The light region reaches past the largest search window (a bounded search,
    /// [`Opts::page`] false): the page background, a gutter, or a bubble larger than the window.
    Beyond,
}

impl Miss {
    pub fn code(self) -> &'static str {
        match self {
            Miss::Dark => "dark",
            Miss::Edge => "edge",
            Miss::TooBig => "tooBig",
            Miss::TooSmall => "tooSmall",
            Miss::NotBubble => "notBubble",
            Miss::Beyond => "beyondWindow",
        }
    }
}

/// How a search is sized (document pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opts {
    /// The page's scale against a 1x page (1 for one): spaces the starts.
    pub scale: f64,
    /// Outline gaps up to about twice this wide don't let the flood out.
    pub gap_radius: i32,
    /// How far from the click a light pixel to start from is looked for.
    pub reach: i32,
    /// Half the sides of the windows around the click searched, smallest first.
    pub windows: [i32; 2],
    /// After the windows, the whole page; when false, a region reaching past the last window is
    /// a [`Miss::Beyond`].
    pub page: bool,
}

impl Default for Opts {
    /// A 1x page's, the whole page searched last.
    fn default() -> Self {
        Self::scaled(1.0)
    }
}

impl Opts {
    /// The 1x sizes times `scale` (clamped to 1..[`MAX_SCALE`]), the whole page searched last.
    pub fn scaled(scale: f64) -> Self {
        let s = if scale.is_finite() { scale.clamp(1.0, MAX_SCALE) } else { 1.0 };
        let px = |v: i32| (f64::from(v) * s).round() as i32;
        Opts { scale: s, gap_radius: px(GAP_RADIUS), reach: px(REACH), windows: WINDOWS.map(px), page: true }
    }

    /// Sized for the ink around `seed` ([`ink_scale`]).
    pub fn auto(img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> Self {
        Self::scaled(ink_scale(img, area, seed))
    }

    /// [`Opts::auto`] on `img` covering `area`, a window of the page `page` (the same as on the
    /// page while the window holds [`INK_WINDOW`] px around `seed`).
    pub fn auto_in(img: &[[u8; 4]], area: Rect, page: Rect, seed: (i32, i32)) -> Self {
        Self::scaled(ink_scale_in(img, area, page, seed))
    }

    /// Bounded to the windows ([`Opts::page`] false), the largest no larger than a bubble on
    /// `page` can be: [`MAX_WINDOW_SHARE`] of its short side, or the 1x window.
    pub fn bounded(self, page: Rect) -> Self {
        let short = f64::from(page.width().min(page.height()));
        let half = ((short * MAX_WINDOW_SHARE / 2.0) as i32).max(WINDOWS[1]).min(self.windows[1]);
        Opts { page: false, ..self.with_window(half) }
    }

    /// The largest window's half side set to `half` (the first no larger).
    pub fn with_window(mut self, half: i32) -> Self {
        let half = half.clamp(16, 1 << 20);
        self.windows = [self.windows[0].min(half), half];
        self
    }

    /// Sizes in range (a caller's may be anything).
    fn checked(self) -> Self {
        let w1 = self.windows[1].clamp(16, 1 << 20);
        Opts {
            scale: if self.scale.is_finite() { self.scale.clamp(1.0, MAX_SCALE) } else { 1.0 },
            gap_radius: self.gap_radius.clamp(1, MAX_GAP_RADIUS),
            reach: self.reach.clamp(1, MAX_REACH),
            windows: [self.windows[0].clamp(16, w1), w1],
            page: self.page,
        }
    }

    fn spread(&self) -> i32 {
        ((f64::from(SPREAD) * self.scale).round() as i32).max(1)
    }

    /// A flood this close (px) to a side of the window that isn't the page's leaves the window:
    /// every pixel the search reads (the gap radius around the core, the interior grown back by
    /// it, the outline three pixels past that) stays inside it until then.
    fn margin(&self) -> i32 {
        self.gap_radius + 6
    }
}

/// A bubble found at a point.
#[derive(Clone, Debug)]
pub struct Found {
    /// The whole interior, tail included, holes (lettering) filled.
    pub interior: Mask,
    /// The body the text goes in: the interior without its tail.
    pub body: Mask,
}

fn luma(p: [u8; 4]) -> f64 {
    if p[3] < 128 { 0.0 } else { 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]) }
}

/// The page's scale against a 1x page, from the ink around `seed` on an 8-bit RGBA image covering
/// `area`: the median length of the ink's runs along rows and columns (each run once, so a stroke
/// counts by its width, not its length; solid fills ignored) over [`STROKE_1X`]. 1 when the ink is no thicker than a
/// 1x page's ([`STROKE_SCALED`]) or there is too little of it to measure.
pub fn ink_scale(img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> f64 {
    ink_scale_in(img, area, area, seed)
}

/// Half the side of the largest square around the click [`ink_scale`] reads.
pub const INK_WINDOW: i32 = 160;

/// [`ink_scale`] on `img` covering `area`, a window of the page `page` (whose size tells a large
/// page).
pub fn ink_scale_in(img: &[[u8; 4]], area: Rect, page: Rect, seed: (i32, i32)) -> f64 {
    let w = area.width() as usize;
    if w == 0 || img.len() != w * area.height() as usize || !area.contains(seed.0, seed.1) {
        return 1.0;
    }
    let ink = |x: i32, y: i32| img.get((y - area.y0) as usize * w + (x - area.x0) as usize).is_some_and(|p| luma(*p) < INK);
    // A run cut by the window's side isn't measured.
    let runs_of = |line: &mut dyn Iterator<Item = bool>, out: &mut Vec<u32>| {
        let (mut n, mut cut) = (0u32, true);
        for d in line {
            if d {
                n += 1;
            } else {
                if n > 0 && n <= MAX_RUN && !cut {
                    out.push(n);
                }
                (n, cut) = (0, false);
            }
        }
    };
    for half in [64, INK_WINDOW] {
        let win = Rect::new(seed.0 - half, seed.1 - half, seed.0 + half + 1, seed.1 + half + 1).intersect(&area);
        let mut runs: Vec<u32> = Vec::new();
        for y in win.y0..win.y1 {
            runs_of(&mut (win.x0..win.x1).map(|x| ink(x, y)), &mut runs);
        }
        for x in win.x0..win.x1 {
            runs_of(&mut (win.y0..win.y1).map(|y| ink(x, y)), &mut runs);
        }
        if runs.len() < MIN_RUNS {
            continue;
        }
        let k = runs.len() / 2;
        let stroke = f64::from(*runs.select_nth_unstable(k).1);
        let large = page.width().min(page.height()) >= LARGE_PAGE as u32;
        let threshold = if large { STROKE_SCALED_LARGE } else { STROKE_SCALED };
        return if stroke < threshold { 1.0 } else { (stroke / STROKE_1X).clamp(1.0, MAX_SCALE) };
    }
    1.0
}

/// The bubble around `seed` on an 8-bit RGBA image covering `area`. A click on lettering or on
/// the outline still finds it: the flood starts from the nearest light pixels around the click
/// (up to [`REACH`] px away), trying the next one when a start leads nowhere (the other side of
/// the outline). What counts as light is relative to the start, so a bubble lighter than a grey
/// or toned page is found as well as a white one. Sized for the ink there ([`Opts::auto`]).
pub fn find(img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> Result<Found, Miss> {
    find_with(img, area, seed, false, MAX_PAGE_SHARE, &Opts::auto(img, area, seed))
}

/// [`find`] for a bubble cut by the image's edge (it opens onto the page edge or a panel drawn to
/// it): the edge counts as the bubble's wall, so a light region reaching it is not refused as
/// touching it, and its stretch of edge counts as dark outline. Still refused when too large,
/// too small or without an outline elsewhere (the page background is still no bubble: it leaks
/// along the gutters to [`MAX_PAGE_SHARE`]).
pub fn find_walled(img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> Result<Found, Miss> {
    find_with(img, area, seed, true, MAX_PAGE_SHARE, &Opts::auto(img, area, seed))
}

/// [`find`] (or [`find_walled`] when `walled`) sized by `opts`.
pub fn find_opts(img: &[[u8; 4]], area: Rect, seed: (i32, i32), walled: bool, opts: &Opts) -> Result<Found, Miss> {
    find_with(img, area, seed, walled, MAX_PAGE_SHARE, opts)
}

/// [`find_walled`] inside a box known to hold one bubble (the lettering's box, when the bubble
/// opens onto the gutter): the box's sides are walls too, and the region may fill the box.
pub fn find_in_box(img: &[[u8; 4]], area: Rect, seed: (i32, i32)) -> Result<Found, Miss> {
    find_with(img, area, seed, true, 1.0, &Opts::auto(img, area, seed))
}

/// [`find`] given only `img` covering `win`, a window of the page `page`: the same result as on
/// the page, or `None` when the search reached a side of the window that isn't the page's (it
/// needs more of the page). A caller with a stale page composites only a window around a click.
pub fn find_in_window(img: &[[u8; 4]], win: Rect, page: Rect, seed: (i32, i32)) -> Option<Result<Found, Miss>> {
    let opts = Opts::auto_in(img, win.intersect(&page), page, seed);
    find_in_window_opts(img, win, page, seed, &opts)
}

/// [`find_in_window`] sized by `opts` (its windows unused: `win` is the one).
pub fn find_in_window_opts(img: &[[u8; 4]], win: Rect, page: Rect, seed: (i32, i32), opts: &Opts) -> Option<Result<Found, Miss>> {
    let win = win.intersect(&page);
    if win.is_empty() || img.len() != win.width() as usize * win.height() as usize || !win.contains(seed.0, seed.1) {
        return None;
    }
    search(img, win, win, page, seed, false, MAX_PAGE_SHARE, &opts.checked())
}

fn find_with(img: &[[u8; 4]], area: Rect, seed: (i32, i32), walls: bool, share: f64, opts: &Opts) -> Result<Found, Miss> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    if w == 0 || h == 0 || img.len() != w * h || !area.contains(seed.0, seed.1) {
        return Err(Miss::Dark);
    }
    let o = opts.checked();
    // A bubble is a few hundred pixels: look in a window around the click first (a phone spent
    // seconds per Bubble Text Remover click on a whole page's luma and flood buffers). A search
    // whose flood reaches the window's inner side runs again in the next window, then over the
    // page (unless bounded), so the result is the same as the page's.
    let mut last = None;
    for half in o.windows {
        let win = Rect::new(seed.0.saturating_sub(half), seed.1.saturating_sub(half), seed.0.saturating_add(half + 1), seed.1.saturating_add(half + 1)).intersect(&area);
        if win == area {
            return search(img, area, area, area, seed, walls, share, &o).unwrap_or(Err(Miss::TooBig));
        }
        if last == Some(win) {
            continue;
        }
        last = Some(win);
        if let Some(r) = search(img, area, win, area, seed, walls, share, &o) {
            return r;
        }
    }
    if !o.page {
        return Err(Miss::Beyond);
    }
    search(img, area, area, area, seed, walls, share, &o).unwrap_or(Err(Miss::TooBig))
}

/// [`find_with`] in the window `win` of `img` (covering `img_area`, itself `page` or a window of
/// it); `None` when a flood reached a side of the window that isn't the page's.
#[allow(clippy::too_many_arguments)]
fn search(img: &[[u8; 4]], img_area: Rect, win: Rect, page: Rect, seed: (i32, i32), walls: bool, share: f64, o: &Opts) -> Option<Result<Found, Miss>> {
    let Some(mut g) = Grid::new(img, img_area, win, walls) else { return Some(Err(Miss::Dark)) };
    // Light starts near the click, nearest first.
    let reach = o.reach;
    let mut starts: Vec<(i32, (i32, i32))> = Vec::new();
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let p = (seed.0 + dx, seed.1 + dy);
            if win.contains(p.0, p.1) && dx * dx + dy * dy <= reach * reach && f64::from(g.lum(p.0, p.1)) >= MIN_PAPER {
                starts.push((dx * dx + dy * dy, p));
            }
        }
    }
    starts.sort_by_key(|(d, _)| *d);
    let spread = o.spread();
    let mut tried: Vec<(i32, i32)> = Vec::new();
    let mut first_miss = None;
    for (_, start) in starts {
        if tried.len() >= MAX_STARTS {
            break;
        }
        if tried.iter().any(|t| (t.0 - start.0).pow(2) + (t.1 - start.1).pow(2) < spread * spread) {
            continue;
        }
        // The paper here: the lightest pixel right around the start.
        let mut paper = 0u8;
        for (dx, dy) in [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1), (-2, 0), (2, 0), (0, -2), (0, 2)] {
            if win.contains(start.0 + dx, start.1 + dy) {
                paper = paper.max(g.lum(start.0 + dx, start.1 + dy));
            }
        }
        let light_min = (f64::from(paper) - LIGHT_TOLERANCE.min(0.4 * f64::from(paper))).ceil().clamp(0.0, 255.0) as u8;
        // A start a missed flood already reached (the page background seen from another side of
        // a letter) misses the same way: tried, not flooded again.
        if g.set_light_min(light_min) && g.flooded(start) {
            tried.push(start);
            continue;
        }
        match flood_from(&mut g, page, start, walls, share, o)? {
            Ok(found) => return Some(Ok(found)),
            // Too dark to start here (a letter's edge): not a try.
            Err(Miss::Dark) => continue,
            Err(m) => {
                tried.push(start);
                first_miss.get_or_insert(m);
            }
        }
    }
    Some(Err(first_miss.unwrap_or(Miss::Dark)))
}

/// What a search reads of the window `win` of `img` (covering `img_area`), each row computed the
/// first time a flood comes near it (a flood is mostly far smaller than its window).
struct Grid<'a> {
    img: &'a [[u8; 4]],
    img_area: Rect,
    win: Rect,
    w: usize,
    /// The image's edge counts as dark (a walled search).
    walls: bool,
    /// Luma, one byte a pixel, where `lum_row` says it's computed.
    lum: Vec<u8>,
    lum_row: Vec<bool>,
    /// The luma weights times each channel value.
    tables: Box<[[f64; 256]; 3]>,
    /// The light threshold `near` and `state` are for.
    light_min: Option<u8>,
    /// Per pixel, where `near_row` says it's computed: how far (px, up to 255) along its row the
    /// nearest pixel darker than `light_min` is (0 for a dark one; the window's sides dark when
    /// walled, none otherwise).
    near: Vec<u8>,
    near_row: Vec<bool>,
    /// Per pixel: 0 not yet tested, 1 blocked, 2 in a flood's core (each pixel's disc is tested
    /// once, not once per neighbour).
    state: Vec<u8>,
}

impl<'a> Grid<'a> {
    fn new(img: &'a [[u8; 4]], img_area: Rect, win: Rect, walls: bool) -> Option<Self> {
        let (w, h) = (win.width() as usize, win.height() as usize);
        let n = w.checked_mul(h)?;
        if w == 0 || !img_area.contains_rect(&win) || img.len() != img_area.width() as usize * img_area.height() as usize {
            return None;
        }
        Some(Grid {
            img,
            img_area,
            win,
            w,
            walls,
            lum: vec![0; n],
            lum_row: vec![false; h],
            tables: Box::new(std::array::from_fn(|c| std::array::from_fn(|v| [0.299, 0.587, 0.114][c] * v as f64))),
            light_min: None,
            near: vec![0; n],
            near_row: vec![false; h],
            state: vec![0; n],
        })
    }

    fn at(&self, x: i32, y: i32) -> usize {
        (y - self.win.y0) as usize * self.w + (x - self.win.x0) as usize
    }

    /// Computes row `y`'s luma (inside the window).
    fn lum_row(&mut self, y: i32) {
        let r = (y - self.win.y0) as usize;
        if self.lum_row.get(r) != Some(&false) {
            return;
        }
        self.lum_row[r] = true;
        let iw = self.img_area.width() as usize;
        let s = (y - self.img_area.y0) as usize * iw + (self.win.x0 - self.img_area.x0) as usize;
        let (Some(src), Some(dst)) = (self.img.get(s..s + self.w), self.lum.get_mut(r * self.w..(r + 1) * self.w)) else { return };
        // `luma` rounded (half up: it is never negative), its products from tables.
        let t = &self.tables;
        for (d, p) in dst.iter_mut().zip(src) {
            let v = if p[3] < 128 { 0.0 } else { t[0][usize::from(p[0])] + t[1][usize::from(p[1])] + t[2][usize::from(p[2])] };
            *d = (v + 0.5) as u8;
        }
    }

    /// The luma at `(x, y)`, inside the window.
    fn lum(&mut self, x: i32, y: i32) -> u8 {
        self.lum_row(y);
        let k = self.at(x, y);
        self.lum.get(k).copied().unwrap_or(0)
    }

    /// Sets the light threshold; false (and the per-threshold rows forgotten) when it changed.
    fn set_light_min(&mut self, light_min: u8) -> bool {
        match self.light_min.replace(light_min) {
            Some(m) if m == light_min => true,
            None => false,
            Some(_) => {
                self.near_row.fill(false);
                self.state.fill(0);
                false
            }
        }
    }

    fn flooded(&self, p: (i32, i32)) -> bool {
        self.state.get(self.at(p.0, p.1)) == Some(&2)
    }

    /// Computes row `y`'s distances to the nearest dark pixel along it.
    fn near_row(&mut self, y: i32) {
        let r = (y - self.win.y0) as usize;
        if self.near_row.get(r) != Some(&false) {
            return;
        }
        self.lum_row(y);
        self.near_row[r] = true;
        let (w, min, walls) = (self.w, self.light_min.unwrap_or(0), self.walls);
        let (Some(lum), Some(near)) = (self.lum.get(r * w..(r + 1) * w), self.near.get_mut(r * w..(r + 1) * w)) else { return };
        // Left to right, then right to left; the window's side is a dark pixel just past it when
        // walled.
        let side: u32 = if walls { 0 } else { 255 };
        let mut d = side;
        for (n, l) in near.iter_mut().zip(lum) {
            d = if *l < min { 0 } else { (d + 1).min(255) };
            *n = d as u8;
        }
        let mut d = side;
        for (n, l) in near.iter_mut().zip(lum).rev() {
            d = if *l < min { 0 } else { (d + 1).min(255) };
            *n = (*n).min(d as u8);
        }
    }

    /// No pixel darker than the threshold within `r` of `(x, y)` (`span[dy + r]`: the disc's half
    /// width on row `dy`). Pixels off the window count as light, or dark when walled (a bubble cut
    /// by the page edge still reaches it in an open search, and is refused as touching it).
    fn passable(&mut self, x: i32, y: i32, r: i32, span: &[i32]) -> bool {
        for (dy, hw) in (-r..=r).zip(span) {
            let yy = y + dy;
            if yy < self.win.y0 || yy >= self.win.y1 {
                if self.walls {
                    return false;
                }
                continue;
            }
            self.near_row(yy);
            let k = self.at(x, yy);
            if self.near.get(k).is_none_or(|d| i32::from(*d) <= *hw) {
                return false;
            }
        }
        true
    }
}

/// The flood from `start` over pixels with no pixel darker than the grid's light threshold within
/// the gap radius (`None`: it reached a side of the window that isn't the `page`'s).
fn flood_from(g: &mut Grid, page: Rect, start: (i32, i32), walls: bool, share: f64, o: &Opts) -> Option<Result<Found, Miss>> {
    let area = g.win;
    let r = o.gap_radius;
    // The disc's half width on each of its rows.
    let span: Vec<i32> = (-r..=r).map(|dy| f64::from(r * r - dy * dy).sqrt().floor() as i32).collect();
    let m = o.margin();
    let leaves = |x: i32, y: i32| {
        (area.x0 > page.x0 && x - m <= area.x0)
            || (area.y0 > page.y0 && y - m <= area.y0)
            || (area.x1 < page.x1 && x + m >= area.x1 - 1)
            || (area.y1 < page.y1 && y + m >= area.y1 - 1)
    };
    if leaves(start.0, start.1) {
        return None;
    }
    if !g.passable(start.0, start.1, r, &span) {
        return Some(Err(Miss::Dark));
    }
    let seed = start;
    // The flood over passable pixels; the core stays `r` inside the page, so one that gets
    // there touches the edge.
    let page_px = page.width() as usize * page.height() as usize;
    let cap = MAX_PIXELS.min((page_px as f64 * share) as usize).max(1);
    let mut core: Vec<(i32, i32)> = vec![seed];
    let k = g.at(seed.0, seed.1);
    let Some(s) = g.state.get_mut(k) else { return Some(Err(Miss::Dark)) };
    *s = 2;
    let mut i = 0;
    let (mut x0, mut y0, mut x1, mut y1) = (seed.0, seed.1, seed.0, seed.1);
    while let Some(&(x, y)) = core.get(i) {
        i += 1;
        if !walls && (x - r <= page.x0 || y - r <= page.y0 || x + r >= page.x1 - 1 || y + r >= page.y1 - 1) {
            return Some(Err(Miss::Edge));
        }
        if leaves(x, y) {
            return None;
        }
        if core.len() > cap {
            return Some(Err(Miss::TooBig));
        }
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        for n in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            let j = g.at(n.0, n.1);
            if g.state.get(j) == Some(&0) {
                let pass = g.passable(n.0, n.1, r, &span);
                g.state[j] = if pass { 2 } else { 1 };
                if pass {
                    core.push(n);
                }
            }
        }
    }
    // Grow the core back by the radius, over light pixels, to the outline.
    let pad = r + 1;
    let (mx, my) = (x0 - pad, y0 - pad);
    let (mw, mh) = ((x1 - x0 + 1 + 2 * pad) as usize, (y1 - y0 + 1 + 2 * pad) as usize);
    // A box this large holds too much for a bubble or fills too little of it (the gutters around
    // a page's panels): refused before the box-sized work (the distance transform, the holes).
    if (mw * mh) as f64 * bubble::MIN_FILL > share * page_px as f64 {
        return Some(Err(Miss::TooBig));
    }
    let Some(mut interior) = Mask::new(mx, my, mw, mh) else { return Some(Err(Miss::TooBig)) };
    for &(x, y) in &core {
        interior.set(x, y, true);
    }
    let to_core = photocraft_algo::selection::edt(&interior.px, mw, mh);
    let light_min = g.light_min.unwrap_or(0);
    for (k, d) in to_core.iter().enumerate() {
        let (x, y) = (mx + (k % mw) as i32, my + (k / mw) as i32);
        if *d <= r as f32 + 0.5 && area.contains(x, y) && g.lum(x, y) >= light_min {
            interior.px[k] = true;
        }
    }
    interior.fill_holes();
    Some(judge(g.img, g.img_area, page, interior, walls, share))
}

fn judge(img: &[[u8; 4]], area: Rect, page: Rect, interior: Mask, walls: bool, share: f64) -> Result<Found, Miss> {
    let b = interior.bounds().ok_or(Miss::TooSmall)?;
    let count = interior.count();
    if (count as f64) < bubble::MIN_AREA || f64::from(b.width().min(b.height())) < bubble::MIN_SIDE {
        return Err(Miss::TooSmall);
    }
    if (count as f64) > share * f64::from(page.width()) * f64::from(page.height()) {
        return Err(Miss::TooBig);
    }
    let fill = count as f64 / (f64::from(b.width()) * f64::from(b.height())).max(1.0);
    let dark = if walls { bubble::outline_dark_walled(&interior, img, area) } else { bubble::outline_dark(&interior, img, area) };
    if fill < bubble::MIN_FILL || dark < bubble::MIN_OUTLINE_DARK {
        return Err(Miss::NotBubble);
    }
    let body = body_of(&interior).ok_or(Miss::TooSmall)?;
    Ok(Found { interior, body })
}

/// The body the text goes in: `interior` without its tail.
pub(crate) fn body_of(interior: &Mask) -> Option<Mask> {
    let b = interior.bounds()?;
    let radius = bubble::opening_radius(f64::from(b.width()), f64::from(b.height()));
    let mut body = if radius > 0 { interior.opened(radius as f32).unwrap_or_else(|| interior.clone()) } else { interior.clone() };
    // The opening can split a body pinched in the middle: keep the larger piece.
    body = largest_piece(&body);
    let bb = body.bounds()?;
    (f64::from(bb.width().min(bb.height())) >= bubble::MIN_SIDE).then_some(body)
}

/// A region at least this rectangular (its area over its minimum-area bounding rectangle's) is a
/// box, a narration caption: an ellipse fills 79 % of its rectangle, a rounded rectangle with
/// corners a tenth of its side 98 %.
pub const BOX_RECTANGULARITY: f64 = 0.92;

/// How rectangular `m` is: its pixel count over the area of its minimum-area bounding rectangle
/// (any angle; rotating calipers over the convex hull of the pixels' corners), and that
/// rectangle's corners in document pixels, clockwise from the first. `None` for an empty mask.
pub fn rectangularity(m: &Mask) -> Option<(f64, [(f64, f64); 4])> {
    // The hull of each row's outer pixel corners holds every pixel's.
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for (y, row) in m.px.chunks(m.width.max(1)).enumerate() {
        let Some(first) = row.iter().position(|p| *p) else { continue };
        let last = row.iter().rposition(|p| *p).unwrap_or(first);
        let (x0, x1, y0) = (f64::from(m.x0) + first as f64, f64::from(m.x0) + last as f64 + 1.0, f64::from(m.y0) + y as f64);
        pts.extend([(x0, y0), (x0, y0 + 1.0), (x1, y0), (x1, y0 + 1.0)]);
    }
    let hull = convex_hull(pts);
    if hull.len() < 3 {
        return None;
    }
    let n = hull.len();
    let mut best: Option<(f64, [(f64, f64); 4])> = None;
    for i in 0..n {
        let (a, b) = (hull[i], hull[(i + 1) % n]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if len < 1e-9 {
            continue;
        }
        let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let (mut s0, mut s1, mut t0, mut t1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in &hull {
            let (s, t) = (p.0 * ux + p.1 * uy, -p.0 * uy + p.1 * ux);
            (s0, s1, t0, t1) = (s0.min(s), s1.max(s), t0.min(t), t1.max(t));
        }
        let area = (s1 - s0) * (t1 - t0);
        if best.as_ref().is_none_or(|(a, _)| area < *a) {
            let pt = |s: f64, t: f64| (s * ux - t * uy, s * uy + t * ux);
            best = Some((area, [pt(s0, t0), pt(s1, t0), pt(s1, t1), pt(s0, t1)]));
        }
    }
    let (area, corners) = best?;
    Some(((m.count() as f64 / area.max(1.0)).min(1.0), corners))
}

/// Andrew's monotone chain: the convex hull, counter-clockwise, without collinear points.
pub(crate) fn convex_hull(mut pts: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut hull: Vec<(f64, f64)> = Vec::with_capacity(pts.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &(f64, f64)>> = if pass == 0 { Box::new(pts.iter()) } else { Box::new(pts.iter().rev()) };
        for &p in iter {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// The largest 4-connected piece of `m`.
fn largest_piece(m: &Mask) -> Mask {
    let (w, h) = (m.width, m.height);
    let mut label = vec![0u32; w * h];
    let (mut best, mut best_n, mut next) = (0u32, 0usize, 0u32);
    for start in 0..w * h {
        if !m.px[start] || label[start] != 0 {
            continue;
        }
        next += 1;
        let mut n = 0usize;
        let mut stack = vec![start];
        label[start] = next;
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = (i % w, i / w);
            let mut push = |j: usize| {
                if m.px[j] && label[j] == 0 {
                    label[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        if n > best_n {
            (best, best_n) = (next, n);
        }
    }
    if next <= 1 {
        return m.clone();
    }
    Mask { px: label.iter().map(|l| *l == best).collect(), ..m.clone() }
}

#[cfg(test)]
#[path = "bubble_find_tests.rs"]
mod tests;
