//! A crash signal for the usage statistics: a page killed by the browser (out of memory, a
//! renderer crash) runs no code on the way out, so each page keeps a breadcrumb in localStorage
//! (its last command and tool, the memory high-water mark, a heartbeat) and removes it when it
//! goes away normally. A breadcrumb whose heartbeat stopped, found by the next load, is an
//! unclean exit: reported once as `unclean_exit` (when statistics are on) and logged.

use std::cell::{Cell, RefCell};

use photocraft_ui_egui::PhotocraftApp;
use serde_json::{Value, json};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

const PREFIX: &str = "capishot.crumb.";
/// Heartbeat period, and how old a breadcrumb must be to belong to a dead page (ms).
const BEAT_MS: f64 = 2000.0;
const DEAD_MS: f64 = 15_000.0;

thread_local! {
    static KEY: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST: Cell<f64> = const { Cell::new(0.0) };
    static HIGH: Cell<u64> = const { Cell::new(0) };
    static STARTED: Cell<f64> = const { Cell::new(0.0) };
    /// Unclean exits found at start, for the statistics.
    static FOUND: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The page's memory now (the WebAssembly memory, which never shrinks: also its high-water mark).
pub fn memory_bytes() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::memory_size(0) as u64 * 65536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// At start: collects the breadcrumbs of dead pages and starts this page's.
pub fn start() {
    let now = js_sys::Date::now();
    STARTED.with(|s| s.set(now));
    let Some(st) = storage() else { return };
    let len = st.length().unwrap_or(0);
    let keys: Vec<String> = (0..len).filter_map(|i| st.key(i).ok().flatten()).filter(|k| k.starts_with(PREFIX)).collect();
    for k in keys {
        let v: Option<Value> = st.get_item(&k).ok().flatten().and_then(|t| serde_json::from_str(&t).ok());
        let beat = v.as_ref().and_then(|v| v.get("t")).and_then(Value::as_f64).unwrap_or(0.0);
        if now - beat < DEAD_MS {
            continue; // another tab, alive
        }
        let _ = st.remove_item(&k);
        if let Some(v) = v {
            log::warn!("photocraft-web: the previous page didn't close cleanly: {v}");
            FOUND.with(|f| f.borrow_mut().push(v));
        }
    }
    let key = format!("{PREFIX}{:x}{:x}", now as u64, (js_sys::Math::random() * 1e9) as u64);
    KEY.with(|k| *k.borrow_mut() = Some(key));
    // A normal close (or entering the back/forward cache) is clean; coming back starts again.
    if let Some(window) = web_sys::window() {
        let gone = Closure::<dyn FnMut()>::new(|| {
            if let (Some(st), Some(k)) = (storage(), KEY.with(|k| k.borrow().clone())) {
                let _ = st.remove_item(&k);
            }
            LAST.with(|l| l.set(0.0));
        });
        let _ = window.add_event_listener_with_callback("pagehide", gone.as_ref().unchecked_ref());
        gone.forget();
    }
}

/// Every frame: the breadcrumb, every couple of seconds.
pub fn tick(app: &PhotocraftApp) {
    let mem = memory_bytes();
    HIGH.with(|h| h.set(h.get().max(mem)));
    let now = js_sys::Date::now();
    if now - LAST.with(Cell::get) < BEAT_MS {
        return;
    }
    LAST.with(|l| l.set(now));
    let (Some(st), Some(k)) = (storage(), KEY.with(|k| k.borrow().clone())) else { return };
    let crumb = json!({
        "t": now,
        "cmd": app.perf.last_command,
        "tool": format!("{:?}", app.ui.tool),
        "memMB": HIGH.with(Cell::get) >> 20,
        "upMin": ((now - STARTED.with(Cell::get)) / 60_000.0).round(),
        "docs": app.session.documents().len(),
    });
    let _ = st.set_item(&k, &crumb.to_string());
}

/// The unclean exits found at start, as statistics events (taken once).
pub fn take_events() -> Vec<Value> {
    FOUND.with(|f| std::mem::take(&mut *f.borrow_mut()))
        .into_iter()
        .map(|v| {
            json!({
                "e": "unclean_exit",
                "cmd": v.get("cmd").cloned().unwrap_or(Value::Null),
                "tool": v.get("tool").cloned().unwrap_or(Value::Null),
                "memMB": v.get("memMB").cloned().unwrap_or(Value::Null),
                "upMin": v.get("upMin").cloned().unwrap_or(Value::Null),
                "docs": v.get("docs").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}
