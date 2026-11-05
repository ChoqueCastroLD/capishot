//! Several canvases in a host's rects (`capishot-host`'s `CapishotView::ui_tiles`, Capikoma
//! request 7): each document or named view in a tile of its own, with one camera or one each.
//!
//! The active document's first plain tile is live: it has the tools and their overlays, as the
//! one canvas of [`super::embedded_area`] does. Every other tile is a view (`primary` off): it
//! pans and zooms, and a press on another document's tile makes that document the active one.
//!
//! A tile with a named view composites a copy of the document with that view's visibility
//! ([`photocraft_engine::doc_views_cmds::view_document`]); the document's `layer.visible` stays as
//! it is. The copy shares the layers' pixels: what a named view costs is its own display texture,
//! `4/3 × 4 × width × height` bytes (with mips; twice that for a high-bit `Rgba16Float` canvas),
//! about 44 MB for a 2400×3400 page, kept per document and view name. It is drawn by the CPU
//! compositor, in full when the view changes and in the changed area after an edit; off the GPU it
//! is one reduced egui texture ([`super::MAX_TEXTURE`] a side at most). These textures count against
//! `editor.setMemoryBudget` and [`crate::gpu_canvas::canvas_budget`] (over it, the tile draws off
//! the GPU). Those no tile showed in the last frame go first, and at most [`MAX_IDLE_VIEWS`] stay.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Rect, TextureHandle, UiBuilder};
use photocraft_doc::{DocId, Document};
use photocraft_engine::doc_views_cmds::{view_document, view_signature};
use photocraft_engine::layer_multi_cmds::step_damage;

use super::display::{canvas_display, display_image, texture_key};
use super::paint_dots;
use super::textures::{canvas_texture_options, retain_gpu_documents, texture_factor, texture_side};
use super::{PhotocraftApp, View, ViewXform, canvas_view_tile};

/// The most tiles one frame draws; the rest report an error.
pub const MAX_TILES: usize = 32;
/// The most named views' textures kept with no tile showing them (a host flipping a tile between
/// views doesn't draw each one again).
const MAX_IDLE_VIEWS: usize = 4;
/// Where tile texture keys start: above document ids, apart from the preview keys (bits 61, 62).
const TILE_KEY_BASE: u64 = 1 << 60;

/// One canvas a host wants drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbedTile {
    /// The document.
    pub document: DocId,
    /// Where, in egui points.
    pub rect: Rect,
    /// The named view (`document.views.*`) it shows, composited on its own; `None`: the document
    /// as it is.
    pub view: Option<String>,
}

/// Whose camera the tiles draw with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TileCamera {
    /// One camera for all (zoom, centre, rotation, as numbers: tiles of same-sized pages line
    /// up): the active document's, and panning or zooming any tile moves it (default).
    #[default]
    Shared,
    /// One per tile: a document's first tile has the document's own camera, any other tile starts
    /// fit to its rect and keeps its own for as long as the document is open.
    Own,
}

/// What a tile drew.
#[derive(Clone, Debug, Default)]
pub struct TileDrawn {
    /// A canvas was drawn (false: see [`Self::error`]; the rect shows the pasteboard).
    pub drawn: bool,
    /// This tile has the tools.
    pub live: bool,
    /// Its document ↔ screen mapping (inside its rulers).
    pub xform: Option<ViewXform>,
    /// Its document's size in pixels.
    pub size: [u32; 2],
    /// Why it wasn't drawn: no such document or view, a rect without an area, too many tiles.
    pub error: Option<String>,
}

/// Which tile a canvas view is drawing.
pub(super) struct TileSource {
    /// Tells apart the views of one document in a frame.
    pub(super) salt: u64,
    pub(super) named: Option<String>,
}

/// What a named view's texture holds.
struct Drawn {
    revision: u64,
    signature: u64,
    display: u64,
    /// The document as drawn, to tell the next edit's damage. Shares the pixels with the document.
    shown: Arc<Document>,
}

impl Drawn {
    fn is(&self, revision: u64, signature: u64, display: u64) -> bool {
        self.revision == revision && self.signature == signature && self.display == display
    }
}

/// A named view's textures.
struct NamedTexture {
    /// Its key on the GPU canvas.
    key: u64,
    gpu: Option<Drawn>,
    cpu: Option<Drawn>,
    texture: Option<TextureHandle>,
    last_used: u64,
}

