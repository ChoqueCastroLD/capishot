//! The events both embedding APIs push (`embed_api.rs`'s `onEvent`, `embed.rs`'s
//! `window.photocraft` window events and `postMessage`): derived once per frame, after the frame's
//! UI, from the session; each kind at most once per frame and only on change (`embed/README.md`).
//! The logic is in `embed_api_core.rs` (tested natively); this keeps the state between frames.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use photocraft_ui_egui::PhotocraftApp;
use serde_json::{Value, json};
use wasm_bindgen::JsCast as _;

use crate::embed_api_core::{self as core, DocSnap, LayerOrder, LayerProps, ReadyTracker, Snapshot, ViewSnap};

/// Where events go.
pub type Sink = fn(&Value);

/// The view last reported, the latest one not reported yet, and whether an animation frame is
/// asked for to report it.
#[derive(Default)]
struct ViewState {
    reported: Option<ViewSnap>,
    next: Option<ViewSnap>,
    asked: bool,
}

/// What the events compare from one frame to the next.
pub struct Events {
    sink: Sink,
    last: Option<Snapshot>,
    /// Per document, with the revision they were read at: selection bounds, stacking order and
    /// layer settings (each read once per revision, not per frame).
    selections: Vec<(u64, u64, Option<[i32; 4]>)>,
    orders: Vec<(u64, u64, Arc<LayerOrder>)>,
    props: Vec<(u64, u64, Arc<Vec<LayerProps>>)>,
    ready: ReadyTracker,
    view: Rc<RefCell<ViewState>>,
    view_frame: Option<wasm_bindgen::closure::Closure<dyn FnMut()>>,
}

impl Events {
    pub fn new(sink: Sink) -> Self {
        Self {
            sink,
            last: None,
            selections: Vec::new(),
            orders: Vec::new(),
            props: Vec::new(),
            ready: ReadyTracker::default(),
            view: Rc::default(),
            view_frame: None,
        }
    }

    /// An `open` reached the app (`documentReady` times count from here).
    pub fn note_open(&mut self) {
        self.ready.note_open(crate::frame_perf::now_ms());
    }

    /// The open just noted failed.
    pub fn open_failed(&mut self) {
        self.ready.open_failed();
    }

    /// The frame's events, after its UI (where pans, zooms and rotations happen) and the calls
    /// run before it. `ppp`: device pixels per point.
    pub fn report(&mut self, app: &mut PhotocraftApp, ppp: f32) {
        let sink = self.sink;
        // Layers whose pixels or mask changed (compared tile by tile with the document seen last).
        for st in app.session.documents() {
            for c in st.pixel_revs.take_changes(&st.doc) {
                sink(&core::pixels_changed_event(st.doc.id.0, &c));
            }
        }
        // Opened documents whose fonts settled (their type drawn with them).
        for mut e in std::mem::take(&mut app.font_checks.ready) {
            e["type"] = json!("fontsReady");
            sink(&e);
        }
        // Unchanged documents keep their cached order and settings (the same `Arc`s), so the
        // comparison is a pointer check per document.
        let snap = self.snapshot(app, ppp);
        for e in core::events(self.last.as_ref(), &snap) {
            sink(&e);
        }
        self.last = Some(snap);
        let docs: Vec<u64> = app.session.documents().iter().map(|st| st.doc.id.0).collect();
        let ready = self.ready.frame(crate::frame_perf::now_ms(), &docs, |id| photocraft_ui_egui::doc_ready::complete(app, photocraft_engine::doc::DocId(id)));
        for r in ready {
            sink(&core::ready_event(r.id, r.first_ms, r.final_ms));
        }
        let Some(view) = self.last.as_ref().and_then(|s| s.view.clone()) else { return };
        let ask = {
            let mut v = self.view.borrow_mut();
            let changed = v.reported.as_ref() != Some(&view);
            v.next = changed.then_some(view);
            changed && !std::mem::replace(&mut v.asked, true)
        };
        if ask {
            self.ask_view_frame();
        }
    }

    /// Reports the latest view on the next animation frame: a gesture that runs several app
    /// frames in one browser frame still sends one `viewChanged` / `viewportChanged`.
    fn ask_view_frame(&mut self) {
        let Some(window) = web_sys::window() else { return };
        let (state, sink) = (self.view.clone(), self.sink);
        let cb = self.view_frame.get_or_insert_with(|| {
            wasm_bindgen::closure::Closure::new(move || {
                let events = {
                    let mut v = state.borrow_mut();
                    v.asked = false;
                    match v.next.take() {
                        Some(next) => {
                            let events = core::view_events(v.reported.as_ref(), &next);
                            v.reported = Some(next);
                            events
                        }
                        None => Vec::new(),
                    }
                };
                for e in events {
                    sink(&e);
                }
            })
        });
        if window.request_animation_frame(cb.as_ref().unchecked_ref()).is_err() {
            self.view.borrow_mut().asked = false;
        }
    }

    fn snapshot(&mut self, app: &PhotocraftApp, ppp: f32) -> Snapshot {
        let mut snaps = Vec::new();
        for st in app.session.documents() {
            let (id, rev) = (st.doc.id.0, st.revision);
            // Measuring a selection walks its tiles: once per revision, not per frame.
            let selection = cached(&mut self.selections, id, rev, || {
                photocraft_engine::doc_events::selection_bounds(&st.doc)
            });
            let order = cached(&mut self.orders, id, rev, || Arc::new(core::layer_order(&st.doc)));
            let props = cached(&mut self.props, id, rev, || Arc::new(core::layer_props(&st.doc)));
            snaps.push(DocSnap { id, revision: rev, dirty: rev != st.saved_revision, selection, order, props, hidden: st.hidden });
        }
        let open = |d: &u64| snaps.iter().any(|s| s.id == *d);
        self.selections.retain(|(d, ..)| open(d));
        self.orders.retain(|(d, ..)| open(d));
        self.props.retain(|(d, ..)| open(d));
        let active = app.session.active().map(|st| st.doc.id.0);
        let canvas = app.last_canvas_rect.size() * ppp;
        let view = app.session.active_index().zip(active).and_then(|(i, doc)| {
            let v = app.ui.views.get(i).filter(|v| !v.fit_pending && !v.fill_pending)?;
            Some(ViewSnap { doc, zoom: v.zoom, center: v.center, rotation: v.rotation, canvas: [canvas.x, canvas.y] })
        });
        Snapshot { active, docs: snaps, tool: app.ui.tool.id(), error: app.ui.status_error.then(|| app.ui.status.clone()), view }
    }
}

/// `cache`'s value for document `id` at revision `rev`, made by `make` when it has none.
fn cached<T: Clone>(cache: &mut Vec<(u64, u64, T)>, id: u64, rev: u64, make: impl FnOnce() -> T) -> T {
    if let Some((_, _, v)) = cache.iter().find(|(d, r, _)| *d == id && *r == rev) {
        return v.clone();
    }
    let v = make();
    cache.retain(|(d, ..)| *d != id);
    cache.push((id, rev, v.clone()));
    v
}

