//! What [`crate::Host::show`] and [`crate::Host::prewarm`] tell the widget, and the widget's own
//! requests, queued for its next frame (the session is only lent to the widget while it draws).

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};

use super::DocumentCamera;
use crate::{DocumentId, Error, Result, ViewHooks};
use photocraft_ui_egui::embed::CameraChange;

/// Why a control reply is awaited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Awaited {
    /// `editor.showDocument`.
    Show(DocumentId),
    /// `document.prewarm`.
    Prewarm(DocumentId),
    /// Anything else, answered as [`super::ViewEvent::Reply`].
    Reply,
}

/// A request for the next frame.
#[derive(Clone, Debug)]
pub(crate) enum Request {
    /// A control-channel method (`docs/control-protocol.md`).
    Control { token: u64, method: String, params: Value, awaited: Awaited },
    /// Move a document's camera.
    Camera { document: DocumentId, change: CameraChange },
    /// The host's [`crate::Host::show`] swapped documents in the session already: give the new
    /// one its camera.
    Shown { document: DocumentId, camera: DocumentCamera },
}

/// The queue the widget and its [`ViewHooks`] share.
#[derive(Clone, Default)]
pub(crate) struct Queue {
    inner: Rc<RefCell<QueueState>>,
}

#[derive(Default)]
struct QueueState {
    requests: Vec<Request>,
    next_token: u64,
    /// The camera [`crate::Host::show`] gives the shown document.
    show_camera: DocumentCamera,
}

impl Queue {
    pub(crate) fn push(&self, r: Request) {
        self.inner.borrow_mut().requests.push(r);
    }

    pub(crate) fn take(&self) -> Vec<Request> {
        std::mem::take(&mut self.inner.borrow_mut().requests)
    }

    pub(crate) fn token(&self) -> u64 {
        let mut q = self.inner.borrow_mut();
        q.next_token = q.next_token.wrapping_add(1);
        q.next_token
    }

    pub(crate) fn control(&self, method: &str, params: Value, awaited: Awaited) -> u64 {
        let token = self.token();
        self.push(Request::Control { token, method: method.into(), params, awaited });
        token
    }

    pub(crate) fn set_show_camera(&self, c: DocumentCamera) {
        self.inner.borrow_mut().show_camera = c;
    }

    fn show_camera(&self) -> DocumentCamera {
        self.inner.borrow().show_camera
    }
}

/// The [`ViewHooks`] the widget installs in its [`crate::Host`].
pub(crate) struct Hooks {
    pub(crate) queue: Queue,
}

impl ViewHooks for Hooks {
    fn show(&mut self, document: DocumentId) {
        let camera = self.queue.show_camera();
        self.queue.push(Request::Shown { document, camera });
    }

    fn prewarm(&mut self, document: DocumentId, options: &Value) -> Result<bool> {
        // `document.prewarm`'s own fields (`viewport`, `full`, `timeoutMs`), checked when it runs.
        let mut params = match options {
            Value::Object(m) => Value::Object(m.clone()),
            Value::Null => json!({}),
            _ => {
                return Err(Error::BadParams { command: "document.prewarm".into(), message: "options must be an object (viewport, full, timeoutMs)".into() });
            }
        };
        params["id"] = json!(document.0);
        self.queue.control("document.prewarm", params, Awaited::Prewarm(document));
        Ok(true)
    }
}