impl NamedTexture {
    fn cpu_bytes(&self) -> u64 {
        self.texture.as_ref().map_or(0, |t| {
            let [w, h] = t.size();
            (w as u64).saturating_mul(h as u64).saturating_mul(4)
        })
    }
}

/// [`PhotocraftApp`]'s tile state.
#[derive(Default)]
pub(crate) struct TileViews {
    textures: HashMap<(DocId, String), NamedTexture>,
    /// [`TileCamera::Own`]: the cameras of tiles after their document's first (document, view,
    /// which of the tiles showing them).
    cameras: HashMap<(DocId, Option<String>, usize), View>,
    next_key: u64,
    /// The documents the last tiles frame drew, and that frame.
    on_screen: (Vec<DocId>, u64),
}

/// Whether a host's tile rect can hold a canvas.
pub(crate) fn tile_rect_ok(r: Rect) -> bool {
    r.is_finite() && r.is_positive()
}

/// The keys of the named views' textures, which the GPU canvas must keep.
pub(super) fn gpu_keys(app: &PhotocraftApp) -> Vec<u64> {
    app.tile_views.textures.values().map(|t| t.key).collect()
}

/// Once a frame: a closed document's textures and cameras go, and the idle views past the cap (all
/// of them once the host draws no more tiles).
pub(super) fn retain_documents(app: &mut PhotocraftApp) {
    let docs = app.session.documents();
    let open = |id: &DocId| docs.iter().any(|st| st.doc.id == *id);
    // The GPU side goes with `retain`: its key isn't live any more.
    app.tile_views.textures.retain(|(id, _), _| open(id));
    app.tile_views.cameras.retain(|(id, ..), _| open(id));
    let keep = if app.tile_views.on_screen.1.saturating_add(1) < app.frame { 0 } else { MAX_IDLE_VIEWS };
    for which in idle_views(app).iter().rev().skip(keep) {
        free(app, which);
    }
}

/// The bytes document `id`'s named views hold in textures.
pub(crate) fn tile_views_bytes(app: &PhotocraftApp, id: DocId) -> u64 {
    app.tile_views
        .textures
        .iter()
        .filter(|((d, _), _)| *d == id)
        .map(|(_, t)| app.gpu.as_ref().and_then(|g| g.texture_info(t.key)).map_or(0, |(_, b)| b).saturating_add(t.cpu_bytes()))
        .sum()
}

/// Whether the last frame's tiles showed document `id`: its canvas is on screen, active or not.
pub(crate) fn tiles_show(app: &PhotocraftApp, id: DocId) -> bool {
    let (docs, frame) = &app.tile_views.on_screen;
    frame.saturating_add(1) >= app.frame && docs.contains(&id)
}

fn free(app: &mut PhotocraftApp, which: &(DocId, String)) -> u64 {
    let Some(t) = app.tile_views.textures.remove(which) else { return 0 };
    t.cpu_bytes().saturating_add(app.gpu.as_ref().map_or(0, |g| g.evict(t.key)))
}

/// Free document `id`'s named views: the budget took its canvas.
pub(crate) fn free_document_tile_views(app: &mut PhotocraftApp, id: DocId) {
    let gone: Vec<(DocId, String)> = app.tile_views.textures.keys().filter(|(d, _)| *d == id).cloned().collect();
    for which in gone {
        free(app, &which);
    }
}

/// Free the named views no tile showed in the last frame, the longest unused first, until `want`
/// bytes are free. Returns the bytes freed.
pub(crate) fn free_idle_tile_views(app: &mut PhotocraftApp, want: u64) -> u64 {
    let mut freed = 0u64;
    for which in idle_views(app) {
        if freed >= want {
            break;
        }
        freed = freed.saturating_add(free(app, &which));
    }
    freed
}

/// The views no tile showed in the last frame, the longest unused first.
fn idle_views(app: &PhotocraftApp) -> Vec<(DocId, String)> {
    let mut idle: Vec<_> = app.tile_views.textures.iter().filter(|(_, t)| t.last_used.saturating_add(1) < app.frame).map(|(k, t)| (t.last_used, k.clone())).collect();
    idle.sort_by_key(|(used, _)| *used);
    idle.into_iter().map(|(_, k)| k).collect()
}

fn texture_entry<'a>(views: &'a mut TileViews, id: DocId, name: &str, frame: u64) -> &'a mut NamedTexture {
    let next = &mut views.next_key;
    let entry = views.textures.entry((id, name.to_string())).or_insert_with(|| {
        *next = next.saturating_add(1);
        NamedTexture { key: TILE_KEY_BASE | *next, gpu: None, cpu: None, texture: None, last_used: frame }
    });
    entry.last_used = frame;
    entry
}

