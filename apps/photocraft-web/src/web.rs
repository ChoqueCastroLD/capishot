//! The browser shell: web `Services`, drag-and-drop, and the eframe web runner.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_engine::Session;
use photocraft_ui_egui::i18n;
use photocraft_ui_egui::served_fonts;
use photocraft_ui_egui::theme::ThemeKind;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogRequest, PhotocraftApp, Services};
use wasm_bindgen::JsCast as _;

type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

const SVG_EXTS: &[&str] = &["svg", "svgz"];
const CANVAS_ID: &str = "photocraft_canvas";

/// Fonts the host serves next to the page (`photocraft_ui_egui::served_fonts`): craft-fonts' manifest
/// format, each file relative to the site root. No manifest (a 404) means no served fonts.
const FONTS_MANIFEST: &str = "fonts/manifest.txt";

/// The served fonts: the manifest once read, and what the network has delivered.
#[derive(Default)]
struct ServedFonts {
    /// Font files by family, each with its Subresource Integrity value when the manifest has one.
    files: HashMap<String, Vec<(String, Option<String>)>>,
    /// Families fetched or being fetched: each one once per page load.
    fetched: HashSet<String>,
    /// Downloaded files waiting for the next frame: (family, bytes).
    arrived: Vec<(String, Vec<u8>)>,
}

type Served = Arc<Mutex<ServedFonts>>;

/// Threads build: linking wasm-bindgen-rayon exports `initThreadPool(n)` (and the worker entry
/// points it needs) to the page, which starts it together with [`start_threads`] and calls
/// [`thread_pool_ready`] when it resolves.
#[cfg(target_feature = "atomics")]
use wasm_bindgen_rayon as _;

/// Threads build: the page calls this (as `start`) while the worker pool starts: the graphics
/// device, fonts and presets get ready meanwhile, and the app is built once the pool is up.
#[cfg(target_feature = "atomics")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = start)]
pub fn start_threads() {
    start();
}

/// Parallelism diagnostic, for checking that a browser really runs Capishot's parallel code on
/// the worker pool: a busy loop spread over rayon reports how many workers took part and how long it
/// took (pure parallel work, so it shows the pool's scaling), then a Gaussian blur (radius 20) runs
/// through the engine on a noisy `width`×`height` document and an image that size is encoded as
/// PNG (deflated in parallel bands). The threads page exposes the module as `photocraftModule`, so
/// from the console: `photocraftModule.parallelSelfTest(4000, 3000)`. Compare with the page opened as
/// `?threads=1`, or with the single-threaded build (Trunk's page exposes it as
/// `wasmBindings.parallelSelfTest`), where the same steps run on the page's one thread.
#[wasm_bindgen::prelude::wasm_bindgen(js_name = parallelSelfTest)]
pub fn parallel_self_test(width: u32, height: u32) -> Result<String, String> {
    use serde_json::json;
    let spin = |i: u64| {
        let mut x = i;
        for _ in 0..500_000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        }
        std::hint::black_box(x);
    };
    let t0 = js_sys::Date::now();
    #[cfg(target_feature = "atomics")]
    let (pool_threads, workers_used) = {
        use rayon::prelude::*;
        let workers: std::collections::BTreeSet<usize> = (0..1024u64)
            .into_par_iter()
            .filter_map(|i| {
                spin(i);
                rayon::current_thread_index()
            })
            .collect();
        (rayon::current_num_threads(), workers.len())
    };
    #[cfg(not(target_feature = "atomics"))]
    let (pool_threads, workers_used) = {
        (0..1024u64).for_each(spin);
        (1usize, 0usize)
    };
    let loop_ms = js_sys::Date::now() - t0;
    let mut s = Session::new();
    let err = |e: photocraft_engine::EngineError| e.to_string();
    s.execute("file.new", json!({"width": width.clamp(1, 16_000), "height": height.clamp(1, 16_000), "background": "white"})).map_err(err)?;
    s.execute("filter.noise.addNoise", json!({"amount": 20, "seed": 3, "distribution": "gaussian"})).map_err(err)?;
    let t0 = js_sys::Date::now();
    s.execute("filter.blur.gaussianBlur", json!({"radius": 20})).map_err(err)?;
    let blur_ms = js_sys::Date::now() - t0;
    // PNG export of a noisy RGBA image the same size (deflated in parallel bands).
    let (w, h) = (width.clamp(1, 16_000), height.clamp(1, 16_000));
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let px: Vec<u8> = (0..w as usize * h as usize * 4)
        .map(|i| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            if i % 4 == 3 { 255 } else { 96 + (seed >> 59) as u8 }
        })
        .collect();
    let img = Image::from_u8(w, h, ChannelLayout::Rgba, px).map_err(|e| e.to_string())?;
    let t0 = js_sys::Date::now();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())?;
    let png_ms = js_sys::Date::now() - t0;
    Ok(json!({
        "poolThreads": pool_threads, "workersUsed": workers_used, "loopMs": loop_ms.round(),
        "blurMs": blur_ms.round(), "pngMs": png_ms.round(), "pngBytes": png.len(),
    })
    .to_string())
}

