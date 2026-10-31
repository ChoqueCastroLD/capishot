use super::display::{canvas_display, display_doc, display_image};
use super::strokes::live_stroke;
use super::textures::effect_reach;
use super::{Document, PhotocraftApp, Rect, TextureOptions};
use photocraft_doc::Rect as DRect;

/// Longest side of the Navigator panel's image (about twice the panel's width, for HiDPI).
pub const NAVIGATOR_SIDE: u32 = 512;

/// The navigator image of a document: its snapshot, preview key and revision, and the texture.
pub(crate) type NavigatorCache = (std::sync::Weak<Document>, u64, u64, egui::TextureHandle);

/// How long adjustment-dialog settings must stay unchanged before the navigator shows them.
const NAVIGATOR_SETTLE_MS: f64 = 200.0;

/// While a Move or Patch drag or a live paint stroke is under way, the navigator keeps its image
/// of document `idx` and catches up on release: each step of the drag is a new preview, and a
/// thumbnail composite of the whole document per step makes painting lag on large images.
pub(super) fn navigator_waits_for_drag(app: &PhotocraftApp, idx: usize) -> bool {
    crate::move_ui::showing(app) || crate::patch_preview::showing(app) || crate::cam_preview::showing(app) || live_stroke(app, idx).is_some()
}

/// The Navigator panel's image of document `idx`: a thumbnail cached per document state, so an
/// edit to a huge document doesn't also pay a full-resolution CPU composite for the navigator
/// (the CPU canvas composites only the tiles in view, `cpu_canvas`).
/// The Navigator's preview from the GPU canvas: not used with Capishot's GPU compositor, whose
/// pages aren't shared with the Navigator (it draws the cached thumbnail).
pub fn navigator_gpu_view(_app: &PhotocraftApp, _idx: usize, _rect: Rect, _ctx: &egui::Context) -> Option<crate::gpu_canvas::ViewParams> {
    None
}

/// Whether the Navigator should wait a frame for the GPU canvas: never (see [`navigator_gpu_view`]).
pub fn navigator_waits_for_gpu(_app: &PhotocraftApp, _idx: usize, _ctx: &egui::Context) -> bool {
    false
}

/// What changed in a document since the Navigator's image of it: the revision that image shows
/// (`base`), the last revision seen (`seen`) and the area the edits between them changed (`None`:
/// not known, the whole image). Kept by [`navigator_track`] every frame, so an edit that takes
/// several revisions (a Move commits its drag in two or three steps) still updates only the area
/// it touched; with the last edit's damage alone, it recomposited the whole document on the UI
/// thread, hundreds of milliseconds on a large page in the browser.
pub(crate) struct NavigatorDamage {
    base: u64,
    seen: u64,
    area: Option<DRect>,
}

/// Every frame: note each document's edits since the Navigator's image (see [`NavigatorDamage`]).
pub(crate) fn navigator_track(app: &mut PhotocraftApp) {
    for st in app.session.documents() {
        let Some(d) = app.navigator_damage.get_mut(&st.doc.id) else { continue };
        if d.seen == st.revision {
            continue;
        }
        d.area = match (d.area, st.last_damage) {
            (Some(a), Some(b)) if d.seen + 1 == st.revision => Some(union(a, b)),
            _ => None,
        };
        d.seen = st.revision;
    }
}

fn union(a: DRect, b: DRect) -> DRect {
    if a.is_empty() {
        b
    } else if b.is_empty() {
        a
    } else {
        a.union(&b)
    }
}

/// The area to update in the Navigator's image of `id`, at revision `base`, to show `revision`:
/// `None` when only a whole new image will do.
fn navigator_damage(app: &PhotocraftApp, id: photocraft_doc::DocId, base: u64, revision: u64) -> Option<DRect> {
    app.navigator_damage.get(&id).filter(|d| d.base == base && d.seen == revision).and_then(|d| d.area)
}

/// A Navigator image being brought up to date a band of rows per frame (an edit's area, or the
/// whole image on the page of the single-threaded build, where no worker can take it): the image
/// keeps what it showed until each band is redrawn, and no frame spends more than
/// [`NAVIGATOR_BAND_MS`] on it.
pub(crate) struct NavigatorBands {
    snapshot: std::sync::Weak<Document>,
    key: u64,
    revision: u64,
    /// The area being redrawn (document pixels).
    region: DRect,
    /// The next document row to draw.
    next: i32,
    /// Document rows per band, sized by the last band's time.
    rows: i32,
}