/// Bring view `name` of document `idx` up to date on the GPU canvas. Returns its texture key, or
/// `None` when the canvas can't hold it (the tile draws off the GPU).
pub(super) fn ensure_gpu(app: &mut PhotocraftApp, idx: usize, name: &str) -> Option<u64> {
    let gpu = app.gpu.clone()?;
    let st = app.session.documents().get(idx)?;
    let (id, revision, doc) = (st.doc.id, st.revision, st.doc.clone());
    let signature = view_signature(&doc, name)?;
    let (display, _) = canvas_display(app, &doc, None);
    let display_key = texture_key(display.as_deref());
    let size = [doc.size.width, doc.size.height];
    let frame = app.frame;
    let entry = texture_entry(&mut app.tile_views, id, name, frame);
    let key = entry.key;
    let present = gpu.has(key, size);
    if present && entry.gpu.as_ref().is_some_and(|d| d.is(revision, signature, display_key)) {
        return Some(key);
    }
    if !gpu.fits_canvas(&doc) {
        entry.gpu = None;
        return None;
    }
    let shown = Arc::new(view_document(&doc, name)?);
    let damage = entry.gpu.as_ref().filter(|d| present && d.display == display_key).and_then(|d| step_damage(&d.shown, &shown));
    let r = gpu.refresh(key, &shown, damage, display.as_deref());
    if r.kind == "lost" || r.kind == crate::gpu_canvas::REFRESH_OVER_BUDGET {
        entry.gpu = None;
        return None;
    }
    entry.gpu = Some(Drawn { revision, signature, display: display_key, shown });
    app.perf.record(r.kind, r.px, r.composite_ms, r.upload_ms);
    Some(key)
}

/// Bring view `name` of document `idx` up to date in its egui texture, for the canvas off the GPU.
pub(super) fn ensure_cpu(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize, name: &str, output: Option<u32>) -> Option<egui::TextureId> {
    let st = app.session.documents().get(idx)?;
    let (id, revision, doc) = (st.doc.id, st.revision, st.doc.clone());
    if doc.size.width == 0 || doc.size.height == 0 {
        return None;
    }
    let signature = view_signature(&doc, name)?;
    let (display, display_key) = canvas_display(app, &doc, output);
    let frame = app.frame;
    let entry = texture_entry(&mut app.tile_views, id, name, frame);
    if let Some(t) = entry.texture.as_ref().filter(|_| entry.cpu.as_ref().is_some_and(|d| d.is(revision, signature, display_key))) {
        return Some(t.id());
    }
    let shown = Arc::new(view_document(&doc, name)?);
    let factor = texture_factor([doc.size.width, doc.size.height], texture_side(ctx));
    let (w, h) = ((doc.size.width / factor).max(1), (doc.size.height / factor).max(1));
    let damage = entry.cpu.as_ref().filter(|d| d.display == display_key).and_then(|d| step_damage(&d.shown, &shown));
    let fits = entry.texture.as_ref().is_some_and(|t| t.size() == [w as usize, h as usize]);
    if let Some(d) = damage.filter(|_| fits) {
        // Only what the edit touched, with the values the whole reduction gives it.
        let buf = photocraft_compose::render_reduced_damage(&shown, w, h, d);
        let r = buf.rect;
        if !r.is_empty()
            && let Some(t) = entry.texture.as_mut()
        {
            t.set_partial([r.x0 as usize, r.y0 as usize], display_image(display.as_deref(), &buf), canvas_texture_options(factor));
        }
    } else {
        let image = display_image(display.as_deref(), &photocraft_compose::render_reduced(&shown, w, h));
        match entry.texture.as_mut() {
            Some(t) if fits => t.set(image, canvas_texture_options(factor)),
            _ => entry.texture = Some(ctx.load_texture(format!("tile-view-{}", entry.key), image, canvas_texture_options(factor))),
        }
    }
    entry.cpu = Some(Drawn { revision, signature, display: display_key, shown });
    entry.texture.as_ref().map(TextureHandle::id)
}

/// How each tile will draw.
#[derive(Clone, Debug, Default, PartialEq)]
struct Plan {
    /// Its document's session index (`None`: see `error`).
    idx: Option<usize>,
    live: bool,
    /// The tile with the document's own camera: the live one, else the document's first.
    first: bool,
    /// Which of the tiles showing this document and view.
    slot: usize,
    error: Option<String>,
}