pub fn start() {
    crate::startup::mark("start");
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    // The app's own timings (Window › Info's performance readout, previews that wait for a value to
    // settle, double key presses) need a clock; `std::time` has none on the web.
    photocraft_ui_egui::gpu_canvas::set_clock(crate::frame_perf::now_ms);
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let q = query();
        // The Photopea-compatible config in the URL hash (crate::embed).
        let photopea = crate::embed::photopea_config();
        let embed_query = q.clone();
        let force_cpu = q.contains("cpu");
        let perf_debug = q.split(['?', '&']).any(|kv| kv == "perf=1" || kv == "perf");
        if q.split(['?', '&']).any(|kv| kv == "gpuopt=0") {
            // Measurements: the GPU canvas as before pass fusion and level-of-detail refreshes.
            photocraft_ui_egui::gpu_canvas::set_reduced_refreshes(false);
        }
        if q.split(['?', '&']).any(|kv| kv == "movecopy=1") {
            photocraft_ui_egui::move_ui::set_move_in_place(false);
        }
        let mut options = eframe::WebOptions::default();
        photocraft_ui_egui::gpu_canvas::use_adapter_limits(&mut options.wgpu_options.wgpu_setup);
        if q.contains("webgl")
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        // Hydrate before constructing an interactive session: a late load must never undo a
        // user deletion/reorder. Storage denial or timeout still opens a usable session.
        let (database, presets, preset_warnings) = match crate::indexed_presets::load().await {
            Ok((db, bridge, warnings)) => (Some(db), bridge, warnings),
            Err(e) => {
                let bridge = crate::preset_bridge::Bridge::default();
                bridge.unavailable(e);
                (None, bridge, Vec::new())
            }
        };
        crate::startup::mark("presets loaded");
        // Before eframe installs its listeners: real Ctrl + wheel, and no browser page zoom.
        crate::wheel_guard::install();
        cap_pixel_density();
        cap_memory();
        let pen_target = canvas.clone();
        let perf = crate::frame_perf::FramePerf::new(&canvas, perf_debug);
        let boot = Boot {
            presets,
            preset_warnings,
            database,
            embed_query,
            photopea,
            pen_target,
            force_cpu,
            perf,
            loading: document.get_element_by_id("photocraft_loading"),
        };
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    crate::startup::mark("graphics ready");
                    PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::Pro);
                    crate::startup::mark("fonts and theme");
                    // The threads build may get here before its worker pool is up (the page starts
                    // both together): the app, the first thing to use rayon, is built once it is.
                    Ok(Box::new(WebRoot::Booting(Some(Box::new((boot, cc.wgpu_render_state.clone()))))))
                }),
            )
            .await;
        if let Err(e) = result
            && let Some(el) = document.get_element_by_id("photocraft_loading")
        {
            // No WebGPU adapter and no WebGL2 context (no GPU and the browser refuses software
            // WebGL, or hardware acceleration is off): nothing can draw the interface.
            el.set_inner_html(&format!(
                "<p>Capishot failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required. If this computer has no graphics \
                 acceleration, turn on hardware acceleration in the browser settings (Chrome: Settings › System › Use graphics \
                 acceleration) or try another browser.</p>"
            ));
        }
    });
}

/// Is rayon's worker pool up? The single-threaded build has none to wait for; the threads page
/// calls [`thread_pool_ready`] when `initThreadPool` resolves.
static THREAD_POOL_READY: AtomicBool = AtomicBool::new(cfg!(not(target_feature = "atomics")));

/// Threads build: the page calls this (as `threadPoolReady`) once `initThreadPool` has resolved.
/// Until then nothing may touch rayon (it would build its one-thread fallback pool for good), so
/// [`WebRoot`] holds the app back while the graphics device, fonts and presets get ready.
#[cfg(target_feature = "atomics")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = threadPoolReady)]
pub fn thread_pool_ready() {
    THREAD_POOL_READY.store(true, Ordering::Release);
    crate::startup::mark("worker pool ready");
}

/// What building the app needs, gathered before the first frame.
struct Boot {
    presets: crate::preset_bridge::Bridge,
    preset_warnings: Vec<String>,
    database: Option<web_sys::IdbDatabase>,
    embed_query: String,
    photopea: Option<crate::embed_protocol::PhotopeaConfig>,
    pen_target: web_sys::HtmlCanvasElement,
    force_cpu: bool,
    /// `?perf=1` frame timings (`frame_perf.rs`), watching the canvas from before the first frame.
    perf: crate::frame_perf::FramePerf,
    /// The page's "Loading Capishot…", removed once the app is built.
    loading: Option<web_sys::Element>,
}

impl Boot {
    fn build(self, ctx: &egui::Context, wgpu: Option<eframe::egui_wgpu::RenderState>) -> WebShell {
        let Boot { presets, mut preset_warnings, database, embed_query, photopea, pen_target, force_cpu, perf, loading } = self;
        let inbox: Inbox = Arc::default();
        let places: Inbox = Arc::default();
        let served: Served = Arc::default();
        load_font_manifest(served.clone(), ctx.clone());
        let user_fonts = UserFontStore::default();
        open_user_fonts(user_fonts.clone(), ctx.clone());
        let (session, warnings) = crate::preset_bridge::session(presets.clone());
        preset_warnings.extend(warnings);
        crate::startup::mark("session");
        let control = crate::embed::install(&embed_query, ctx.clone());
        let mut app = PhotocraftApp::new(session, services(inbox.clone(), photopea.clone())).with_control(control);
        // Image › Upscale with AI…: the AI worker module, loaded on first use (ai_bridge.rs).
        // Window › AI Models… (`ai.models.*`): downloads through a second worker instance.
        if let Some(ai) = crate::ai_bridge::from_page() {
            app.session.set_ai_upscaler(Some(ai.upscaler));
            app.session.set_inpainter(Some(ai.inpainter));
            app.session.set_ai_models(Some(ai.models));
        }
        crate::startup::mark("app");
        if let Some(config) = &photopea {
            crate::embed::open_config_files(config);
        }
        if !preset_warnings.is_empty() {
            photocraft_ui_egui::notices::post(&mut app, i18n::t("Some brush presets could not be loaded"), preset_warnings, true, None);
        }
        listen_pen(&pen_target, app.stylus.feed.clone());
        reject_palms();
        crate::clipboard::install(ctx);
        listen_drag_position();
        app.set_theme(ctx, ThemeKind::Pro);
        let mut gpu = ("cpu", "unknown");
        if let Some(rs) = wgpu
            && !force_cpu
        {
            let info = rs.adapter.get_info();
            log::info!("photocraft-web: wgpu backend {:?}", info.backend);
            gpu = (
                match info.backend {
                    eframe::wgpu::Backend::BrowserWebGpu => "webgpu",
                    eframe::wgpu::Backend::Gl => "webgl",
                    _ => "unknown",
                },
                photocraft_ui_egui::telemetry::gpu_vendor_class(info.vendor, &info.name),
            );
            app.set_wgpu(rs);
        }
        crate::startup::mark("canvas");
        crate::usage_stats::init(gpu.0, gpu.1);
        let unsaved = Arc::new(AtomicBool::new(false));
        guard_unload(unsaved.clone());
        if let Some(el) = loading {
            el.remove();
        }
        WebShell { app, inbox, places, canvas: pen_target, unsaved, served, user_fonts, database, presets, startup_frames: 0, perf }
    }
}

