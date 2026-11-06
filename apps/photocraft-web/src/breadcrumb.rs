//! A crash signal for the usage statistics: a page killed by the browser (out of memory, a
//! renderer crash) runs no code on the way out, so each page keeps a breadcrumb in localStorage
//! (its last command and tool, the memory high-water mark, a heartbeat) and removes it when it
//! goes away normally. A breadcrumb whose heartbeat stopped, found by the next load, is an
//! unclean exit: reported once as `unclean_exit` (when statistics are on) and logged.
//!
//! The breadcrumb also says what the page was doing ([`photocraft_ui_egui::telemetry::Stage`],
//! written as a stage starts: a page that runs out of memory decoding never reaches its next
//! heartbeat), the browser's memory, and whether it panicked and where ([`on_panic`]). A panic is
//! sent at once as `crash` too; the next load sends it only if that didn't happen.
//!
//! A silent breadcrumb doesn't prove a dead page: one in the background or behind the file picker
//! draws no frames, and one in a long task or still starting doesn't reach its heartbeat. So each
//! page holds a Web Lock named after its breadcrumb, which the browser releases when the page is
//! gone, and a breadcrumb whose lock is held is left alone. Where there are no locks, a live hidden
//! page can still be reported; its stage is then `hidden`, as for one really reclaimed from the
//! background, and the statistics don't count those as crashes ([`Stage::is_background`]).

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use js_sys::Reflect;
use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::telemetry::{self, Stage};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen::closure::Closure;

const PREFIX: &str = "capishot.crumb.";
/// Heartbeat period, and how old a breadcrumb must be to belong to a dead page (ms).
const BEAT_MS: f64 = 2000.0;
const DEAD_MS: f64 = 15_000.0;
/// Unclean exits in a row: reset by a page that closes normally.
const LOOP_KEY: &str = "capishot.crashLoop";

/// This page's first panic: its location and class. Static, as a worker can panic too: it has no
/// storage, so the page's next heartbeat writes it.
static PANIC: OnceLock<(String, &'static str)> = OnceLock::new();
/// The panic's `crash` event reached the browser's beacon queue.
static PANIC_SENT: AtomicBool = AtomicBool::new(false);

thread_local! {
    static KEY: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST: Cell<f64> = const { Cell::new(0.0) };
    static HIGH: Cell<u64> = const { Cell::new(0) };
    static STARTED: Cell<f64> = const { Cell::new(0.0) };
    /// The page closed normally (it may come back from the back/forward cache): no breadcrumb.
    static CLOSED: Cell<bool> = const { Cell::new(false) };
    /// Lets go of this page's lock.
    static RELEASE: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
    /// Unclean exits found at start, for the statistics.
    static FOUND: RefCell<Vec<(String, Value)>> = const { RefCell::new(Vec::new()) };
    /// Breadcrumbs taken into a statistics batch, removed once it is sent ([`reported`]).
    static TAKEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// A breadcrumb looked alive at start: a page reopened right after a crash finds its
    /// predecessor's heartbeat only seconds old, so it is checked again once that is long enough.
    static RECHECK: Cell<bool> = const { Cell::new(false) };
    /// The stage the breadcrumb says.
    static STAGE_WRITTEN: Cell<&'static str> = const { Cell::new("") };
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

/// `performance.memory`'s used heap (Chromium only), in MB, and `navigator.deviceMemory`, in GB.
fn browser_memory() -> (Option<u64>, Option<f64>) {
    let Some(window) = web_sys::window() else { return (None, None) };
    let get = |target: &JsValue, key: &str| Reflect::get(target, &JsValue::from_str(key)).ok().filter(|v| !v.is_undefined());
    let heap = get(&window, "performance").and_then(|p| get(&p, "memory")).and_then(|m| get(&m, "usedJSHeapSize")).and_then(|v| v.as_f64());
    let device = get(&window.navigator(), "deviceMemory").and_then(|v| v.as_f64());
    (heap.filter(|b| b.is_finite() && *b >= 0.0).map(|b| b as u64 >> 20), device)
}

/// Changes this page's breadcrumb now. Nothing on a worker, which has no storage or breadcrumb, and
/// never a borrow that could be held by the code that just panicked.
fn patch(change: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    // After a normal close too (a stage can still change on the way out, and `visibilitychange`
    // comes after `pagehide`): a breadcrumb written then would read as a crash at the next load.
    if CLOSED.with(Cell::get) {
        return;
    }
    let Some(st) = storage() else { return };
    let Some(key) = KEY.with(|k| k.try_borrow().ok().and_then(|k| k.clone())) else { return };
    let mut crumb = st.get_item(&key).ok().flatten().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({}));
    let Some(fields) = crumb.as_object_mut() else { return };
    fields.entry("t").or_insert_with(|| json!(js_sys::Date::now()));
    change(fields);
    let _ = st.set_item(&key, &crumb.to_string());
}

/// A stage started (`telemetry::set_stage`). A stage flickering on and off every frame writes at
/// most once per heartbeat: leaving one writes nothing, and only [`tick`] writes `idle`.
fn stage_started(stage: &'static str) {
    if STAGE_WRITTEN.with(|w| w.replace(stage)) != stage {
        patch(|fields| {
            fields.insert("stage".into(), json!(stage));
        });
    }
}

fn panic_fields(fields: &mut serde_json::Map<String, Value>) {
    if let Some((location, code)) = PANIC.get() {
        fields.insert("panic".into(), json!(true));
        fields.insert("loc".into(), json!(location));
        fields.insert("msg".into(), json!(code));
        fields.insert("sent".into(), json!(PANIC_SENT.load(Ordering::Acquire)));
    }
}

/// From the panic hook, on any thread: writes the panic to the breadcrumb and sends it, before the
/// page's WebAssembly dies. Only this page's first panic. A panic in here would abort without the
/// rest of the hook, so nothing here unwraps or blocks.
pub fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    let (location, code) = telemetry::panic_report(info);
    if PANIC.set((location.clone(), code)).is_err() {
        return;
    }
    patch(panic_fields);
    let event = telemetry::crash_event("panic", Some(&location), Some(code), None, Some(telemetry::stage_name()), Some(memory_bytes() >> 20));
    if crate::usage_stats::send_crash(&event) {
        PANIC_SENT.store(true, Ordering::Release);
        patch(panic_fields);
    }
}

