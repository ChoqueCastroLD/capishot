//! Startup timings: what the page spends between navigation and the first frame, logged once
//! to the console (`photocraft-web: startup …`) so a slow start can be told apart from a slow
//! download. Times are `performance.now()`: milliseconds since navigation started.

use std::cell::RefCell;

thread_local! {
    static MARKS: RefCell<Vec<(&'static str, f64)>> = const { RefCell::new(Vec::new()) };
}

fn now() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
}

/// Records that startup reached `step`.
pub fn mark(step: &'static str) {
    let t = now();
    MARKS.with_borrow_mut(|m| m.push((step, t)));
}

/// Logs the steps recorded so far as one line (step at time since navigation, and the time
/// since the previous step), once: later calls do nothing.
pub fn report() {
    let marks = MARKS.with_borrow_mut(std::mem::take);
    let Some(&(_, first)) = marks.first() else { return };
    let mut line = format!("photocraft-web: startup: wasm running at {first:.0} ms");
    let mut prev = first;
    for (step, t) in marks.iter().skip(1) {
        line.push_str(&format!(", {step} {t:.0} ms (+{:.0})", t - prev));
        prev = *t;
    }
    log::info!("{line}");
}