/// The eframe app: [`WebShell`] once built, and before that (the threads build waiting for its
/// worker pool) an empty frame in the page's background colour, under "Loading Capishot…".
enum WebRoot {
    Booting(Option<Box<(Boot, Option<eframe::egui_wgpu::RenderState>)>>),
    Running(Box<WebShell>),
}

impl WebRoot {
    /// Builds the app as soon as rayon's worker pool is up (at once in the single-threaded build).
    fn running(&mut self, ctx: &egui::Context) -> Option<&mut WebShell> {
        if let WebRoot::Booting(boot) = self {
            if !THREAD_POOL_READY.load(Ordering::Acquire) {
                // No event wakes eframe when the pool comes up: look again next frame.
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return None;
            }
            let (boot, wgpu) = *boot.take()?;
            *self = WebRoot::Running(Box::new(boot.build(ctx, wgpu)));
        }
        match self {
            WebRoot::Running(shell) => Some(shell),
            WebRoot::Booting(_) => None,
        }
    }
}

impl eframe::App for WebRoot {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let WebRoot::Running(shell) = self {
            shell.raw_input_hook(ctx, raw_input);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(shell) = self.running(ctx) {
            shell.logic(ctx, frame);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        match self {
            WebRoot::Running(shell) => shell.ui(ui, frame),
            WebRoot::Booting(_) => {
                egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::from_gray(0x26))).show(ui, |_| {});
            }
        }
    }
}

/// Pen pressure, tilt, twist and the eraser button from Pointer Events (eframe forwards none of them for pens) into
/// the app's stylus feed. The sample is kept through `pointerup` so the stroke's last points keep
/// their pressure; hovering, a mouse, or leaving the canvas clears it.
fn listen_pen(target: &web_sys::HtmlCanvasElement, feed: photocraft_ui_egui::stylus::StylusFeed) {
    use photocraft_ui_egui::stylus::PenSample;
    use wasm_bindgen::closure::Closure;
    for kind in ["pointerdown", "pointermove", "pointerup", "pointercancel", "pointerleave"] {
        let feed = feed.clone();
        let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
            let ty = e.type_();
            if ty == "pointerup" && e.pointer_type() == "pen" {
                return;
            }
            let pen = e.pointer_type() == "pen" && e.buttons() != 0 && ty != "pointercancel" && ty != "pointerleave";
            // W3C Pointer Events: `buttons` bit 5 (32) is the pen's eraser.
            let eraser = e.buttons() & 32 != 0;
            feed.set(pen.then(|| PenSample {
                pressure: e.pressure(),
                tilt_x: e.tilt_x() as f32,
                tilt_y: e.tilt_y() as f32,
                rotation: e.twist() as f32,
                eraser,
            }));
        });
        if target.add_event_listener_with_callback(kind, cb.as_ref().unchecked_ref()).is_ok() {
            cb.forget();
        }
    }
}

/// A browser tab's memory: `navigator.deviceMemory` (GB, Chrome and Edge, at most 8) tells the
/// canvas caches what the device has; Memory Usage (undo history and caches) is capped to a quarter
/// of it, and where the browser doesn't say, to 1 GB on touch devices and 2 GB elsewhere (a wasm32
/// tab can't address more than 4 GB, and mobile browsers kill tabs well before that).
fn cap_memory() {
    use wasm_bindgen::JsValue;
    let Some(window) = web_sys::window() else { return };
    let nav = window.navigator();
    let gb = js_sys::Reflect::get(&nav, &JsValue::from_str("deviceMemory")).ok().and_then(|v| v.as_f64()).filter(|v| v.is_finite() && *v > 0.0);
    let touch = nav.max_touch_points() > 0;
    let cap = match gb {
        Some(gb) => {
            let bytes = (gb * f64::from(1u32 << 30)) as u64;
            photocraft_ui_egui::gpu_canvas::set_physical_memory(bytes);
            (bytes / 4).clamp(384 << 20, 2048 << 20)
        }
        None if touch => 1024 << 20,
        None => 2048 << 20,
    };
    photocraft_engine::prefs::set_memory_cap(cap);
    log::info!("photocraft-web: memory {} GB, Memory Usage capped at {} MB", gb.map_or("unknown".to_string(), |g| g.to_string()), cap >> 20);
}

