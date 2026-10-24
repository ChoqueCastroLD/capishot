//! Anonymous usage statistics for the hosted web app: the browser side of
//! [`photocraft_ui_egui::telemetry`] (what is recorded is documented there).
//!
//! Only on Capishot's own hosts, and only while Preferences › General › Send anonymous usage
//! statistics is on and the browser sends neither Do Not Track nor Global Privacy Control.
//! Batches go to the page's own origin (`/capishot/api/t`, so the cross-origin isolated page needs
//! nothing third-party) every 30 s and when the page hides, with `navigator.sendBeacon` (falling
//! back to a `keepalive` fetch). A failed send is dropped, never retried. Errors are silent.

use std::cell::RefCell;

use js_sys::Reflect;
use photocraft_doc::Document;
use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::telemetry::{self, Environment, Recorder};
use serde_json::json;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

/// Same origin on every host below.
const ENDPOINT: &str = "/capishot/api/t";
/// Capishot's own deployments; anywhere else (a dev server, a fork) records nothing.
const HOSTS: &[&str] = &["capichan.com"];
/// The random install id (localStorage).
const INSTALL_KEY: &str = "capishot.telemetry.iid";

struct State {
    env: Environment,
    /// The preference as last applied.
    enabled: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Called once at startup with the canvas backend (`webgpu`, `webgl`, `cpu`) and GPU vendor
/// class. Nothing is recorded until [`sync`] sees the preference on.
pub fn init(gpu: &'static str, gpu_vendor: &'static str) {
    let Some(window) = web_sys::window() else { return };
    let host = window.location().hostname().unwrap_or_default();
    if !HOSTS.contains(&host.as_str()) || privacy_signal(&window) {
        return;
    }
    telemetry::set_clock(js_sys::Date::now);
    let env = environment(&window, gpu, gpu_vendor);
    STATE.with(|s| *s.borrow_mut() = Some(State { env, enabled: false }));
    install_listeners(&window);
}

/// Every frame: follows the preference, notes tool switches and that the user interacted.
pub fn sync(app: &PhotocraftApp, ctx: &egui::Context) {
    let want = app.session.prefs().general.send_anonymous_usage_statistics;
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        (st.enabled != want).then(|| {
            st.enabled = want;
            st.env.clone()
        })
    });
    match changed {
        Some(env) if want => start_session(env),
        Some(_) => telemetry::stop(),
        None => {}
    }
    if !telemetry::is_active() {
        return;
    }
    telemetry::observe_tool(app.ui.tool);
    if ctx.input(|i| i.events.iter().any(is_interaction)) {
        telemetry::with(|r| r.interaction(js_sys::Date::now()));
    }
}

fn is_interaction(e: &egui::Event) -> bool {
    matches!(
        e,
        egui::Event::PointerButton { .. }
            | egui::Event::PointerMoved(_)
            | egui::Event::MouseWheel { .. }
            | egui::Event::Key { .. }
            | egui::Event::Text(_)
            | egui::Event::Touch { .. }
            | egui::Event::Zoom(_)
    )
}

fn start_session(mut env: Environment) {
    env.language = photocraft_ui_egui::i18n::current().code().to_string();
    let mut recorder = Recorder::new(&install_id(), &random_id(), js_sys::Date::now());
    recorder.event(telemetry::session_start_event(&env));
    telemetry::start(recorder);
}

/// A document was opened (`file` is only used for its extension).
pub fn doc_open(file: &str, bytes: usize, result: Result<&Document, &String>, ms: f64) {
    if !telemetry::is_active() {
        return;
    }
    let mut e = json!({"e": "doc_open", "kind": telemetry::file_kind(file), "bytes": telemetry::bytes_bucket(bytes), "ms": round(ms), "ok": result.is_ok()});
    if let Ok(doc) = result {
        e["mp"] = json!(telemetry::mp_bucket(doc.size.width, doc.size.height));
        e["layers"] = json!(telemetry::layers_bucket(doc.layer_count()));
    }
    telemetry::with(|r| {
        r.event(e);
        if let Err(message) = result {
            r.error("file.open", message);
        }
    });
}

/// A document was encoded for saving (layered formats) or exporting (flat images).
pub fn doc_saved(file: &str, doc: &Document, ok: Result<(), &String>, ms: f64) {
    doc_saved_stats(file, (doc.size.width, doc.size.height), doc.layer_count(), ok, ms);
}

/// [`doc_saved`] from the document's size and layer count (a save encoded on the worker pool).
pub fn doc_saved_stats(file: &str, (width, height): (u32, u32), layers: usize, ok: Result<(), &String>, ms: f64) {
    if !telemetry::is_active() {
        return;
    }
    let kind = telemetry::file_kind(file);
    let (event, id) = if telemetry::is_document_kind(kind) { ("doc_save", "file.save") } else { ("export", "file.export") };
    let e = json!({
        "e": event, "kind": kind, "mp": telemetry::mp_bucket(width, height),
        "layers": telemetry::layers_bucket(layers), "ms": round(ms), "ok": ok.is_ok(),
    });
    telemetry::with(|r| {
        r.event(e);
        if let Err(message) = ok {
            r.error(id, message);
        }
    });
}

