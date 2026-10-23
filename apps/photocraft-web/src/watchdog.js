// Long-task watchdog (watchdog.rs): the browser's `longtask` entries of at least 200 ms, kept
// until the app takes them. Nothing runs at import time (the worker pool imports this module).
let pending = [];
let started = false;

export function wd_start() {
  if (started) return;
  started = true;
  try {
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) if (e.duration >= 200 && pending.length < 100) pending.push(e.duration);
    }).observe({ type: 'longtask', buffered: true });
  } catch (e) {
    // Firefox and Safari have no long-task entries: the app's frame check still reports.
  }
}

export function wd_take() {
  const out = pending;
  pending = [];
  return out;
}
