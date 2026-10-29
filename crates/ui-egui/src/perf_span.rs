//! Named spans of a frame's work, so a frame that froze the page says what it spent its time on.
//!
//! `let _s = perf_span::span("canvas.refresh");` times the code until the guard drops. Spans nest:
//! each records its *self* time (its own duration less its spans'), summed by name over the frame.
//! The shell's freeze watchdog reads the frame's spans ([`frame`]) when a frame ran long and puts
//! the largest one into the freeze report (`phase`) and the console. [`begin_frame`] clears them.
//! Costs two clock reads per span; frames have a few dozen.

use std::cell::RefCell;

/// The spans of a frame, at most this many distinct names (more are folded into the last one).
const MAX_NAMES: usize = 64;

#[derive(Default)]
struct State {
    /// Open spans: the time their children took so far.
    stack: Vec<f64>,
    /// Self time by name, this frame.
    frame: Vec<(&'static str, f64)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// A running span; records itself when dropped.
pub struct Span {
    name: &'static str,
    start: f64,
}

/// Starts timing `name` (a static, dotted id: `canvas.refresh`, `job.apply`).
pub fn span(name: &'static str) -> Span {
    STATE.with(|s| s.borrow_mut().stack.push(0.0));
    Span { name, start: crate::gpu_canvas::now_ms() }
}

impl Drop for Span {
    fn drop(&mut self) {
        let total = (crate::gpu_canvas::now_ms() - self.start).max(0.0);
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let children = s.stack.pop().unwrap_or(0.0);
            if let Some(parent) = s.stack.last_mut() {
                *parent += total;
            }
            let own = (total - children).max(0.0);
            if let Some(e) = s.frame.iter_mut().find(|e| e.0 == self.name) {
                e.1 += own;
            } else if s.frame.len() < MAX_NAMES {
                s.frame.push((self.name, own));
            } else if let Some(last) = s.frame.last_mut() {
                last.1 += own;
            }
        });
    }
}

/// A new frame: forget the last one's spans (open spans keep running).
pub fn begin_frame() {
    STATE.with(|s| s.borrow_mut().frame.clear());
}

/// This frame's spans so far, the longest first.
pub fn frame() -> Vec<(&'static str, f64)> {
    let mut v = STATE.with(|s| s.borrow().frame.clone());
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

/// The span this frame spent the most time in, when it took at least `min_ms`.
pub fn top(min_ms: f64) -> Option<&'static str> {
    frame().first().filter(|e| e.1 >= min_ms).map(|e| e.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(ms: f64) {
        let t = crate::gpu_canvas::now_ms();
        while crate::gpu_canvas::now_ms() - t < ms {}
    }

    #[test]
    fn spans_record_their_own_time_and_the_longest_comes_first() {
        begin_frame();
        {
            let _a = span("outer");
            busy(5.0);
            {
                let _b = span("inner");
                busy(30.0);
            }
        }
        {
            let _b = span("inner");
            busy(5.0);
        }
        let f = frame();
        assert_eq!(f[0].0, "inner");
        assert!(f[0].1 >= 35.0 && f[1].1 < 20.0, "{f:?}");
        assert_eq!(top(20.0), Some("inner"));
        assert_eq!(top(1_000.0), None);
        begin_frame();
        assert!(frame().is_empty());
    }
}