fn round(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 { ms.min(3_600_000.0).round() as u64 } else { 0 }
}

/// Send what is waiting. Fire and forget: a failure drops the batch.
fn flush() {
    let Some(body) = telemetry::take_batch() else { return };
    let Some(window) = web_sys::window() else { return };
    if window.navigator().send_beacon_with_opt_str(ENDPOINT, Some(&body)).unwrap_or(false) {
        return;
    }
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));
    let _ = Reflect::set(&init, &JsValue::from_str("keepalive"), &JsValue::TRUE);
    let request = window.fetch_with_str_and_init(ENDPOINT, &init);
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(request).await;
    });
}

fn visible() -> bool {
    web_sys::window().and_then(|w| w.document()).is_some_and(|d| !d.hidden())
}

/// The 30 s timer (heartbeat check, then flush), and flushes when the page hides or goes away.
fn install_listeners(window: &web_sys::Window) {
    let timer = Closure::<dyn FnMut()>::new(|| {
        telemetry::with(|r| r.tick(js_sys::Date::now(), visible()));
        flush();
    });
    if window.set_interval_with_callback_and_timeout_and_arguments_0(timer.as_ref().unchecked_ref(), telemetry::FLUSH_MS as i32).is_ok() {
        timer.forget();
    }
    if let Some(document) = window.document() {
        let hidden = Closure::<dyn FnMut()>::new(|| {
            if !visible() {
                flush();
            }
        });
        if document.add_event_listener_with_callback("visibilitychange", hidden.as_ref().unchecked_ref()).is_ok() {
            hidden.forget();
        }
    }
    let gone = Closure::<dyn FnMut()>::new(|| {
        telemetry::with(|r| r.end(js_sys::Date::now()));
        flush();
    });
    if window.add_event_listener_with_callback("pagehide", gone.as_ref().unchecked_ref()).is_ok() {
        gone.forget();
    }
    // Back from the back/forward cache: the ended session is over, a new one starts.
    let back = Closure::<dyn FnMut(web_sys::Event)>::new(|e: web_sys::Event| {
        let restored = Reflect::get(&e, &JsValue::from_str("persisted")).ok().and_then(|v| v.as_bool()) == Some(true);
        if !restored {
            return;
        }
        let env = STATE.with(|s| s.borrow().as_ref().filter(|st| st.enabled).map(|st| st.env.clone()));
        if let Some(env) = env {
            start_session(env);
        }
    });
    if window.add_event_listener_with_callback("pageshow", back.as_ref().unchecked_ref()).is_ok() {
        back.forget();
    }
}

/// Do Not Track or Global Privacy Control turns statistics off.
fn privacy_signal(window: &web_sys::Window) -> bool {
    let navigator = window.navigator();
    let get = |target: &JsValue, key: &str| Reflect::get(target, &JsValue::from_str(key)).ok();
    let dnt = |v: Option<JsValue>| v.and_then(|v| v.as_string()).is_some_and(|s| s == "1" || s == "yes");
    dnt(get(&navigator, "doNotTrack")) || dnt(get(window, "doNotTrack")) || get(&navigator, "globalPrivacyControl").and_then(|v| v.as_bool()) == Some(true)
}

fn environment(window: &web_sys::Window, gpu: &'static str, gpu_vendor: &'static str) -> Environment {
    let get = |target: &JsValue, key: &str| Reflect::get(target, &JsValue::from_str(key)).ok();
    let navigator = window.navigator();
    let number = |v: Option<JsValue>| v.and_then(|v| v.as_f64()).filter(|n| n.is_finite() && *n >= 0.0).map_or(0, |n| n.min(100_000.0) as u32);
    let screen = get(window, "screen");
    let (w, h) = screen.map_or((0, 0), |s| (number(get(&s, "width")), number(get(&s, "height"))));
    let user_agent = navigator.user_agent().unwrap_or_default();
    let touch_points = number(get(&navigator, "maxTouchPoints"));
    let memory_gb = get(&navigator, "deviceMemory").and_then(|v| v.as_f64()).filter(|n| n.is_finite() && *n > 0.0).unwrap_or(0.0);
    Environment {
        device: telemetry::device_class(&user_agent, touch_points, w.min(h)),
        memory_gb,
        threads: cfg!(target_feature = "atomics"),
        cross_origin_isolated: get(window, "crossOriginIsolated").and_then(|v| v.as_bool()) == Some(true),
        cores: number(get(&navigator, "hardwareConcurrency")),
        gpu,
        gpu_vendor,
        user_agent,
        screen: (w, h),
        language: String::new(),
    }
}

/// 128 random bits as hex.
fn random_id() -> String {
    (0..4).map(|_| format!("{:08x}", (js_sys::Math::random() * 4_294_967_296.0) as u32)).collect()
}

/// The install id kept in localStorage (a new one per session when storage is unavailable).
fn install_id() -> String {
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    if let Some(id) = storage.as_ref().and_then(|s| s.get_item(INSTALL_KEY).ok().flatten())
        && id.len() == 32
        && id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return id;
    }
    let id = random_id();
    if let Some(s) = storage {
        let _ = s.set_item(INSTALL_KEY, &id);
    }
    id
}