/// Preferences › Performance › Max Pixel Density (read from the saved preferences, so it applies
/// from the first frame; a change applies at next launch): the page reports a device pixel ratio
/// of at most the cap, so eframe sizes the canvas's backing store and egui draws at it, and the
/// browser scales the canvas up. A 3× phone then draws 2.25 times fewer pixels a frame.
fn cap_pixel_density() {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast as _, JsValue};
    let saved = local_storage().and_then(|s| s.get_item(PREFS_KEY).ok().flatten());
    let density =
        saved.and_then(|t| serde_json::from_str::<photocraft_engine::prefs::Preferences>(&t).ok()).map(|p| p.performance.max_pixel_density).unwrap_or_default();
    let Some(cap) = density.cap() else { return };
    let Some(window) = web_sys::window() else { return };
    let key = JsValue::from_str("devicePixelRatio");
    let Some(real) = js_sys::Reflect::get(&js_sys::Object::get_own_property_descriptor(&window, &key), &JsValue::from_str("get"))
        .ok()
        .and_then(|g| g.dyn_into::<js_sys::Function>().ok())
    else {
        return;
    };
    let this = window.clone();
    let getter = Closure::<dyn Fn() -> f64>::new(move || {
        let dpr = real.call0(&this).ok().and_then(|v| v.as_f64()).filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
        dpr.min(cap)
    });
    let desc = js_sys::Object::new();
    let set = |k: &str, v: &JsValue| js_sys::Reflect::set(&desc, &JsValue::from_str(k), v).is_ok();
    if !(set("get", getter.as_ref()) && set("configurable", &JsValue::TRUE) && set("enumerable", &JsValue::TRUE)) {
        return;
    }
    if js_sys::Reflect::define_property(&window, &key, &desc).unwrap_or(false) {
        getter.forget();
        // eframe measures the canvas in real device pixels when the browser offers them
        // (`devicePixelContentBoxSize`); without it, it takes CSS pixels × the ratio above.
        if let Ok(entry) = js_sys::Reflect::get(&window, &JsValue::from_str("ResizeObserverEntry"))
            && let Ok(proto) = js_sys::Reflect::get(&entry, &JsValue::from_str("prototype"))
            && let Some(proto) = proto.dyn_ref::<js_sys::Object>()
        {
            let _ = js_sys::Reflect::delete_property(proto, &JsValue::from_str("devicePixelContentBoxSize"));
        }
        log::info!("photocraft-web: drawing at most {cap}× device pixels per CSS pixel");
    }
}

/// While a pen is in use, fingers resting on the screen are ignored.
const PALM_WINDOW_MS: f64 = 1000.0;

thread_local! {
    /// When a pen last touched or hovered the page (`Event.timeStamp`), and whether it is down.
    static PEN_SEEN: std::cell::Cell<(f64, bool)> = const { std::cell::Cell::new((f64::NEG_INFINITY, false)) };
}

/// Palm rejection: a finger (not a stylus) that touches or moves on the page while a pen is down
/// or was used within [`PALM_WINDOW_MS`] never reaches eframe, so a hand resting on a tablet
/// doesn't paint, pan or click. Ends and cancels always pass, so egui never keeps a touch it saw
/// start. Fingers reach eframe through their touch events only (not their pointer events too).
/// Listeners on the window in the capture phase run before eframe's.
fn reject_palms() {
    use wasm_bindgen::closure::Closure;
    let Some(window) = web_sys::window() else { return };
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(true);
    opts.set_passive(false);
    let pen = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(|e: web_sys::PointerEvent| {
        if e.pointer_type() == "pen" {
            let down = match e.type_().as_str() {
                "pointerdown" => true,
                "pointerup" | "pointercancel" => false,
                _ => e.buttons() != 0,
            };
            PEN_SEEN.with(|p| p.set((e.time_stamp(), down)));
        } else if e.pointer_type() == "touch" && matches!(e.type_().as_str(), "pointerdown" | "pointerup") {
            // eframe takes a finger's press and release from its touch events too: passed on,
            // each tap was two presses, so a menu title that opens on the press closed again.
            e.stop_immediate_propagation();
        }
    });
    for kind in ["pointerdown", "pointermove", "pointerup", "pointercancel"] {
        let _ = window.add_event_listener_with_callback_and_add_event_listener_options(kind, pen.as_ref().unchecked_ref(), &opts);
    }
    pen.forget();
    let touch = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(|e: web_sys::TouchEvent| {
        if !palm_time(e.time_stamp()) {
            return;
        }
        let changed = e.changed_touches();
        // An Apple Pencil arrives as touches too, of type "stylus": those are the pen's.
        let stylus = (0..changed.length())
            .filter_map(|i| changed.get(i))
            .any(|t| js_sys::Reflect::get(&t, &wasm_bindgen::JsValue::from_str("touchType")).ok().and_then(|v| v.as_string()).as_deref() == Some("stylus"));
        if !stylus {
            e.stop_immediate_propagation();
            e.prevent_default();
        }
    });
    for kind in ["touchstart", "touchmove"] {
        let _ = window.add_event_listener_with_callback_and_add_event_listener_options(kind, touch.as_ref().unchecked_ref(), &opts);
    }
    touch.forget();
}

/// Whether a finger at `now` (`Event.timeStamp`) counts as a palm: the pen is down or was just used.
fn palm_time(now: f64) -> bool {
    let (seen, down) = PEN_SEEN.with(std::cell::Cell::get);
    down || (now - seen).abs() < PALM_WINDOW_MS
}

thread_local! {
    /// Where the last file drag was over the page (client CSS pixels): eframe's drop event carries
    /// no position.
    static DRAG_AT: std::cell::Cell<Option<(f64, f64)>> = const { std::cell::Cell::new(None) };
}

/// Track the pointer while files are dragged over the page, for where a drop lands.
fn listen_drag_position() {
    use wasm_bindgen::closure::Closure;
    let Some(window) = web_sys::window() else { return };
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(true);
    let cb = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(|e: web_sys::MouseEvent| {
        DRAG_AT.with(|d| d.set(Some((f64::from(e.client_x()), f64::from(e.client_y())))));
    });
    for kind in ["dragover", "drop"] {
        let _ = window.add_event_listener_with_callback_and_add_event_listener_options(kind, cb.as_ref().unchecked_ref(), &opts);
    }
    cb.forget();
}

/// The last drag position in egui points (the canvas fills the page; egui's points are CSS pixels
/// divided by its zoom factor).
fn drop_position(canvas: &web_sys::HtmlCanvasElement, ctx: &egui::Context) -> Option<egui::Pos2> {
    let (x, y) = DRAG_AT.with(std::cell::Cell::get)?;
    let r = canvas.get_bounding_client_rect();
    let zoom = ctx.zoom_factor().max(0.01);
    let p = egui::pos2(((x - r.left()) as f32) / zoom, ((y - r.top()) as f32) / zoom);
    (p.x.is_finite() && p.y.is_finite()).then_some(p)
}