/// One more unclean exit in a row: how many now.
fn bump_loop(st: &web_sys::Storage) -> u64 {
    let n = st.get_item(LOOP_KEY).ok().flatten().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0).saturating_add(1);
    let _ = st.set_item(LOOP_KEY, &n.to_string());
    n
}

/// Takes this page's lock, held until it lets go or is gone.
fn hold_lock() {
    let Some(key) = KEY.with(|k| k.borrow().clone()) else { return };
    let Some(locks) = web_locks() else { return };
    let Ok(request) = Reflect::get(&locks, &JsValue::from_str("request")).and_then(|f| f.dyn_into::<js_sys::Function>()) else { return };
    let held = Closure::once_into_js(|| {
        js_sys::Promise::new(&mut |resolve, _| {
            RELEASE.with(|r| *r.borrow_mut() = Some(resolve));
        })
    });
    if let Ok(done) = request.call2(&locks, &JsValue::from_str(&key), &held).and_then(|p| p.dyn_into::<js_sys::Promise>()) {
        // Awaited for a refusal not to become an unhandled rejection, which would read as a crash.
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(done).await;
        });
    }
}

fn release_lock() {
    if let Some(release) = RELEASE.with(|r| r.borrow_mut().take()) {
        let _ = release.call0(&JsValue::NULL);
    }
}

fn web_locks() -> Option<JsValue> {
    let locks = Reflect::get(&web_sys::window()?.navigator(), &JsValue::from_str("locks")).ok()?;
    (!locks.is_undefined()).then_some(locks)
}

/// The names of the Web Locks held on this origin, right now. Empty when not known.
async fn held_locks() -> Vec<String> {
    let Some(locks) = web_locks() else { return Vec::new() };
    let Some(query) = Reflect::get(&locks, &JsValue::from_str("query")).ok().and_then(|f| f.dyn_into::<js_sys::Function>().ok()) else { return Vec::new() };
    let Some(promise) = query.call0(&locks).ok().and_then(|p| p.dyn_into::<js_sys::Promise>().ok()) else { return Vec::new() };
    let Ok(state) = wasm_bindgen_futures::JsFuture::from(promise).await else { return Vec::new() };
    let Some(held) = Reflect::get(&state, &JsValue::from_str("held")).ok().and_then(|h| h.dyn_into::<js_sys::Array>().ok()) else { return Vec::new() };
    held.iter().filter_map(|l| Reflect::get(&l, &JsValue::from_str("name")).ok()?.as_string()).collect()
}

