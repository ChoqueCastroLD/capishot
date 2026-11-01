//! Downloads written a piece at a time ([`photocraft_ui_egui::DownloadStream`]): each piece is
//! appended to a Blob as it comes, so an export made of many pages (a ZIP) never sits whole in the
//! app's memory; the browser holds the Blob (and may keep it on disk). Pieces from a worker (the
//! threads build) are queued and appended on the page's thread each frame ([`drain`]); in the
//! single-threaded build they are appended at once. A stream dropped before it finished is
//! discarded: nothing is downloaded.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use photocraft_ui_egui::DownloadStream;

enum Msg {
    Chunk(Vec<u8>),
    Finish,
    Abort,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static QUEUE: std::sync::Mutex<Vec<(u64, Msg)>> = std::sync::Mutex::new(Vec::new());

thread_local! {
    /// Open streams on the page's thread: the download's name and the Blob so far.
    static STREAMS: RefCell<HashMap<u64, (String, Option<web_sys::Blob>)>> = RefCell::new(HashMap::new());
}

struct Stream {
    id: u64,
    done: bool,
}

/// Starts a stream for the download `name` (on the page's thread).
pub fn open(name: &str) -> Result<Box<dyn DownloadStream>, String> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    STREAMS.with(|s| s.borrow_mut().insert(id, (name.to_string(), None)));
    Ok(Box::new(Stream { id, done: false }))
}

fn send(id: u64, msg: Msg) -> Result<(), String> {
    if cfg!(target_feature = "atomics") {
        QUEUE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((id, msg));
        Ok(())
    } else {
        apply(id, msg)
    }
}

fn apply(id: u64, msg: Msg) -> Result<(), String> {
    match msg {
        Msg::Chunk(bytes) => STREAMS.with(|s| {
            let mut s = s.borrow_mut();
            let Some((name, blob)) = s.get_mut(&id) else { return Ok(()) };
            let parts = js_sys::Array::new();
            if let Some(b) = blob.take() {
                parts.push(&b);
            }
            parts.push(&js_sys::Uint8Array::from(bytes.as_slice()));
            *blob = Some(crate::web::blob_of(name, &parts)?);
            Ok(())
        }),
        Msg::Finish => {
            let Some((name, blob)) = STREAMS.with(|s| s.borrow_mut().remove(&id)) else { return Ok(()) };
            let blob = match blob {
                Some(b) => b,
                None => crate::web::blob_of(&name, &js_sys::Array::new())?,
            };
            crate::web::deliver_blob(&name, &blob)
        }
        Msg::Abort => {
            STREAMS.with(|s| s.borrow_mut().remove(&id));
            Ok(())
        }
    }
}

/// Appends the pieces workers queued (each frame, on the page's thread).
pub fn drain() {
    let msgs = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    for (id, m) in msgs {
        if let Err(e) = apply(id, m) {
            log::warn!("download stream: {e}");
            STREAMS.with(|s| s.borrow_mut().remove(&id));
        }
    }
}

impl DownloadStream for Stream {
    fn push(&mut self, chunk: Vec<u8>) -> Result<(), String> {
        send(self.id, Msg::Chunk(chunk))
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        self.done = true;
        send(self.id, Msg::Finish)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.done {
            let _ = send(self.id, Msg::Abort);
        }
    }
}