/// Closing or reloading the tab while a document has unsaved changes asks first, as closing the
/// desktop window does: the browser shows its own "Leave site?" prompt (#1380). `unsaved` is
/// refreshed every frame by [`WebShell`].
fn guard_unload(unsaved: Arc<AtomicBool>) {
    use wasm_bindgen::closure::Closure;
    let Some(window) = web_sys::window() else { return };
    let cb = Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(move |e: web_sys::BeforeUnloadEvent| {
        if unsaved.load(Ordering::Relaxed) {
            e.prevent_default();
            // Older browsers show the prompt only when a return value is set.
            e.set_return_value("");
        }
    });
    if window.add_event_listener_with_callback("beforeunload", cb.as_ref().unchecked_ref()).is_ok() {
        cb.forget();
    }
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

/// The user's font store (IndexedDB), shared with the tasks that read and write it.
#[derive(Clone, Default)]
struct UserFontStore(std::rc::Rc<std::cell::RefCell<UserFontState>>);

#[derive(Default)]
struct UserFontState {
    db: Option<web_sys::IdbDatabase>,
    /// When the index was restored (`Date.now()`): idle reads start a moment after start-up.
    restored_at: f64,
    /// `navigator.storage.persist()` was asked for.
    persist_asked: bool,
}

/// Opens the user's font store and restores its index (no font bytes: those are read when a
/// family is needed or in idle time), without holding up the first frame.
fn open_user_fonts(store: UserFontStore, ctx: egui::Context) {
    use photocraft_ui_egui::user_fonts_ui::user_fonts::{self, Persistence};
    user_fonts::store().set_persistence(Persistence::Pending);
    wasm_bindgen_futures::spawn_local(async move {
        let opened = match crate::indexed_fonts::open().await {
            Ok(db) => crate::indexed_fonts::read_index(&db).await.map(|index| (db, index)),
            Err(e) => Err(e),
        };
        match opened {
            Ok((db, index)) => {
                let n = index.len();
                let warnings = user_fonts::store().restore(index);
                for w in warnings {
                    log::warn!("user fonts: {w}");
                }
                user_fonts::store().set_persistence(Persistence::Stored);
                {
                    let mut st = store.0.borrow_mut();
                    st.db = Some(db);
                    st.restored_at = js_sys::Date::now();
                }
                crate::startup::mark("user fonts index");
                log::info!("photocraft-web: {n} stored user fonts");
                if let Ok(p) = crate::indexed_fonts::persisted().await {
                    user_fonts::store().persisted = Some(p);
                }
            }
            Err(e) => {
                log::warn!("user fonts are session-only: {e}");
                user_fonts::store().set_persistence(Persistence::Unavailable(e));
            }
        }
        ctx.request_repaint();
    });
}

/// Reads the served font manifest, if the host has one, and lists its families in the font menus.
fn load_font_manifest(served: Served, ctx: egui::Context) {
    wasm_bindgen_futures::spawn_local(async move {
        // Most hosts serve no fonts: a missing manifest is the normal case, not an error.
        let Ok(bytes) = fetch_bytes(FONTS_MANIFEST, None).await else { return };
        let text = String::from_utf8_lossy(&bytes);
        // A host that answers unknown paths with its HTML page (single-page app fallback).
        if text.trim_start().starts_with('<') {
            return;
        }
        let (fonts, skipped) = served_fonts::parse_manifest(&text);
        for s in skipped {
            log::warn!("{FONTS_MANIFEST}: skipped {s}");
        }
        let mut files: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
        for f in &fonts {
            files.entry(f.family.clone()).or_default().push((f.file.clone(), f.integrity.clone()));
        }
        log::info!("photocraft-web: {} served font families", files.len());
        served.lock().unwrap_or_else(|e| e.into_inner()).files = files;
        // The families for the font menus, and the script fallbacks (the manifest's `scripts`).
        served_fonts::add_fonts(&fonts);
        // Fetch now the fallbacks of the scripts the browser's languages use (an Arabic reader gets
        // the Arabic font before typing); the rest is fetched when text or a font menu needs it.
        served_fonts::request_for_languages(browser_languages().iter().map(String::as_str));
        ctx.request_repaint();
    });
}

/// Translation catalogs not compiled into the web build (`photocraft-ui-egui/all-catalogs`) are
/// served next to the page as `i18n/<code>.tsv`: fetched the first time the UI needs one (the
/// language is chosen, or a preview draws in it), the UI showing English until it arrives.
const CATALOG_DIR: &str = "i18n";

fn fetch_catalogs(ctx: &egui::Context) {
    for code in i18n::take_catalog_requests() {
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let url = format!("{CATALOG_DIR}/{code}.tsv");
            let installed = match fetch_bytes(&url, None).await {
                Ok(bytes) => String::from_utf8(bytes).map_err(|e| e.to_string()).and_then(|text| {
                    // A host that answers unknown paths with its HTML page (single-page app fallback).
                    if text.trim_start().starts_with('<') { Err("not a catalog".into()) } else { i18n::provide_catalog(code, &text) }
                }),
                Err(e) => Err(e),
            };
            match installed {
                Ok(()) => ctx.request_repaint(),
                Err(e) => log::warn!("photocraft-web: {url}: {e}; showing English"),
            }
        });
    }
}

/// The browser's preferred languages (`navigator.languages`, e.g. `["ar-EG", "en"]`).
fn browser_languages() -> Vec<String> {
    web_sys::window().map(|w| w.navigator().languages().iter().filter_map(|l| l.as_string()).collect()).unwrap_or_default()
}