/// Moves the breadcrumbs of dead pages (no heartbeat for [`DEAD_MS`], no lock) not seen yet into
/// [`FOUND`]. Returns whether another page's breadcrumb was too young to tell.
fn collect(now: f64, held: &[String]) -> bool {
    let Some(st) = storage() else { return false };
    let own = KEY.with(|k| k.borrow().clone());
    let len = st.length().unwrap_or(0);
    let keys: Vec<String> = (0..len).filter_map(|i| st.key(i).ok().flatten()).filter(|k| k.starts_with(PREFIX)).collect();
    let mut young = false;
    for k in keys {
        if own.as_deref() == Some(k.as_str()) || held.contains(&k) || FOUND.with(|f| f.borrow().iter().any(|(f, _)| *f == k)) || TAKEN.with(|t| t.borrow().contains(&k)) {
            continue;
        }
        let v: Option<Value> = st.get_item(&k).ok().flatten().and_then(|t| serde_json::from_str(&t).ok());
        let beat = v.as_ref().and_then(|v| v.get("t")).and_then(Value::as_f64).unwrap_or(0.0);
        if now - beat < DEAD_MS {
            young = true; // a tab just starting (its lock on the way), or a page that died seconds ago
            continue;
        }
        // Kept until the statistics have sent it ([`reported`]): this page may die before then.
        match v {
            Some(mut v) => {
                log::warn!("photocraft-web: the previous page didn't close cleanly: {v}");
                let background = !counts_as_crash(&v);
                if v.is_object() && !background {
                    v["loop"] = json!(bump_loop(&st));
                }
                FOUND.with(|f| f.borrow_mut().push((k, v)));
            }
            None => {
                let _ = st.remove_item(&k);
            }
        }
    }
    young
}

/// A page reclaimed or closed from the background is no crash, unless it had panicked.
fn counts_as_crash(crumb: &Value) -> bool {
    crumb.get("panic").and_then(Value::as_bool) == Some(true) || !crumb.get("stage").and_then(Value::as_str).is_some_and(Stage::is_background)
}

/// Looks for dead pages, and reports them if the statistics are already up (else [`take_events`]
/// has them when they start).
fn check_dead_pages(check_again: bool) {
    wasm_bindgen_futures::spawn_local(async move {
        let held = held_locks().await;
        let young = collect(js_sys::Date::now(), &held);
        if check_again {
            RECHECK.with(|r| r.set(young));
        }
        if telemetry::is_active() {
            let events = take_events();
            if !events.is_empty() {
                telemetry::with(|r| {
                    for e in events {
                        r.event(e);
                    }
                });
                crate::usage_stats::flush_now();
                reported();
            }
        }
    });
}

/// At start: starts this page's breadcrumb and collects those of dead pages.
pub fn start() {
    let now = js_sys::Date::now();
    STARTED.with(|s| s.set(now));
    let key = format!("{PREFIX}{:x}{:x}", now as u64, (js_sys::Math::random() * 1e9) as u64);
    KEY.with(|k| *k.borrow_mut() = Some(key));
    hold_lock();
    check_dead_pages(true);
    // From now on: a page that dies while starting leaves a breadcrumb too.
    telemetry::set_stage_hook(stage_started);
    if let Some(document) = web_sys::window().and_then(|w| w.document()) {
        telemetry::set_page_hidden(document.hidden());
    }
    stage_started(telemetry::stage_name());
    // A normal close (or entering the back/forward cache) is clean; coming back starts again.
    if let Some(window) = web_sys::window() {
        let gone = Closure::<dyn FnMut()>::new(|| {
            CLOSED.with(|c| c.set(true));
            if let (Some(st), Some(k)) = (storage(), KEY.with(|k| k.borrow().clone())) {
                let _ = st.remove_item(&k);
                let _ = st.remove_item(LOOP_KEY);
            }
            LAST.with(|l| l.set(0.0));
            release_lock();
        });
        let _ = window.add_event_listener_with_callback("pagehide", gone.as_ref().unchecked_ref());
        gone.forget();
        let back = Closure::<dyn FnMut()>::new(|| {
            if CLOSED.with(|c| c.replace(false)) {
                hold_lock();
            }
        });
        let _ = window.add_event_listener_with_callback("pageshow", back.as_ref().unchecked_ref());
        back.forget();
        if let Some(document) = window.document() {
            let page = document.clone();
            let visibility = Closure::<dyn FnMut()>::new(move || telemetry::set_page_hidden(page.hidden()));
            let _ = document.add_event_listener_with_callback("visibilitychange", visibility.as_ref().unchecked_ref());
            visibility.forget();
        }
    }
}