/// The time a frame may spend on the Navigator's image in bands (ms).
const NAVIGATOR_BAND_MS: f64 = 8.0;

/// A Navigator image composited on the worker pool, for the document state `snapshot` with the
/// preview `key` (see [`navigator_texture`]).
pub(crate) struct NavigatorPending {
    snapshot: std::sync::Weak<Document>,
    key: u64,
    image: std::sync::Arc<wasm_sync::Mutex<Option<egui::ColorImage>>>,
}

pub fn navigator_texture(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize) -> Option<egui::TextureId> {
    // Keyed by the document snapshot, not its revision: selecting a layer changes no pixels.
    let (snapshot, id, revision, last_damage) =
        app.session.documents().get(idx).map(|st| (std::sync::Arc::downgrade(&st.doc), st.doc.id, st.revision, st.last_damage))?;
    let (doc, preview_key) = display_doc(app, idx);
    let (display, display_key) = canvas_display(app, &doc, None);
    let preview_key = preview_key ^ display_key;
    let cached = app.navigator_textures.get(&id).cloned();
    if let Some((r, p, _, t)) = &cached
        && r.ptr_eq(&snapshot)
        && *p == preview_key
    {
        return Some(t.id());
    }
    // While a gesture on the canvas is under way the Navigator keeps its image: nothing a panel
    // shows may cost a frame of the drag (it catches up on release).
    if let Some((_, _, _, t)) = &cached
        && (navigator_waits_for_drag(app, idx) || (ctx.input(|i| i.pointer.any_down()) && app.canvas_gesture_tool(ctx).is_some()))
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return Some(t.id());
    }
    // While an adjustment dialog's settings are changing, the navigator keeps its image and
    // catches up once they settle (a thumbnail composite per change would cost more than the canvas).
    if let Some((r, _, _, t)) = &cached
        && r.ptr_eq(&snapshot)
        && crate::adjust_preview::settling(app, NAVIGATOR_SETTLE_MS)
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(NAVIGATOR_SETTLE_MS as u64));
        return Some(t.id());
    }
    app.perf_counters.navigator += 1;
    let t0 = crate::gpu_canvas::now_ms();
    // One edit since the cached image, with the area it changed: update just that part (nothing
    // for a rename, a selection or a new empty layer). A thumbnail composite of the whole
    // document on every edit cost a full-resolution CPU composite on the UI thread.
    let size = photocraft_compose::thumbnail_size(&doc, NAVIGATOR_SIDE);
    let tracked = cached.as_ref().and_then(|(_, _, rev, _)| navigator_damage(app, id, *rev, revision));
    let damage = tracked.or(last_damage.filter(|_| cached.as_ref().is_some_and(|(_, _, rev, _)| rev.checked_add(1) == Some(revision))));
    // The image is there at this size and preview: update the area the edits changed (all of it
    // when that isn't known), a band of rows per frame within a time budget, so even an edit
    // across the whole page (a long stroke) never holds a frame for the whole composite. A whole
    // new image goes to a thread instead where there is one.
    if let Some((_, p, _, t)) = &cached
        && *p == preview_key
        && t.size() == [size.0 as usize, size.1 as usize]
        && (damage.is_some() || !NAVIGATOR_OFF_THREAD)
    {
        let region = match damage {
            Some(d) if d.is_empty() => d,
            Some(d) => d.inflate(effect_reach(&doc.layers)).intersect(&doc.bounds()),
            None => doc.bounds(),
        };
        let t = t.clone();
        let id = navigator_bands(app, ctx, &doc, display.as_deref(), (snapshot, preview_key, revision), size, t, region, damage.is_none());
        app.perf.span("navigator", crate::gpu_canvas::now_ms() - t0);
        return Some(id);
    }
    // The browser threads build composites the whole thumbnail on its worker pool: the panel keeps
    // its image (or none yet) for the frames that takes instead of the page waiting for it.
    let image = if NAVIGATOR_OFF_THREAD {
        let pending = app.navigator_pending.get(&id).filter(|p| p.snapshot.ptr_eq(&snapshot) && p.key == preview_key);
        match pending.map(|p| p.image.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()) {
            Some(Some(image)) => {
                app.navigator_pending.remove(&id);
                image
            }
            Some(None) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return cached.map(|(_, _, _, t)| t.id());
            }
            None => {
                let slot: std::sync::Arc<wasm_sync::Mutex<Option<egui::ColorImage>>> = std::sync::Arc::default();
                let out = std::sync::Arc::clone(&slot);
                let (doc, display) = (doc.clone(), display.clone());
                let waker = ctx.clone();
                app.perf_counters.navigator_full += 1;
                off_thread(move || {
                    let image = display_image(display.as_deref(), &photocraft_compose::thumbnail_buffer(&doc, NAVIGATOR_SIDE));
                    *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(image);
                    waker.request_repaint();
                });
                app.navigator_pending.insert(id, NavigatorPending { snapshot, key: preview_key, image: slot });
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return cached.map(|(_, _, _, t)| t.id());
            }
        }
    } else {
        app.perf_counters.navigator_full += 1;
        display_image(display.as_deref(), &photocraft_compose::thumbnail_buffer(&doc, NAVIGATOR_SIDE))
    };
    let tex = match cached {
        Some((_, _, _, mut t)) => {
            t.set(image, TextureOptions::LINEAR);
            t
        }
        None => ctx.load_texture(format!("navigator-{}", id.0), image, TextureOptions::LINEAR),
    };
    app.perf.span("navigator", crate::gpu_canvas::now_ms() - t0);
    app.navigator_textures.insert(id, (snapshot, preview_key, revision, tex.clone()));
    shown(app, id, revision);
    Some(tex.id())
}

