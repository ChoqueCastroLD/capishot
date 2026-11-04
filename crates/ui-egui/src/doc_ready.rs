//! When a document is complete on screen: the web embed's `documentReady` event and the host
//! API's `DocumentReady` (`capishot-host`). A document is new the first frame it is among the
//! documents watched (the web: every open document; a host: the shown one, so a document shown
//! again is new again); it is ready the first frame it is complete (decoded, its fonts settled,
//! nothing working on it, and for hosts its canvas drawn).

use std::collections::VecDeque;

use photocraft_doc::DocId;

use crate::PhotocraftApp;

/// How long an `open` waits to be matched with the document it makes (ms): one that failed
/// without saying so is forgotten then.
const OPEN_MATCH_MS: f64 = 120_000.0;

/// A document became ready.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ready {
    /// The document (`DocId.0`).
    pub id: u64,
    /// From when it was asked for (an open, a show, else the frame it appeared in) to the end of
    /// the first frame that showed it (ms).
    pub first_ms: f64,
    /// From when it was asked for to the end of the first frame it was complete in (ms).
    pub final_ms: f64,
}

/// `documentReady` bookkeeping: when each open was asked for, and the documents not complete yet.
#[derive(Debug)]
pub struct ReadyTracker {
    /// Opens asked for and not matched with a document yet, oldest first (ms).
    opens: VecDeque<f64>,
    /// Documents asked for by id (a host's show) and when (ms).
    asked: Vec<(u64, f64)>,
    /// `(document, asked at, first frame at)` of documents not complete yet.
    pending: Vec<(u64, f64, f64)>,
    /// The documents seen so far (a new one is one not in it).
    seen: Vec<u64>,
    started: bool,
    /// The first frame's documents are the baseline (not reported).
    baseline: bool,
}

impl Default for ReadyTracker {
    fn default() -> Self {
        ReadyTracker { opens: VecDeque::new(), asked: Vec::new(), pending: Vec::new(), seen: Vec::new(), started: false, baseline: true }
    }
}

impl ReadyTracker {
    /// A tracker that also reports the documents of its first frame.
    pub fn without_baseline() -> Self {
        ReadyTracker { baseline: false, ..Self::default() }
    }

    /// An `open` / `openDocument` reached the app at `now` (ms).
    pub fn note_open(&mut self, now: f64) {
        self.opens.push_back(now);
        if self.opens.len() > 64 {
            self.opens.pop_front();
        }
    }

    /// The last open noted failed: it makes no document.
    pub fn open_failed(&mut self) {
        self.opens.pop_back();
    }

    /// Document `id` was asked for at `now` (ms): the next time it is new, its times count from
    /// here.
    pub fn note_asked(&mut self, id: u64, now: f64) {
        self.asked.retain(|(d, _)| *d != id);
        self.asked.push((id, now));
        if self.asked.len() > 256 {
            self.asked.remove(0);
        }
    }

    /// The end of a frame at `now` (ms) with the documents watched; `complete` is asked only about
    /// documents not ready yet. Returns the documents ready in this frame.
    pub fn frame(&mut self, now: f64, docs: &[u64], mut complete: impl FnMut(u64) -> bool) -> Vec<Ready> {
        while self.opens.front().is_some_and(|t| now - t > OPEN_MATCH_MS) {
            self.opens.pop_front();
        }
        self.asked.retain(|(_, t)| now - t <= OPEN_MATCH_MS);
        let report = std::mem::replace(&mut self.started, true) || !self.baseline;
        for id in docs {
            if self.seen.contains(id) {
                continue;
            }
            self.seen.push(*id);
            if report {
                let by_id = self.asked.iter().position(|(d, _)| d == id).map(|i| self.asked.remove(i).1);
                let asked = by_id.or_else(|| self.opens.pop_front()).unwrap_or(now);
                self.pending.push((*id, asked, now));
            }
        }
        self.seen.retain(|id| docs.contains(id));
        let mut out = Vec::new();
        self.pending.retain(|&(id, asked, first)| {
            if !docs.contains(&id) {
                return false;
            }
            if !complete(id) {
                return true;
            }
            out.push(Ready { id, first_ms: first - asked, final_ms: now - asked });
            false
        });
        out
    }
}

/// Is the document complete: no job works on it (a stepped open, a background filter) and the
/// fonts its type uses have arrived or failed (that type drawn with them).
pub fn complete(app: &PhotocraftApp, doc: DocId) -> bool {
    app.session.job_on(doc).is_none() && crate::user_fonts_ui::fonts_settled(app, doc)
}

/// [`complete`], and its canvas is drawn for the view it has (all of what shows is on screen).
pub fn complete_on_screen(app: &mut PhotocraftApp, ctx: &egui::Context, doc: DocId) -> bool {
    let Some(index) = app.session.index_of_document(doc) else { return false };
    complete(app, doc) && crate::canvas::prewarm(app, ctx, index, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(r: &mut ReadyTracker, now: f64, docs: &[(u64, bool)]) -> Vec<Ready> {
        let ids: Vec<u64> = docs.iter().map(|(d, _)| *d).collect();
        r.frame(now, &ids, |id| docs.iter().any(|(d, c)| *d == id && *c))
    }

    fn ready(id: u64, first_ms: f64, final_ms: f64) -> Ready {
        Ready { id, first_ms, final_ms }
    }

    #[test]
    fn documents_are_ready_once_complete_timed_from_their_open() {
        let mut r = ReadyTracker::default();
        assert!(frame(&mut r, 0.0, &[(1, true)]).is_empty(), "the first frame is the baseline");
        r.note_open(10.0);
        assert!(frame(&mut r, 16.0, &[(1, true)]).is_empty(), "still opening");
        assert!(frame(&mut r, 40.0, &[(1, true), (2, false)]).is_empty(), "shown, not complete");
        assert_eq!(frame(&mut r, 90.0, &[(1, true), (2, true)]), [ready(2, 30.0, 80.0)]);
        assert!(frame(&mut r, 100.0, &[(1, true), (2, true)]).is_empty(), "once");
        // A document made without an open counts from the frame it appeared in.
        assert_eq!(frame(&mut r, 120.0, &[(1, true), (2, true), (3, true)]), [ready(3, 0.0, 0.0)]);
        // A failed open isn't matched with the next document; a closed one is dropped.
        r.note_open(130.0);
        r.open_failed();
        r.note_open(200.0);
        assert!(frame(&mut r, 210.0, &[(4, false)]).is_empty());
        assert!(frame(&mut r, 220.0, &[]).is_empty());
    }

    #[test]
    fn a_shown_document_is_new_each_time_it_is_shown_and_counts_from_the_show() {
        let mut r = ReadyTracker::without_baseline();
        assert_eq!(frame(&mut r, 5.0, &[(1, true)]), [ready(1, 0.0, 0.0)], "no baseline");
        r.note_asked(2, 10.0);
        assert!(frame(&mut r, 20.0, &[(2, false)]).is_empty());
        assert_eq!(frame(&mut r, 50.0, &[(2, true)]), [ready(2, 10.0, 40.0)]);
        // Back to 1: new again.
        assert_eq!(frame(&mut r, 60.0, &[(1, true)]), [ready(1, 0.0, 0.0)]);
    }

    #[test]
    fn complete_is_asked_only_about_pending_documents() {
        let mut r = ReadyTracker::without_baseline();
        let mut asked = Vec::new();
        r.frame(0.0, &[1], |id| {
            asked.push(id);
            true
        });
        r.frame(1.0, &[1], |id| {
            asked.push(id);
            true
        });
        assert_eq!(asked, [1]);
    }
}
