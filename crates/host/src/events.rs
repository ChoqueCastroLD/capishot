//! What changed: [`Event`]s, derived by [`crate::Host::poll`] from the session's state compared
//! with the last look (the same comparisons the web embed's events make, from
//! `photocraft_engine::doc_events` and `pixel_revs`). Each kind is reported at most once per
//! poll per layer or document, and only on change; the first poll is the baseline (no events:
//! read the state then).

use std::collections::HashMap;
use std::sync::Arc;

use photocraft_engine::Session;
use photocraft_engine::doc::{Document, LayerContent};
use photocraft_engine::doc_events::{self, LayerOrder, LayerProps};
use photocraft_engine::jobs::{JobEvent, JobOutcome};
use serde::Serialize;
use serde_json::Value;

use crate::{DocumentId, JobHandle, LayerId, LayerUid};

/// Who made a change: this process's own calls and the person using the editor (`Local`), or a
/// collaborator's op applied with [`crate::Host::live_apply`] (`Remote`). Live sync sends only
/// local changes back out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    /// This process (calls, the person's edits, jobs, undo/redo).
    Local,
    /// A remote op applied with [`crate::Host::live_apply`].
    Remote,
}

/// How a background job ended.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum JobEnd {
    /// Applied; the command's result.
    Done {
        /// The command's result.
        result: Value,
    },
    /// Failed; the document is unchanged.
    Failed {
        /// Why.
        error: String,
    },
    /// Cancelled; the document is unchanged.
    Cancelled,
}

/// A change the host is told about. Serialises as the embed API's events do (`type` plus
/// camelCase fields), so a host can forward them as JSON.
///
/// `#[non_exhaustive]`: new event kinds may be added in a minor release; match with a `_` arm.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Event {
    /// A layer's pixels or layer mask changed (any cause: strokes, fills, Remove, filters, undo,
    /// jobs). `rect` `[x, y, width, height]` in document pixels, tile-aligned, the union of what
    /// changed since the last poll (it may reach past the canvas).
    LayerPixelsChanged {
        /// The document.
        document: DocumentId,
        /// The layer.
        layer: LayerId,
        /// The layer's stable id.
        uid: Option<LayerUid>,
        /// The changed area.
        rect: [i32; 4],
        /// The layer's pixel revision now (`document.inspect`'s `pixelRev`).
        pixel_rev: u64,
        /// Who changed it.
        origin: Origin,
    },
    /// A layer's settings, type, shape or adjustment changed, or it was added or removed.
    /// `what`: see `photocraft_engine::doc_events::LayerChange::what`.
    LayersChanged {
        /// The document.
        document: DocumentId,
        /// The layer.
        layer: LayerId,
        /// The layer's stable id.
        uid: Option<LayerUid>,
        /// What changed (`added`, `removed`, `name`, `visible`, `content`…).
        what: Vec<String>,
        /// Who changed it.
        origin: Origin,
    },
    /// The layers kept in a group (`None`: the top level) changed order; `layers` are its
    /// children now, top to bottom.
    LayersReordered {
        /// The document.
        document: DocumentId,
        /// The group, `None` for the top level.
        group: Option<LayerId>,
        /// The group's children now, top to bottom.
        layers: Vec<LayerId>,
        /// Who changed it.
        origin: Origin,
    },
    /// A type layer's text or its style changed (also reported as `LayersChanged` with
    /// `content`).
    TextChanged {
        /// The document.
        document: DocumentId,
        /// The type layer.
        layer: LayerId,
        /// The layer's stable id.
        uid: Option<LayerUid>,
        /// Who changed it.
        origin: Origin,
    },
    /// The selection changed; `bounds` `[x, y, width, height]`, `None` without one.
    SelectionChanged {
        /// The document.
        document: DocumentId,
        /// The selection's bounds.
        bounds: Option<[i32; 4]>,
    },
    /// The tool changed ([`crate::Host::set_tool`], or the widget's tool).
    ToolChanged {
        /// The tool id.
        tool: String,
    },
    /// The document's undo history changed (a step recorded, undone or redone).
    HistoryChanged {
        /// The document.
        document: DocumentId,
        /// Steps that can be undone.
        undo_steps: usize,
        /// Whether something can be redone.
        can_redo: bool,
        /// The current step's label.
        current: String,
    },
    /// The document changed (its revision moved).
    DocumentChanged {
        /// The document.
        document: DocumentId,
        /// Its revision now.
        revision: u64,
        /// Unsaved changes.
        dirty: bool,
    },
    /// The open documents or the active one changed (hidden documents are not listed).
    DocumentsChanged {
        /// The shown documents, in tab order.
        documents: Vec<DocumentId>,
        /// The active document.
        active: Option<DocumentId>,
    },
    /// A background job ended.
    JobFinished {
        /// The job.
        job: JobHandle,
        /// Its command id.
        command: String,
        /// The document it worked on.
        document: Option<DocumentId>,
        /// How it ended.
        end: JobEnd,
    },
}