/// GET `url` (relative to the page) and read the whole body. With `integrity` (a Subresource
/// Integrity value), the browser rejects a body whose hash differs.
async fn fetch_bytes(url: &str, integrity: Option<&str>) -> Result<Vec<u8>, String> {
    use wasm_bindgen_futures::JsFuture;
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let request = match integrity {
        Some(sri) => {
            let init = web_sys::RequestInit::new();
            init.set_integrity(sri);
            window.fetch_with_str_and_init(url, &init)
        }
        None => window.fetch_with_str(url),
    };
    let resp: web_sys::Response = JsFuture::from(request).await.map_err(js)?.dyn_into().map_err(|_| "not a Response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = JsFuture::from(resp.array_buffer().map_err(js)?).await.map_err(js)?;
    Ok(js_sys::Uint8Array::new(&body).to_vec())
}

/// Wraps the app to read dropped files asynchronously (browsers can't read them synchronously,
/// so the app's own drop path can't handle them) and feed them through the inbox.
struct WebShell {
    app: PhotocraftApp,
    inbox: Inbox,
    /// Files dropped on the canvas, read: placed as layers, like the desktop's canvas drops.
    places: Inbox,
    canvas: web_sys::HtmlCanvasElement,
    /// Read by the `beforeunload` listener ([`guard_unload`]).
    unsaved: Arc<AtomicBool>,
    served: Served,
    /// The user's fonts in IndexedDB (`indexed_fonts`), once open.
    user_fonts: UserFontStore,
    database: Option<web_sys::IdbDatabase>,
    presets: crate::preset_bridge::Bridge,
    /// Frames begun so far, counted up to 2 (for the startup timings).
    startup_frames: u8,
    perf: crate::frame_perf::FramePerf,
}

impl WebShell {
    fn save_presets(&mut self, ctx: &egui::Context) {
        if let Some(db) = self.database.clone()
            && let Some(batch) = self.presets.begin()
        {
            let presets = self.presets.clone();
            let wake = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::indexed_presets::write(&db, &batch).await;
                if let Err(e) = &result {
                    log::error!("Could not save browser presets: {e}");
                }
                presets.finish(batch, result);
                wake.request_repaint();
            });
        }
        self.unsaved.store(self.app.has_unsaved_work() || self.presets.unsaved() || crate::embed::upload_unconfirmed(), Ordering::Relaxed);
    }

    /// Fetches the served families asked for (picked in a font menu, or needed by a layout) and
    /// installs the files that arrived.
    fn serve_fonts(&mut self, ctx: &egui::Context) {
        let requested = served_fonts::take_requests();
        let (fetch, arrived) = {
            let mut s = self.served.lock().unwrap_or_else(|e| e.into_inner());
            let mut fetch = Vec::new();
            for family in requested {
                if let Some(files) = s.files.get(&family).cloned()
                    && s.fetched.insert(family.clone())
                {
                    fetch.extend(files.into_iter().map(|(file, integrity)| (family.clone(), file, integrity)));
                }
            }
            (fetch, std::mem::take(&mut s.arrived))
        };
        for (family, file, integrity) in fetch {
            let served = self.served.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_bytes(&file, integrity.as_deref()).await {
                    Ok(bytes) => {
                        served.lock().unwrap_or_else(|e| e.into_inner()).arrived.push((family, bytes));
                        ctx.request_repaint();
                    }
                    Err(e) => log::error!("couldn't fetch served font {file}: {e}"),
                }
            });
        }
        served_fonts::install(&mut self.app, arrived);
    }

    /// The user's font store: writes the fonts loaded or removed, reads stored ones back when a
    /// layout, menu or document needs them (or in idle time), and keeps the storage figures of
    /// Manage Fonts fresh.
    fn pump_user_fonts(&mut self, ctx: &egui::Context) {
        use photocraft_ui_egui::user_fonts_ui::{Delivery, user_fonts};
        let Some(db) = self.user_fonts.0.borrow().db.clone() else { return };
        let sink = self.app.font_sink(Some(ctx));
        let (ops, want_persist, want_estimate) = {
            let mut s = user_fonts::store();
            let ops = s.take_ops();
            let persist = std::mem::take(&mut s.want_persist);
            let estimate = std::mem::take(&mut s.want_estimate);
            (ops, persist, estimate)
        };
        let wrote = !ops.is_empty();
        for op in ops {
            let db = db.clone();
            let sink = sink.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match op {
                    user_fonts::Op::Put { entry, data } => {
                        let meta = serde_json::to_string(&entry).unwrap_or_default();
                        let r = crate::indexed_fonts::put(&db, &entry.id, &meta, &data).await;
                        sink.send(Delivery::Written(entry.id, r));
                    }
                    user_fonts::Op::Delete(id) => {
                        if let Err(e) = crate::indexed_fonts::delete(&db, &id).await {
                            log::error!("couldn't delete a stored font: {e}");
                        }
                    }
                    user_fonts::Op::Clear => {
                        if let Err(e) = crate::indexed_fonts::clear(&db).await {
                            log::error!("couldn't clear the stored fonts: {e}");
                        }
                    }
                }
                user_fonts::store().want_estimate = true;
            });
        }
        let ask_persist = want_persist && !std::mem::replace(&mut self.user_fonts.0.borrow_mut().persist_asked, true);
        if ask_persist || want_estimate || wrote {
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if ask_persist {
                    match crate::indexed_fonts::persist().await {
                        Ok(p) => user_fonts::store().persisted = Some(p),
                        Err(e) => log::warn!("navigator.storage.persist: {e}"),
                    }
                }
                if let Ok(e) = crate::indexed_fonts::estimate().await {
                    user_fonts::store().estimate = Some(e);
                }
                ctx.request_repaint();
            });
        }
        // Stored fonts asked for now; one at a time in idle time, a few seconds after start-up
        // and never while the pointer is busy on the canvas.
        let since = js_sys::Date::now() - self.user_fonts.0.borrow().restored_at;
        let idle = since > 2500.0 && !ctx.input(|i| i.pointer.any_down());
        for id in photocraft_ui_egui::user_fonts_ui::take_loads(idle) {
            let db = db.clone();
            let sink = sink.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let r = crate::indexed_fonts::get(&db, &id).await;
                sink.send(Delivery::Stored(id, r));
            });
        }
        if user_fonts::store().idle_work() {
            // Keep idle reads going without input.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

impl eframe::App for WebShell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::wheel_guard::restore(raw_input);
        // The app's own hook (clipboard keys, synthetic automation input), around the browser's
        // clipboard (clipboard.rs).
        let typing = ctx.text_edit_focused() || self.app.ui.text_edit.is_some();
        crate::clipboard::before_app(typing, raw_input);
        eframe::App::raw_input_hook(&mut self.app, ctx, raw_input);
        crate::clipboard::after_app(typing, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Startup timings: the first frame starts, and the second starts once it is drawn.
        if self.startup_frames < 2 {
            self.startup_frames += 1;
            crate::startup::mark(if self.startup_frames == 1 { "first frame" } else { "first frame drawn" });
            if self.startup_frames == 2 {
                crate::startup::report();
            }
        }
        self.perf.begin(frame.info().cpu_usage);
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        // Dropped on the canvas of an open document: placed as layers; elsewhere opened.
        let on_canvas = !dropped.is_empty() && self.app.drop_places_on_canvas(ctx, drop_position(&self.canvas, ctx));
        for f in dropped {
            let inbox = if on_canvas { self.places.clone() } else { self.inbox.clone() };
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dropped".into());
                match f.bytes_async().await {
                    Ok(bytes) => {
                        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                        ctx.request_repaint();
                    }
                    Err(e) => log::error!("couldn't read dropped file {name}: {e}"),
                }
            });
        }
        let placed = std::mem::take(&mut *self.places.lock().unwrap_or_else(|e| e.into_inner()));
        for (name, bytes) in placed {
            self.app.place_dropped_bytes(name, bytes);
        }
        for (title, lines, error) in crate::clipboard::take_notices() {
            photocraft_ui_egui::notices::post(&mut self.app, i18n::t(&title), lines, error, None);
        }
        if crate::clipboard::take_finished() {
            let _ = self.app.resume_clipboard_paste(ctx);
        }
        self.serve_fonts(ctx);
        self.pump_user_fonts(ctx);
        fetch_catalogs(ctx);
        self.app.logic(ctx, frame);
        crate::embed::process(&mut self.app);
        crate::usage_stats::sync(&self.app, ctx);
        self.save_presets(ctx);
        self.perf.end_logic();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let error = self.presets.error();
        if error.is_some() || self.presets.unsaved() {
            egui::Panel::top("browser_preset_storage").show(ui, |ui| {
                if let Some(error) = error {
                    let tokens = photocraft_ui_egui::theme::Tokens::for_kind(self.app.ui.theme);
                    // Put retry first so floating brush windows cannot cover the control.
                    ui.horizontal_wrapped(|ui| {
                        if self.database.is_some() && ui.button(i18n::t("Retry saving presets")).clicked() {
                            self.presets.retry();
                            ui.ctx().request_repaint();
                        }
                        ui.colored_label(tokens.warning, i18n::fmt(i18n::t("Brush preset changes are not saved: {error}"), &[("error", &error)]));
                    });
                    ui.label(if self.database.is_some() {
                        i18n::t("Keep this tab open to retain your brushes.")
                    } else {
                        i18n::t("Browser storage is unavailable; brushes are session-only. Keep this tab open to retain them.")
                    });
                } else {
                    ui.label(i18n::t("Saving brush presets… Keep this tab open until saving finishes."));
                }
            });
        }
        self.app.ui(ui, frame);
        self.save_presets(ui.ctx());
        self.perf.end_update(&self.app, ui.ctx());
    }
}