/// The Navigator's image of `id` now shows `revision`: edits are tracked from there.
fn shown(app: &mut PhotocraftApp, id: photocraft_doc::DocId, revision: u64) {
    app.navigator_damage.insert(id, NavigatorDamage { base: revision, seen: revision, area: Some(DRect::EMPTY) });
}

/// Whole Navigator images are composited off the UI thread: the browser threads build's worker
/// pool, or a native thread. Not in tests, which read the image the frame they ask for it.
const NAVIGATOR_OFF_THREAD: bool = crate::jobs_ui::POOL_THREADS || cfg!(all(not(target_arch = "wasm32"), not(test)));

fn off_thread(f: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    rayon::spawn(f);
    #[cfg(target_arch = "wasm32")]
    crate::jobs_ui::on_pool(f);
}

/// The next bands of `region` (document pixels) of the Navigator image `t` drawn (see
/// [`NavigatorBands`]); once the last is in, the image is the document state `state`'s. A newer
/// state starts again over its own region: the edits tracked since the image's state, which
/// include whatever the older one hadn't drawn yet. `whole`: a whole new image (counted).
#[allow(clippy::too_many_arguments)]
fn navigator_bands(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    doc: &Document,
    display: Option<&photocraft_engine::display_color::CanvasDisplay>,
    state: (std::sync::Weak<Document>, u64, u64),
    size: (u32, u32),
    mut t: egui::TextureHandle,
    region: DRect,
    whole: bool,
) -> egui::TextureId {
    let (snapshot, key, revision) = state;
    let id = doc.id;
    let mut bands = match app.navigator_bands.remove(&id) {
        Some(b) if b.snapshot.ptr_eq(&snapshot) && b.key == key => b,
        _ => {
            if whole {
                app.perf_counters.navigator_full += 1;
            }
            // About a sixteenth of the page a band to start with; the time each takes sizes the next.
            NavigatorBands { snapshot: snapshot.clone(), key, revision, region, next: region.y0, rows: (doc.size.height as i32 / 16).max(16) }
        }
    };
    let region = bands.region;
    let t0 = crate::gpu_canvas::now_ms();
    while bands.next < region.y1 && crate::gpu_canvas::now_ms() - t0 < NAVIGATOR_BAND_MS {
        let b0 = crate::gpu_canvas::now_ms();
        let band = DRect::new(region.x0, bands.next, region.x1, (bands.next + bands.rows).min(region.y1));
        let buf = photocraft_compose::render_reduced_damage(doc, size.0, size.1, band);
        let r = buf.rect;
        if !r.is_empty() {
            t.set_partial([r.x0 as usize, r.y0 as usize], display_image(display, &buf), TextureOptions::LINEAR);
        }
        bands.next = band.y1;
        // Aim each band at half the frame's budget, for its width.
        let took = (crate::gpu_canvas::now_ms() - b0).max(0.1);
        bands.rows = ((f64::from(bands.rows) * (NAVIGATOR_BAND_MS / 2.0) / took) as i32).clamp(8, (doc.size.height as i32).max(8));
    }
    if bands.next >= region.y1 {
        app.navigator_textures.insert(id, (snapshot, key, bands.revision, t.clone()));
        shown(app, id, bands.revision);
    } else {
        app.navigator_bands.insert(id, bands);
        ctx.request_repaint();
    }
    t.id()
}

#[cfg(test)]
#[path = "navigator_tests.rs"]
mod tests;