/// Every frame: the breadcrumb, every couple of seconds.
pub fn tick(app: &PhotocraftApp) {
    if CLOSED.with(Cell::get) {
        return;
    }
    let mem = memory_bytes();
    HIGH.with(|h| h.set(h.get().max(mem)));
    let now = js_sys::Date::now();
    if now - LAST.with(Cell::get) < BEAT_MS {
        return;
    }
    LAST.with(|l| l.set(now));
    crash_test();
    if RECHECK.with(Cell::get) && now - STARTED.with(Cell::get) > DEAD_MS + BEAT_MS {
        RECHECK.with(|r| r.set(false));
        check_dead_pages(false);
    }
    // A minute in, statistics that would report the dead pages have (they start on the first
    // frames); otherwise they are off, and the breadcrumbs go.
    if now - STARTED.with(Cell::get) > 60_000.0 && FOUND.with(|f| !f.borrow().is_empty()) {
        forget_found();
    }
    let (Some(st), Some(k)) = (storage(), KEY.with(|k| k.borrow().clone())) else { return };
    let stage = telemetry::stage_name();
    STAGE_WRITTEN.with(|w| w.set(stage));
    let (js_mb, dev_gb) = browser_memory();
    let mut crumb = json!({
        "t": now,
        "cmd": app.perf.last_command,
        "tool": format!("{:?}", app.ui.tool),
        "memMB": HIGH.with(Cell::get) >> 20,
        "upMin": ((now - STARTED.with(Cell::get)) / 60_000.0).round(),
        "docs": app.session.documents().len(),
        "stage": stage,
        "js_mb": js_mb,
        "dev_gb": dev_gb,
    });
    if let Some(fields) = crumb.as_object_mut() {
        panic_fields(fields);
    }
    let _ = st.set_item(&k, &crumb.to_string());
}

/// `self.__capishotCrashTest = 'panic' | 'worker'` makes a real panic (packaging/web-threads/e2e/
/// crash-report.mjs): the only way to see the report that a real panic sends. A page's own script
/// can only crash that page; nothing sets this but the test.
#[allow(clippy::panic)]
fn crash_test() {
    let Some(window) = web_sys::window() else { return };
    let Some(what) = Reflect::get(&window, &JsValue::from_str("__capishotCrashTest")).ok().and_then(|v| v.as_string()) else { return };
    let _ = Reflect::delete_property(&window, &JsValue::from_str("__capishotCrashTest"));
    match what.as_str() {
        "panic" => panic!("crash test"),
        #[cfg(target_feature = "atomics")]
        "worker" => rayon::spawn(|| panic!("crash test")),
        _ => {}
    }
}

/// The unclean exits found at start, as statistics events (taken once). Their breadcrumbs stay
/// until [`reported`].
pub fn take_events() -> Vec<Value> {
    FOUND
        .with(|f| std::mem::take(&mut *f.borrow_mut()))
        .into_iter()
        .flat_map(|(k, v)| {
            TAKEN.with(|t| t.borrow_mut().push(k));
            let text = |key: &str| v.get(key).and_then(Value::as_str);
            let number = |key: &str| v.get(key).and_then(Value::as_f64).map(|n| n.max(0.0) as u64);
            let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
            let mut exit = telemetry::unclean_exit_event(text("cmd"), text("tool"), number("memMB"), number("upMin"), number("docs"), Some(flag("panic")));
            telemetry::unclean_exit_detail(&mut exit, text("stage"), number("js_mb"), v.get("dev_gb").and_then(Value::as_f64), number("loop"));
            let mut events = vec![exit];
            // It panicked before the statistics were up (or the beacon failed): sent now.
            if flag("panic") && !flag("sent") {
                events.push(telemetry::crash_event("panic", text("loc"), text("msg"), None, text("stage"), number("memMB")));
            }
            events
        })
        .collect()
}

/// The batch with the events of [`take_events`] was handed to the browser: their breadcrumbs go.
pub fn reported() {
    let taken = TAKEN.with(|t| std::mem::take(&mut *t.borrow_mut()));
    if let Some(st) = storage() {
        for k in taken {
            let _ = st.remove_item(&k);
        }
    }
}

/// Statistics are off: the breadcrumbs of dead pages found at start are only logged, then go.
pub fn forget_found() {
    let found = FOUND.with(|f| std::mem::take(&mut *f.borrow_mut()));
    if let Some(st) = storage() {
        for (k, _) in found {
            let _ = st.remove_item(&k);
        }
    }
}