fn services(inbox: Inbox, photopea: Option<crate::embed_protocol::PhotopeaConfig>) -> Services {
    Services {
        screen_pick: screen_color_service(),
        import: Some(Box::new(|name: &str, bytes: &[u8], max_svg_group_depth: usize| {
            let t0 = js_sys::Date::now();
            let r = photocraft_io::import_with_svg_group_depth(name, bytes, max_svg_group_depth).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string());
            crate::usage_stats::doc_open(name, bytes.len(), r.as_ref().map(|(doc, _)| doc), js_sys::Date::now() - t0);
            r
        })),
        // The desktop's options: PSD/PSB saves carry psd-compiler's merged image here too.
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let t0 = js_sys::Date::now();
            let r = photocraft_io::export(doc, path, &settings.io_options()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string());
            crate::usage_stats::doc_saved(path, doc, r.as_ref().map(|_| ()), js_sys::Date::now() - t0);
            r
        })),
        file_dialog: Some(Box::new(|request, _parent, reply| match request {
            // The browser's file picker hands over the files' contents, not paths. File › Open
            // asks for several files (`multiple`), each opened in its own tab.
            FileDialogRequest::Open { multiple, extensions, .. } => wasm_bindgen_futures::spawn_local(async move {
                let dialog = if let Some(exts) = extensions {
                    rfd::AsyncFileDialog::new().add_filter("Supported Files", &exts)
                } else {
                    rfd::AsyncFileDialog::new().add_filter("All Formats", &crate::open_formats::open_exts()).add_filter("SVG", SVG_EXTS)
                };
                let answer = if multiple {
                    match dialog.pick_files().await {
                        Some(picked) if !picked.is_empty() => {
                            let mut files = Vec::with_capacity(picked.len());
                            for file in picked {
                                files.push((file.file_name(), file.read().await));
                            }
                            Some(FileDialogAnswer::ContentsMany(files))
                        }
                        _ => None,
                    }
                } else {
                    match dialog.pick_file().await {
                        Some(file) => Some(FileDialogAnswer::Contents(file.file_name(), file.read().await)),
                        None => None,
                    }
                };
                reply.send(answer);
            }),
            // No save dialog on the web: the suggested name becomes the download name.
            FileDialogRequest::Save { suggested } => {
                let name = std::path::Path::new(&suggested).file_name().map_or_else(|| suggested.clone(), |n| n.to_string_lossy().to_string());
                reply.send(Some(FileDialogAnswer::SaveTo(name)));
            }
            // A page can't pick a folder (and the web build has no LUT library).
            FileDialogRequest::Folder => reply.send(None),
        })),
        // Embedded in Photopea's way with a save server: documents in its formats go there.
        write: Some(Box::new(move |path: &str, bytes: &[u8]| match &photopea {
            Some(config) if config.saves(path) => crate::embed::upload(config, path, bytes),
            _ => download(path, bytes),
        })),
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        inbox: Some(inbox),
        clipboard_set_image: Some(Box::new(crate::clipboard::set)),
        clipboard_get_image: Some(Box::new(crate::clipboard::get)),
        clipboard_refresh: Some(Box::new(crate::clipboard::refresh)),
        // Preferences live in the browser's localStorage.
        // Type › Load Fonts…: the page's own pickers (files, or a whole folder).
        pick_fonts: Some(Box::new(crate::font_files::pick)),
        load_prefs: Some(Box::new(|| local_storage()?.get_item(PREFS_KEY).ok().flatten())),
        save_prefs: Some(Box::new(|text: &str| local_storage().ok_or("no localStorage")?.set_item(PREFS_KEY, text).map_err(|e| format!("{e:?}")))),
        // The TypeR workspace (script, cursor, style library, settings) too, under its own key.
        load_typer: Some(Box::new(|| local_storage()?.get_item(TYPER_KEY).ok().flatten())),
        save_typer: Some(Box::new(|text: &str| local_storage().ok_or("no localStorage")?.set_item(TYPER_KEY, text).map_err(|e| format!("{e:?}")))),
        // The engine has no clock in wasm: TypeR's `edited` stamps come from here.
        clock: Some(js_sys::Date::now),
        ..Default::default()
    }
}