impl Event {
    /// The event as JSON (`{"type": "layerPixelsChanged", "document": 1, …}`).
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

pub(crate) fn job_event(e: JobEvent) -> Event {
    let end = match e.outcome {
        JobOutcome::Done(result) => JobEnd::Done { result },
        JobOutcome::Failed(error) => JobEnd::Failed { error },
        JobOutcome::Cancelled => JobEnd::Cancelled,
    };
    Event::JobFinished { job: e.id.into(), command: e.command, document: e.document.map(Into::into), end }
}

/// One document as last seen.
#[derive(Clone)]
struct Look {
    id: u64,
    revision: u64,
    dirty: bool,
    hidden: bool,
    selection: Option<[i32; 4]>,
    order: Arc<LayerOrder>,
    props: Arc<Vec<LayerProps>>,
    uids: Arc<HashMap<u64, u32>>,
    history: (usize, bool, String),
}

fn uids(doc: &Document) -> HashMap<u64, u32> {
    doc.walk().into_iter().filter_map(|(_, _, l)| l.psd_id.map(|u| (l.id.0, u))).collect()
}

/// The state [`crate::Host::poll`] compares against.
#[derive(Default)]
pub(crate) struct Tracker {
    started: bool,
    docs: Vec<Look>,
    shown: Vec<u64>,
    active: Option<u64>,
    tool: String,
}

impl Tracker {
    /// The events from the last look to `session` now, tagged `origin`; the look is updated.
    pub(crate) fn collect(&mut self, session: &Session, tool: &str, origin: Origin) -> Vec<Event> {
        let mut out = Vec::new();
        let mut looks = Vec::with_capacity(session.documents().len());
        for st in session.documents() {
            let id = st.doc.id.0;
            let document = DocumentId(id);
            let prev = self.docs.iter().find(|l| l.id == id);
            // Pixel changes are read from the tracker the document keeps (copy-on-write tiles:
            // pointer checks). The first look sets its baseline.
            let changes = st.pixel_revs.take_changes(&st.doc);
            let same = prev.is_some_and(|p| p.revision == st.revision);
            let look = match prev {
                Some(p) if same => Look { hidden: st.hidden, dirty: st.revision != st.saved_revision, ..p.clone() },
                _ => Look {
                    id,
                    revision: st.revision,
                    dirty: st.revision != st.saved_revision,
                    hidden: st.hidden,
                    selection: doc_events::selection_bounds(&st.doc),
                    order: Arc::new(doc_events::layer_order(&st.doc)),
                    props: Arc::new(doc_events::layer_props(&st.doc)),
                    uids: Arc::new(uids(&st.doc)),
                    history: (st.history.past_len(), st.history.can_redo(), st.history.entries().last().cloned().unwrap_or_default()),
                },
            };
            let uid = |l: u64| look.uids.get(&l).or_else(|| prev.and_then(|p| p.uids.get(&l))).copied().map(LayerUid);
            if self.started {
                for c in &changes {
                    out.push(Event::LayerPixelsChanged {
                        document,
                        layer: c.layer.into(),
                        uid: uid(c.layer.0),
                        rect: [c.rect.x0, c.rect.y0, i32::try_from(c.rect.width()).unwrap_or(i32::MAX), i32::try_from(c.rect.height()).unwrap_or(i32::MAX)],
                        pixel_rev: c.rev,
                        origin,
                    });
                }
            }
            if let Some(p) = prev.filter(|_| self.started && !same) {
                if p.revision != look.revision {
                    out.push(Event::DocumentChanged { document, revision: look.revision, dirty: look.dirty });
                }
                for (group, layers) in doc_events::reordered_groups(&p.order, &look.order) {
                    out.push(Event::LayersReordered { document, group: group.map(LayerId), layers: layers.into_iter().map(LayerId).collect(), origin });
                }
                for c in doc_events::layer_changes(&p.props, &look.props) {
                    let text = c.what.contains(&"content") && look.props.iter().any(|l| l.id == c.layer && matches!(l.content, Some(LayerContent::Text(_))));
                    out.push(Event::LayersChanged {
                        document,
                        layer: LayerId(c.layer),
                        uid: uid(c.layer),
                        what: c.what.iter().map(|w| (*w).to_string()).collect(),
                        origin,
                    });
                    if text {
                        out.push(Event::TextChanged { document, layer: LayerId(c.layer), uid: uid(c.layer), origin });
                    }
                }
                if p.selection != look.selection {
                    out.push(Event::SelectionChanged { document, bounds: look.selection });
                }
                if p.history != look.history {
                    let (undo_steps, can_redo, current) = look.history.clone();
                    out.push(Event::HistoryChanged { document, undo_steps, can_redo, current });
                }
            } else if let Some(p) = prev.filter(|p| self.started && p.dirty != look.dirty) {
                // A save: only the dirty flag moved.
                out.push(Event::DocumentChanged { document, revision: p.revision, dirty: look.dirty });
            }
            looks.push(look);
        }
        let shown: Vec<u64> = looks.iter().filter(|l| !l.hidden).map(|l| l.id).collect();
        let active = session.active().map(|st| st.doc.id.0);
        if self.started && (shown != self.shown || active != self.active) {
            out.push(Event::DocumentsChanged { documents: shown.iter().copied().map(DocumentId).collect(), active: active.map(DocumentId) });
        }
        if self.started && tool != self.tool {
            out.push(Event::ToolChanged { tool: tool.to_string() });
        }
        self.docs = looks;
        self.shown = shown;
        self.active = active;
        self.tool = tool.to_string();
        self.started = true;
        out
    }

    /// The layer's uid as last seen (also for a layer removed since).
    pub(crate) fn uid_of(&self, document: DocumentId, layer: LayerId) -> Option<LayerUid> {
        self.docs.iter().find(|l| l.id == document.0)?.uids.get(&layer.0).copied().map(LayerUid)
    }
}