/// Resolve the tiles against the session's documents (`ids`) and named views (`has_view`).
fn plan(tiles: &[EmbedTile], ids: &[DocId], active: Option<usize>, has_view: impl Fn(usize, &str) -> bool) -> Vec<Plan> {
    let mut plans: Vec<Plan> = Vec::with_capacity(tiles.len());
    for (i, t) in tiles.iter().enumerate() {
        let mut p = Plan::default();
        let idx = ids.iter().position(|id| *id == t.document);
        if i >= MAX_TILES {
            p.error = Some(format!("at most {MAX_TILES} tiles"));
        } else if !tile_rect_ok(t.rect) {
            p.error = Some("the tile's rect has no area".into());
        } else if idx.is_none() {
            p.error = Some(format!("no open document {}", t.document.0));
        } else if let Some(name) = t.view.as_deref().filter(|name| idx.is_some_and(|d| !has_view(d, name))) {
            p.error = Some(format!("no view named \"{name}\""));
        } else {
            let earlier = |q: &(usize, &EmbedTile)| plans.get(q.0).is_some_and(|e| e.error.is_none()) && q.1.document == t.document;
            let shown: Vec<_> = tiles.iter().enumerate().take(i).filter(earlier).collect();
            p.idx = idx;
            p.first = shown.is_empty();
            p.slot = shown.iter().filter(|(_, e)| e.view == t.view).count();
            p.live = idx.is_some() && idx == active && t.view.is_none() && !plans.iter().any(|e| e.live);
        }
        plans.push(p);
    }
    // The live tile's canvas stores the document's camera: that one is its document's first.
    if let Some(live) = plans.iter().position(|p| p.live) {
        let doc = plans.get(live).and_then(|p| p.idx);
        for (i, p) in plans.iter_mut().enumerate() {
            if p.idx == doc {
                p.first = i == live;
            }
        }
    }
    plans
}

/// The camera of `from`, on a view that keeps its document's size.
fn adopt_camera(to: &mut View, from: &View) {
    to.zoom = from.zoom;
    to.center = from.center;
    to.rotation = from.rotation;
    to.zoom_animating = from.zoom_animating;
    to.zoom_anchor = from.zoom_anchor;
    to.display_camera = from.display_camera;
    to.zoom_transition = from.zoom_transition;
    to.fit_pending = false;
    to.fill_pending = false;
}

fn same_camera(a: &View, b: &View) -> bool {
    a.zoom == b.zoom && a.center == b.center && a.rotation == b.rotation
}

fn tile_xform(app: &PhotocraftApp, view: &View, rect: Rect, ppp: f32) -> ViewXform {
    let (zoom, center) =
        if view.zoom_animating && app.session.prefs().general.animated_zoom { view.display_camera.unwrap_or((view.zoom, view.center)) } else { (view.zoom, view.center) };
    ViewXform {
        rect: crate::rulers::content_rect(app, rect),
        zoom: zoom / ppp,
        center,
        flip: app.ui.view.flip_horizontal,
        rotation: view.rotation,
        aspect: app.ui.view.display_aspect(),
    }
    .pixel_aligned_for(app, ppp)
}