const PREFS_KEY: &str = "photocraft.preferences";
/// localStorage key of the TypeR workspace (`photocraft_ui_egui::typer::persist`).
const TYPER_KEY: &str = "capishot.typer.workspace";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Trigger a browser download of `bytes` named after the last component of `path`.
fn download(path: &str, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "photocraft".into());
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime_for(&name));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a").map_err(js)?.dyn_into().map_err(|_| "not an anchor")?;
    a.set_href(&url);
    a.set_download(&name);
    a.style().set_property("display", "none").map_err(js)?;
    let body = document.body().ok_or("no body")?;
    body.append_child(&a).map_err(js)?;
    a.click();
    a.remove();
    // Revoke after the click has been dispatched; the download keeps its own reference.
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        web_sys::Url::revoke_object_url(&url).ok();
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000).map_err(js)?;
    Ok(())
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("tif" | "tiff") => "image/tiff",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("psd" | "psb") => "image/vnd.adobe.photoshop",
        Some("ora") => "image/openraster",
        _ => "application/octet-stream",
    }
}

/// Rust bindings to the browser's user-activated EyeDropper API; unsupported browsers keep
/// the document picker and its normal canvas zoom. The browser owns its screen magnifier.
fn screen_color_service() -> Option<photocraft_ui_egui::screen_picker::Service> {
    use js_sys::{Function, Reflect};
    use photocraft_ui_egui::screen_picker::{Capture, Pending};
    use wasm_bindgen::{JsValue, closure::Closure};
    let window = web_sys::window()?;
    let constructor = Reflect::get(&window, &JsValue::from_str("EyeDropper")).ok()?.dyn_into::<Function>().ok()?;
    Some(Box::new(move |ctx| {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = (|| -> Result<_, String> {
            let eye = Reflect::construct(&constructor, &js_sys::Array::new()).map_err(|e| format!("Could not start browser eyedropper: {e:?}"))?;
            let controller = web_sys::AbortController::new().map_err(|e| format!("Could not create eyedropper cancellation: {e:?}"))?;
            let options = js_sys::Object::new();
            Reflect::set(&options, &JsValue::from_str("signal"), &controller.signal()).map_err(|e| format!("Could not set eyedropper options: {e:?}"))?;
            let open = Reflect::get(&eye, &JsValue::from_str("open"))
                .map_err(|e| format!("Browser eyedropper has no open method: {e:?}"))?
                .dyn_into::<Function>()
                .map_err(|_| "Invalid browser eyedropper method")?;
            let promise = open
                .call1(&eye, &options)
                .map_err(|e| format!("Could not open browser eyedropper: {e:?}"))?
                .dyn_into::<js_sys::Promise>()
                .map_err(|_| "Browser eyedropper returned no promise")?;
            let stop = cancelled.clone();
            let abort = controller.clone();
            let timer = Closure::<dyn FnMut()>::new(move || {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    abort.abort();
                }
            });
            let interval = window.set_interval_with_callback_and_timeout_and_arguments_0(timer.as_ref().unchecked_ref(), 50).map_err(|e| {
                controller.abort();
                format!("Could not watch eyedropper cancellation: {e:?}")
            })?;
            Ok((promise, timer, interval))
        })();
        match started {
            Err(e) => {
                let _ = tx.send(Err(e));
            }
            Ok((promise, timer, interval)) => {
                let wake = ctx.clone();
                let window = window.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = match wasm_bindgen_futures::JsFuture::from(promise).await {
                        Ok(value) => Reflect::get(&value, &JsValue::from_str("sRGBHex"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .and_then(|h| photocraft_ui_egui::color_picker_ui::parse_hex(&h))
                            .map(|rgb| Capture::Color(Some(rgb)))
                            .ok_or_else(|| "Browser eyedropper returned an invalid color".into()),
                        Err(e) if Reflect::get(&e, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).as_deref() == Some("AbortError") => {
                            Ok(Capture::Color(None))
                        }
                        Err(e) => Err(format!("Browser screen color selection failed: {e:?}")),
                    };
                    window.clear_interval_with_handle(interval);
                    drop(timer);
                    let _ = tx.send(result);
                    wake.request_repaint();
                });
            }
        }
        Pending { receiver: rx, cancelled }
    }))
}
