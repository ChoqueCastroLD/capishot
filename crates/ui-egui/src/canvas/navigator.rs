use super::display::{canvas_display, display_doc, display_image};
use super::strokes::live_stroke;
use super::textures::{effect_reach, ensure_texture};
use super::{Document, PhotocraftApp, Rect, TextureOptions};

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
    if let Some((_, _, _, t)) = &cached
        && navigator_waits_for_drag(app, idx)
    {
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
    if let Some((_, p, rev, t)) = &cached
        && *p == preview_key
        && rev.checked_add(1) == Some(revision)
        && let Some(d) = last_damage
        && t.size() == [size.0 as usize, size.1 as usize]
    {
        let mut t = t.clone();
        if !d.is_empty() {
            let buf = photocraft_compose::render_reduced_damage(&doc, size.0, size.1, d.inflate(effect_reach(&doc.layers)));
            let r = buf.rect;
            if !r.is_empty() {
                t.set_partial([r.x0 as usize, r.y0 as usize], display_image(display.as_deref(), &buf), TextureOptions::LINEAR);
            }
        }
        app.perf.span("navigator", crate::gpu_canvas::now_ms() - t0);
        app.navigator_textures.insert(id, (snapshot, preview_key, revision, t.clone()));
        return Some(t.id());
    }
    // The browser threads build composites the whole thumbnail on its worker pool: the panel keeps
    // its image (or none yet) for the frames that takes instead of the page waiting for it.
    let image = if crate::jobs_ui::POOL_THREADS {
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
                crate::jobs_ui::on_pool(move || {
                    let image = display_image(display.as_deref(), &photocraft_compose::thumbnail_buffer(&doc, NAVIGATOR_SIDE));
                    *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(image);
                });
                app.navigator_pending.insert(id, NavigatorPending { snapshot, key: preview_key, image: slot });
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return cached.map(|(_, _, _, t)| t.id());
            }
        }
    } else {
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
    Some(tex.id())
}