/// The canvases of `tiles` ([`crate::embed::tiles_frame`]). One entry per tile, in order.
pub(crate) fn embedded_tiles(app: &mut PhotocraftApp, ui: &mut egui::Ui, tiles: &[EmbedTile], camera: TileCamera) -> Vec<TileDrawn> {
    app.drop_canvas_rect = None;
    app.tab_strip = None;
    retain_gpu_documents(app);
    crate::transform_tool::track_steps(app, ui.ctx());
    crate::cas_transform::tick(app, ui.ctx());
    crate::symmetry_ui::track(app, ui.ctx());
    let ppp = ui.ctx().pixels_per_point();
    let active = app.session.active_index();
    let ids: Vec<DocId> = app.session.documents().iter().map(|st| st.doc.id).collect();
    let plans = {
        let docs = app.session.documents();
        plan(tiles, &ids, active, |d, name| docs.get(d).is_some_and(|st| view_signature(&st.doc, name).is_some()))
    };
    let mut out: Vec<TileDrawn> = plans.iter().map(|p| TileDrawn { error: p.error.clone(), ..Default::default() }).collect();
    // The tile whose view the shared camera follows draws first: its fit resolves the camera.
    let leader = plans
        .iter()
        .position(|p| p.live)
        .or_else(|| plans.iter().position(|p| p.idx.is_some() && p.idx == active))
        .or_else(|| plans.iter().position(|p| p.idx.is_some()));
    if let Some(rect) = leader.and_then(|l| tiles.get(l)).map(|t| t.rect) {
        app.last_canvas_rect = rect;
        app.drop_canvas_rect = Some(rect);
    }
    if !plans.iter().any(|p| p.live) {
        // Without a live canvas nothing else records the frame's scale.
        app.ppp = ppp;
    }
    let leader_idx = leader.and_then(|l| plans.get(l)).and_then(|p| p.idx);
    let mut shared: Option<View> = None;
    let dots = app.session.prefs().interface.show_canvas_dots;
    let hover = ui.ctx().input(|i| i.pointer.hover_pos());
    let pressed = ui.ctx().input(|i| if i.pointer.primary_pressed() { i.pointer.interact_pos() } else { None });
    for i in leader.into_iter().chain((0..tiles.len()).filter(|i| Some(*i) != leader)) {
        let (Some(tile), Some(p)) = (tiles.get(i), plans.get(i)) else { continue };
        let Some(idx) = p.idx else {
            if tile_rect_ok(tile.rect) && i < MAX_TILES {
                paint_dots(ui, tile.rect, dots);
            }
            continue;
        };
        let Some(stored) = app.ui.views.get(idx).cloned() else {
            if let Some(o) = out.get_mut(i) {
                o.error = Some(format!("no view for document {}", tile.document.0));
            }
            continue;
        };
        let own_key = (tile.document, tile.view.clone(), p.slot);
        let mut view = match camera {
            TileCamera::Own if !p.first => app.tile_views.cameras.get(&own_key).cloned().unwrap_or_else(|| View { fit_pending: true, ..stored.clone() }),
            _ => stored,
        };
        if camera == TileCamera::Shared
            && let Some(s) = &shared
        {
            adopt_camera(&mut view, s);
        }
        let before = view.clone();
        let source = TileSource { salt: i as u64, named: tile.view.clone() };
        let mut tile_ui = ui.new_child(UiBuilder::new().max_rect(tile.rect).id_salt(("capishot-tile", i)));
        tile_ui.set_clip_rect(tile.rect.intersect(ui.clip_rect()));
        let shown = canvas_view_tile(app, &mut tile_ui, idx, tile.rect, view, p.live, Some(&source));
        let hovered = hover.is_some_and(|h| tile.rect.contains(h));
        match camera {
            TileCamera::Shared => {
                // The person moved the camera on a tile that isn't the leader: all follow.
                if !p.live && Some(i) != leader && hovered && !same_camera(&before, &shown) {
                    if let Some(l) = leader_idx.and_then(|l| app.ui.views.get_mut(l)) {
                        adopt_camera(l, &shown);
                    }
                    ui.ctx().request_repaint();
                }
                if p.live {
                    // Its canvas stored the view; the others follow what it holds now.
                    shared = app.ui.views.get(idx).cloned().or_else(|| Some(shown.clone()));
                } else {
                    if Some(i) == leader {
                        shared = Some(shown.clone());
                    }
                    if p.first
                        && let Some(v) = app.ui.views.get_mut(idx)
                    {
                        *v = shown.clone();
                    }
                }
            }
            TileCamera::Own => {
                if p.first {
                    if !p.live
                        && let Some(v) = app.ui.views.get_mut(idx)
                    {
                        *v = shown.clone();
                    }
                } else {
                    app.tile_views.cameras.insert(own_key, shown.clone());
                }
            }
        }
        // A press on another document's tile makes it the active one (hidden ones stay as they are).
        if !p.live && Some(idx) != active && pressed.is_some_and(|at| tile.rect.contains(at)) {
            app.session.set_active(idx);
        }
        let final_view = if p.live { app.ui.views.get(idx).cloned().unwrap_or_else(|| shown.clone()) } else { shown };
        let xform = tile_xform(app, &final_view, tile.rect, ppp);
        let size = app.session.documents().get(idx).map_or([0, 0], |st| [st.doc.size.width, st.doc.size.height]);
        if let Some(o) = out.get_mut(i) {
            *o = TileDrawn { drawn: true, live: p.live, xform: Some(xform), size, error: None };
        }
    }
    app.tile_views.on_screen = (plans.iter().filter_map(|p| p.idx).filter_map(|d| ids.get(d).copied()).collect(), app.frame);
    crate::layer_transfer::finish(app, ui.ctx());
    crate::paint_mouse::show_picker(app, ui.ctx());
    out
}

