//! Ids a host holds on to.

use serde::{Deserialize, Serialize};

/// An open document, for this session (the control protocol's `document` / `id`). Documents get
/// a new one each time they are opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentId(pub u64);

/// A layer's session id (`document.inspect`'s `id`, what commands' `layer` param takes). Layers
/// are renumbered on every open; [`LayerUid`] survives save and reopen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerId(pub u64);

/// A layer's stable id (`document.inspect`'s `uid`): the PSD layer id (`lyid`), kept in PSD and
/// `.pcraft` files, the same after save and reopen and the same for everyone who opened the same
/// file. Live sync ([`crate::live`]) addresses layers by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerUid(pub u32);

/// A background job ([`crate::Host::start`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobHandle(pub u64);

impl From<photocraft_doc::DocId> for DocumentId {
    fn from(id: photocraft_doc::DocId) -> Self {
        DocumentId(id.0)
    }
}

impl From<DocumentId> for photocraft_doc::DocId {
    fn from(id: DocumentId) -> Self {
        photocraft_doc::DocId(id.0)
    }
}

impl From<photocraft_doc::LayerId> for LayerId {
    fn from(id: photocraft_doc::LayerId) -> Self {
        LayerId(id.0)
    }
}

impl From<LayerId> for photocraft_doc::LayerId {
    fn from(id: LayerId) -> Self {
        photocraft_doc::LayerId(id.0)
    }
}

impl From<photocraft_engine::jobs::JobId> for JobHandle {
    fn from(id: photocraft_engine::jobs::JobId) -> Self {
        JobHandle(id.0)
    }
}

impl From<JobHandle> for photocraft_engine::jobs::JobId {
    fn from(id: JobHandle) -> Self {
        photocraft_engine::jobs::JobId(id.0)
    }
}