#[cfg(test)]
mod tests {
    use egui::{Context, Pos2, RawInput, vec2};
    use photocraft_engine::Session;
    use serde_json::json;

    use super::*;
    use crate::Services;
    use crate::embed::{self, EmbedOptions};

    fn rect(x: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x, 0.0), vec2(300.0, 300.0))
    }

    fn tile(document: DocId, x: f32, view: Option<&str>) -> EmbedTile {
        EmbedTile { document, rect: rect(x), view: view.map(str::to_string) }
    }

    fn input(events: Vec<egui::Event>) -> RawInput {
        RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))), focused: true, events, ..Default::default() }
    }

    fn run(ctx: &Context, app: &mut PhotocraftApp, tiles: &[EmbedTile], camera: TileCamera, events: Vec<egui::Event>) -> Vec<TileDrawn> {
        let mut drawn = Vec::new();
        ctx.run_ui(input(events), |ui| {
            drawn = embed::tiles_frame(app, ui, tiles, camera, &EmbedOptions::default()).1;
        })
        .textures_delta
        .clear();
        drawn
    }

    /// Two 64×48 documents; the second is active.
    fn two_docs() -> (Session, DocId, DocId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
        let a = s.active().expect("active").doc.id;
        s.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
        let b = s.active().expect("active").doc.id;
        (s, a, b)
    }

    /// A document with a layer (id returned) and a view that hides it.
    fn doc_with_view() -> (Session, DocId, u64) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
        let id = s.active().expect("active").doc.id;
        let layer = s.execute("layer.new.layer", json!({"name": "top"})).expect("a layer")["layer"].as_u64().expect("an id");
        s.execute("edit.fill", json!({"contents": "black"})).expect("fill");
        s.execute("document.views.define", json!({"name": "clean", "hidden": [layer]})).expect("define");
        s.execute("document.views.define", json!({"name": "other", "visible": [layer]})).expect("define");
        (s, id, layer)
    }

    fn app_with(session: Session) -> Box<PhotocraftApp> {
        let mut app = Box::new(PhotocraftApp::new_embedded(Services::default()));
        app.session = session;
        app
    }

    #[test]
    fn the_active_document_s_tile_is_live_and_each_tile_has_its_mapping() {
        let (s, a, b) = two_docs();
        let mut app = app_with(s);
        let ctx = Context::default();
        let drawn = run(&ctx, &mut app, &[tile(a, 0.0, None), tile(b, 400.0, None)], TileCamera::Shared, Vec::new());
        assert!(drawn[0].drawn && drawn[1].drawn);
        assert!(!drawn[0].live && drawn[1].live, "the second document is the active one");
        assert!(rect(0.0).contains(drawn[0].xform.expect("a mapping").rect.center()));
        assert!(rect(400.0).contains(drawn[1].xform.expect("a mapping").rect.center()));
        assert_eq!(drawn[0].size, [64, 48]);
    }

    #[test]
    fn a_press_on_another_document_s_tile_activates_it() {
        let (s, a, b) = two_docs();
        let mut app = app_with(s);
        let ctx = Context::default();
        let tiles = [tile(a, 0.0, None), tile(b, 400.0, None)];
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        let pos = Pos2::new(100.0, 100.0);
        let press = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        ];
        run(&ctx, &mut app, &tiles, TileCamera::Shared, press);
        assert_eq!(app.session.active().map(|st| st.doc.id), Some(a));
        let drawn = run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        assert!(drawn[0].live && !drawn[1].live, "the live tile follows the active document");
    }

    #[test]
    fn the_shared_camera_is_one_and_own_cameras_are_apart() {
        let (s, a, b) = two_docs();
        let mut app = app_with(s);
        let ctx = Context::default();
        let tiles = [tile(a, 0.0, None), tile(b, 400.0, None)];
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        let active = app.session.index_of_document(b).expect("open");
        embed::set_camera(&mut app, active, embed::CameraChange::Set { zoom: Some(3.0), center: Some([10.0, 12.0]), rotation: None }).expect("camera");
        let drawn = run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        let (l, r) = (drawn[0].xform.expect("a mapping"), drawn[1].xform.expect("a mapping"));
        assert_eq!(l.zoom, r.zoom, "one zoom");
        assert_eq!(l.center, r.center, "one centre");
        embed::set_camera(&mut app, active, embed::CameraChange::Set { zoom: Some(5.0), center: None, rotation: None }).expect("camera");
        let drawn = run(&ctx, &mut app, &tiles, TileCamera::Own, Vec::new());
        assert_ne!(drawn[0].xform.expect("a mapping").zoom, drawn[1].xform.expect("a mapping").zoom, "each has its own");
    }

    #[test]
    fn a_named_view_draws_on_its_own_and_leaves_visibility_alone() {
        let (s, id, layer) = doc_with_view();
        let mut app = app_with(s);
        let revision = app.session.active().map(|st| st.revision);
        let ctx = Context::default();
        let drawn = run(&ctx, &mut app, &[tile(id, 0.0, None), tile(id, 400.0, Some("clean"))], TileCamera::Shared, Vec::new());
        assert!(drawn[0].live && !drawn[1].live, "the plain tile has the tools");
        assert!(drawn[1].drawn, "{:?}", drawn[1].error);
        let st = app.session.active().expect("active");
        assert!(st.doc.layer(photocraft_doc::LayerId(layer)).is_some_and(|l| l.visible), "the layer stays visible");
        assert_eq!(Some(st.revision), revision, "no edit");
        assert!(tile_views_bytes(&app, id) > 0, "its texture counts");
    }

    #[test]
    fn a_named_view_follows_edits_and_its_definition() {
        let (s, id, _) = doc_with_view();
        let mut app = app_with(s);
        let ctx = Context::default();
        let tiles = [tile(id, 0.0, Some("clean"))];
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        let drawn_at = |app: &PhotocraftApp| app.tile_views.textures.values().next().and_then(|t| t.cpu.as_ref()).map(|d| (d.revision, d.signature));
        let first = drawn_at(&app).expect("drawn");
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        assert_eq!(drawn_at(&app), Some(first), "nothing changed");
        app.session.execute("edit.fill", json!({"contents": "white"})).expect("an edit");
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        assert_ne!(drawn_at(&app).map(|d| d.0), Some(first.0), "an edit draws again");
        let rev = drawn_at(&app);
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        assert_eq!(drawn_at(&app), rev);
    }

    #[test]
    fn bad_tiles_report_and_never_panic() {
        let (s, id, _) = doc_with_view();
        let mut app = app_with(s);
        let ctx = Context::default();
        let nan = Rect::from_min_max(Pos2::new(f32::NAN, 0.0), Pos2::new(10.0, f32::INFINITY));
        let tiles = [
            tile(DocId(9_999), 0.0, None),
            tile(id, 0.0, Some("nope")),
            EmbedTile { document: id, rect: nan, view: None },
            EmbedTile { document: id, rect: Rect::from_min_size(Pos2::ZERO, vec2(-5.0, 10.0)), view: None },
            tile(id, 400.0, None),
        ];
        let drawn = run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        for d in &drawn[..4] {
            assert!(!d.drawn && d.error.is_some(), "{d:?}");
        }
        assert!(drawn[4].drawn && drawn[4].live);
        assert!(run(&ctx, &mut app, &[], TileCamera::Own, Vec::new()).is_empty(), "no tiles is a frame");
        let many: Vec<EmbedTile> = (0..MAX_TILES + 2).map(|i| tile(id, i as f32, None)).collect();
        let drawn = run(&ctx, &mut app, &many, TileCamera::Own, Vec::new());
        assert!(drawn[MAX_TILES].error.is_some() && drawn[MAX_TILES - 1].drawn);
    }

    #[test]
    fn two_cameras_of_one_document_don_t_collide_and_closing_frees_them() {
        let (s, id, _) = doc_with_view();
        let mut app = app_with(s);
        let ctx = Context::default();
        let tiles = [tile(id, 0.0, None), tile(id, 400.0, None), tile(id, 700.0, Some("clean"))];
        let drawn = run(&ctx, &mut app, &tiles, TileCamera::Own, Vec::new());
        assert!(drawn[0].live && !drawn[1].live && !drawn[2].live);
        assert_eq!(app.tile_views.cameras.len(), 2);
        app.session.execute("file.close", json!({})).expect("close");
        run(&ctx, &mut app, &[], TileCamera::Own, Vec::new());
        assert!(app.tile_views.textures.is_empty() && app.tile_views.cameras.is_empty());
    }

    #[test]
    fn idle_views_are_capped_and_the_budget_frees_them_first() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48})).expect("a document");
        let id = s.active().expect("active").doc.id;
        let layer = s.execute("layer.new.layer", json!({"name": "top"})).expect("a layer")["layer"].as_u64().expect("an id");
        for i in 0..8 {
            s.execute("document.views.define", json!({"name": format!("v{i}"), "hidden": [layer]})).expect("define");
        }
        let mut app = app_with(s);
        let ctx = Context::default();
        for i in 0..8 {
            let name = format!("v{i}");
            run(&ctx, &mut app, &[tile(id, 0.0, Some(name.as_str()))], TileCamera::Shared, Vec::new());
            run(&ctx, &mut app, &[tile(id, 0.0, Some(name.as_str()))], TileCamera::Shared, Vec::new());
        }
        // Shown now plus the idle ones the cap keeps.
        assert!(app.tile_views.textures.len() <= MAX_IDLE_VIEWS + 2, "{}", app.tile_views.textures.len());
        run(&ctx, &mut app, &[tile(id, 0.0, Some("v7"))], TileCamera::Shared, Vec::new());
        run(&ctx, &mut app, &[tile(id, 0.0, Some("v7"))], TileCamera::Shared, Vec::new());
        assert!(free_idle_tile_views(&mut app, u64::MAX) > 0);
        assert!(app.tile_views.textures.contains_key(&(id, "v7".to_string())), "what a tile shows stays");
        assert_eq!(app.tile_views.textures.len(), 1);
        assert!(tiles_show(&app, id));
    }

    /// Red under blue: the document shows blue, a view that hides the blue layer shows red, each
    /// in its own texture, on a real device (skipped without one).
    #[test]
    fn on_the_gpu_a_named_view_has_its_own_composite() {
        let Ok(rs) = std::panic::catch_unwind(|| egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default())) else { return };
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).expect("a document");
        let id = s.active().expect("active").doc.id;
        s.execute("layer.new.layer", json!({"name": "red"})).expect("a layer");
        s.execute("edit.fill", json!({"contents": "color", "color": "#ff0000"})).expect("fill");
        let blue = s.execute("layer.new.layer", json!({"name": "blue"})).expect("a layer")["layer"].as_u64().expect("an id");
        s.execute("edit.fill", json!({"contents": "color", "color": "#0000ff"})).expect("fill");
        s.execute("document.views.define", json!({"name": "under", "hidden": [blue]})).expect("define");
        let mut app = app_with(s);
        app.set_wgpu(rs);
        let ctx = Context::default();
        let tiles = [tile(id, 0.0, None), tile(id, 400.0, Some("under"))];
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        let Some(gpu) = app.gpu.clone() else { return };
        let key = app.tile_views.textures.get(&(id, "under".to_string())).expect("its texture").key;
        assert_ne!(key, id.0);
        let (_, _, named) = gpu.read_texels(key).expect("the view's texels");
        let (_, _, plain) = gpu.read_texels(id.0).expect("the document's texels");
        assert!(named[0][0] > 0.9 && named[0][2] < 0.1, "the view shows red: {:?}", named[0]);
        assert!(plain[0][2] > 0.9 && plain[0][0] < 0.1, "the document shows blue: {:?}", plain[0]);
        assert!(tile_views_bytes(&app, id) >= 32 * 32 * 4);
        // A tile's texture survives the frame's retain, and goes when freed.
        run(&ctx, &mut app, &tiles, TileCamera::Shared, Vec::new());
        assert!(gpu.texture_info(key).is_some());
        free_document_tile_views(&mut app, id);
        assert!(gpu.texture_info(key).is_none());
    }

    #[test]
    fn plan_gives_one_live_tile_and_counts_slots() {
        let a = DocId(1);
        let b = DocId(2);
        let tiles = [tile(a, 0.0, None), tile(a, 0.0, None), tile(a, 0.0, Some("x")), tile(b, 0.0, None), tile(a, 0.0, Some("x"))];
        let plans = plan(&tiles, &[a, b], Some(0), |_, _| true);
        assert!(plans[0].live && plans[0].first && plans[0].slot == 0);
        // The live tile has the document's camera wherever it stands.
        let moved = plan(&[tile(a, 0.0, Some("x")), tile(a, 0.0, None)], &[a], Some(0), |_, _| true);
        assert!(!moved[0].first && moved[1].first && moved[1].live);
        assert!(!plans[1].live && !plans[1].first && plans[1].slot == 1);
        assert!(!plans[2].live && plans[2].slot == 0);
        assert!(plans[3].first && !plans[3].live);
        assert_eq!(plans[4].slot, 1);
    }
}
