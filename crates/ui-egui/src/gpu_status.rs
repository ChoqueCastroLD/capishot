//! GPU health in the shell (#243, #4): when the wgpu device is lost or reports an error, the
//! GPU canvas is dropped and every document keeps drawing through the CPU compositor and egui
//! textures for the rest of the session, with a recovery warning. Also runs the desktop app's
//! "started" hook once the first frames have rendered (the crash-safe startup marker), and
//! provides the Help › System Info text (including the monitor profile in use).

use serde_json::json;

use crate::PhotocraftApp;

/// Notice title when the device was lost.
pub const LOST_MESSAGE: &str = "GPU device was lost; using the CPU renderer.";
/// Notice title when the device reported an error (out of memory, validation, internal).
pub const ERROR_MESSAGE: &str = "GPU error; using the CPU renderer.";
/// Frames after which startup counts as done even if a document never drew.
const STARTED_MAX_FRAMES: u64 = 120;

/// Hook run once the app has rendered its first frames (see [`PhotocraftApp::on_started`]).
pub type StartedHook = Box<dyn FnOnce(&mut PhotocraftApp)>;

/// Per-frame check: switch to the CPU canvas when the GPU faulted, and run the started hook.
pub fn check(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if let Some(fault) = app.gpu.as_ref().and_then(|g| g.fault()) {
        fall_back(app, &fault);
        ctx.request_repaint();
    }
    if app.started.is_some() {
        // Frames 1–2 set up fonts; by frame 3 the window has presented, and a document open at
        // launch has drawn its canvas (or the GPU path was abandoned).
        let drawn = app.session.documents().is_empty() || !app.perf.last_refresh.is_empty() || app.gpu.is_none();
        if app.frame >= 3 && (drawn || app.frame >= STARTED_MAX_FRAMES) {
            if let Some(hook) = app.started.take() {
                hook(app);
            }
        } else {
            ctx.request_repaint();
        }
    }
}

/// End-of-frame check of the device the window itself draws with (#3438). Once wgpu reports it
/// lost (a driver reset, for example after the PC slept), egui-wgpu can't draw another frame: one
/// with shapes panics creating its staging buffers (#1215), and one without leaves a frozen window
/// whose close button opens an unsaved-changes prompt nobody can see. So this frame and every later
/// one draw nothing; the first writes every unsaved document to crash recovery, waits for the
/// writes, and closes the app without asking once all of them are saved. The next launch reopens
/// the documents. If a write fails (a full disk, say) the app stays open with the documents in
/// memory and tries again every [`RECOVERY_RETRY_SECS`]: closing would lose them.
pub fn check_window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some((device, health)) = app.window_device.as_ref() else { return };
    // A loss while the app sat idle or the PC slept is noticed here, before the next frame.
    health.probe(device);
    if !health.device_lost() {
        return;
    }
    ctx.graphics_mut(|g| *g = Default::default());
    let now = ctx.input(|i| i.time);
    if !app.window_lost {
        app.window_lost = true;
        let reason = health.fault().map_or_else(|| "GPU device lost".to_string(), |f| f.to_string());
        log::error!("{reason}: the window can't be drawn any more; saving unsaved documents for recovery and closing");
    } else {
        match app.window_recovery_retry {
            // Saved, and the close is on its way.
            None => return,
            Some(at) if now < at => {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((at - now).max(0.0)));
                return;
            }
            Some(_) => {}
        }
    }
    match save_for_recovery(app, ctx) {
        Ok(()) => {
            app.window_recovery_retry = None;
            app.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Err(e) => {
            log::error!("{e}; keeping the documents open and trying again in {RECOVERY_RETRY_SECS} s");
            app.window_recovery_retry = Some(now + RECOVERY_RETRY_SECS);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(RECOVERY_RETRY_SECS));
        }
    }
}

/// Seconds between attempts to save unsaved work for recovery after the window's device was lost.
pub const RECOVERY_RETRY_SECS: f64 = 2.0;

/// Write every document with unsaved changes to crash recovery (Capishot's checkpoint writer,
/// `prefs_ui::tick`), whatever the autosave timer, and report whether each is known to be on
/// disk at its current revision. Until then the caller keeps the documents open and retries.
pub fn save_for_recovery(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    if app.services.autosave.is_none() {
        return Err("no crash-recovery autosave".into());
    }
    crate::prefs_ui::autosave_now(app);
    crate::prefs_ui::tick(app, ctx);
    let pending = crate::prefs_ui::recovery_pending(app);
    if pending.is_empty() { Ok(()) } else { Err(format!("waiting for recovery checkpoints of {}", pending.join(", "))) }
}

/// Drop the GPU canvas after `fault`: free its resources, send every view through the CPU
/// path, record why, and tell the user. Documents are untouched.
pub fn fall_back(app: &mut PhotocraftApp, fault: &photocraft_gpu::Fault) {
    let Some(gpu) = app.gpu.take() else { return };
    log::error!("{fault}; using the CPU renderer for the rest of the session");
    gpu.release();
    // Canvas caches pointed at GPU textures: rebuild them as egui textures.
    app.canvases.clear();
    app.proxy_uploaded = None;
    app.prefs_rt.gpu_style = None;
    app.perf.gpu = false;
    app.perf.gpu_fallback = Some(fault.to_string());
    // The CPU canvas has no GPU memory budget: GPU Memory Full no longer applies.
    app.perf.gpu_layer_fit = None;
    app.perf.gpu_info.canvas = "cpu".into();
    app.perf.gpu_info.lost = Some(fault.to_string());
    let title = if fault.is_lost() { LOST_MESSAGE } else { ERROR_MESSAGE };
    // Unsaved work goes to crash recovery now, not at the next autosave: a lost device is often
    // the start of a crash (the browser may drop the tab next).
    crate::prefs_ui::autosave_now(app);
    queue_fallback_notice(app, fault.to_string());
    app.ui.status = tl!(title).to_string();
    app.ui.status_error = true;
}

/// English diagnostics for issue reports (Copy and the control API), independent of UI language.
pub fn system_info(app: &PhotocraftApp) -> Vec<String> {
    crate::i18n::with_language(crate::i18n::Lang::EN, || system_info_ui(app))
}

/// Help › System Info's on-screen text: version, platform and the graphics state.
pub fn system_info_ui(app: &PhotocraftApp) -> Vec<String> {
    let mut v = vec![format!("Capishot {}", photocraft_engine::build_info::long_version()), {
        let template = "Platform: {os} {arch}";
        crate::i18n::fmt(tl!(template), &[("os", std::env::consts::OS), ("arch", std::env::consts::ARCH)])
    }];
    v.extend(app.perf.gpu_info.lines());
    v.extend(crate::monitor_status::summary_lines(app));
    v
}

/// The `help.systemInfo` result: the same facts as JSON.
pub fn system_info_json(app: &PhotocraftApp) -> serde_json::Value {
    json!({
        "version": photocraft_engine::build_info::long_version(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "gpu": app.perf.gpu_info,
        "monitor": app.session.color.monitor_status(),
        "displays": app.session.color.display_statuses(),
        "lines": system_info(app),
    })
}

/// Queue the same recovery warning for startup and runtime failures.
pub fn queue_fallback_notice(app: &mut PhotocraftApp, reason: impl Into<String>) {
    app.ui.gpu_fallback_notice = Some(reason.into());
}

/// Store the next-launch choice without restarting or risking unsaved documents.
fn choose_recovery(app: &mut PhotocraftApp, retry: bool) -> Result<(), String> {
    app.run(
        "prefs.set",
        json!({"values": {
            "performance.renderingMode": if retry { "gpu" } else { "cpu" },
            "performance.useGpu": retry,
            "performance.gpuBackend": "auto"
        }}),
    )?;
    crate::prefs_ui::save_preferences(app)?;
    app.ui.gpu_fallback_notice = None;
    if retry {
        crate::notices::post(
            app,
            "GPU retry scheduled",
            vec!["Save your work and restart Capishot to retry GPU acceleration. CPU rendering remains active for this session.".into()],
            false,
            None,
        );
    }
    Ok(())
}

/// Once a frame: the WebGL canvas's pipelines are built one at a time ([`GpuCanvas::warm_up`]);
/// one that took too long keeps this device on the CPU canvas ([`keep_cpu_canvas`]).
pub fn warm_up(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(gpu) = app.gpu.as_ref() else { return };
    if gpu.warm_up() {
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    }
    if !app.gpu_slow_noted
        && let Some(ms) = gpu.slow_pipelines()
    {
        app.gpu_slow_noted = true;
        keep_cpu_canvas(app, "slow_pipelines", 1, Some(ms));
    }
}

/// [`save_for_recovery`] for every open document, saved or not: the page reloads after its
/// graphics context was lost, and what was open comes back from crash recovery (the browser
/// otherwise keeps checkpoints of unsaved work only).
pub fn save_all_for_recovery(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    if app.services.autosave.is_none() {
        return Err("no crash-recovery autosave".into());
    }
    app.services.autosave_unsaved_only = false;
    crate::prefs_ui::autosave_now(app);
    crate::prefs_ui::tick(app, ctx);
    let pending = crate::prefs_ui::recovery_pending_all(app);
    if pending.is_empty() { Ok(()) } else { Err(format!("waiting for recovery checkpoints of {}", pending.join(", "))) }
}

/// Whether a WebGL renderer is too weak for the GPU canvas (its shaders took seconds to build
/// and the browser dropped the context): software rendering, old Intel HD Graphics (up to the
/// 4600) and GMA, or textures under 8192 px a side. Such a device starts on the CPU canvas.
pub fn weak_webgl(renderer: &str, max_texture: u32) -> bool {
    if max_texture > 0 && max_texture < 8192 {
        return true;
    }
    let r = renderer.to_lowercase();
    if ["swiftshader", "basic render", "llvmpipe", "softpipe", "software"].iter().any(|w| r.contains(w)) || r.contains("gma") {
        return true;
    }
    if let Some(at) = r.find("hd graphics")
        && r.contains("intel")
    {
        // "Intel(R) HD Graphics 4000", "Intel HD Graphics" (no number: Ironlake, Sandy Bridge's
        // lowest); "HD Graphics 520"/"620" (Skylake on) are fine.
        let digits: String = r[at + "hd graphics".len()..].trim_start().chars().take_while(char::is_ascii_digit).collect();
        return digits.parse::<u32>().map_or(true, |n| (1000..=4600).contains(&n));
    }
    false
}

/// The page's address `href` with `cpu=1` (the CPU canvas), its query and hash kept.
pub fn cpu_url(href: &str) -> String {
    let (base, hash) = match href.split_once('#') {
        Some((b, h)) => (b, Some(h)),
        None => (href, None),
    };
    let has = base.split_once('?').is_some_and(|(_, q)| q.split('&').any(|kv| kv == "cpu" || kv.starts_with("cpu=")));
    let mut url = base.to_string();
    if !has {
        url.push(if base.contains('?') { '&' } else { '?' });
        url.push_str("cpu=1");
    }
    if let Some(h) = hash {
        url.push('#');
        url.push_str(h);
    }
    url
}

/// Whether a page whose graphics context was lost at `since` should reload now: crash recovery
/// holds every unsaved document (`saved`), or 8 s passed.
pub fn ready_to_reload(saved: bool, since: f64, now: f64) -> bool {
    saved || now - since >= 8000.0
}

/// The class of the window's GPU for the usage statistics ("webgl", "webgpu", a native backend).
pub fn api_class(app: &PhotocraftApp) -> &'static str {
    match app.perf.gpu_info.backend.to_lowercase().as_str() {
        "gl" | "gles" | "webgl" => "webgl",
        "browserwebgpu" | "webgpu" => "webgpu",
        "vulkan" => "vulkan",
        "dx12" => "dx12",
        "metal" => "metal",
        _ => "unknown",
    }
}

/// From now on this device draws documents with the CPU canvas (Preferences › Performance ›
/// Rendering: CPU), and the user is told why and how to undo it. `cause` and `n` (times this
/// session) go to the usage statistics (`gpu_lost`).
pub fn keep_cpu_canvas(app: &mut PhotocraftApp, cause: &str, n: u32, ms: Option<f64>) {
    record_gpu_lost(app, cause, n, ms);
    let _ = app.run("prefs.set", serde_json::json!({"values": {"performance.renderingMode": "cpu"}}));
    crate::notices::post(
        app,
        tl!("Capishot will draw on the CPU on this device"),
        vec![tl!("The graphics card was too slow or restarted while drawing. To use it again: Preferences › Performance › Rendering Mode.").to_string()],
        false,
        None,
    );
}

/// The `gpu_lost` usage event (see [`crate::telemetry::Recorder::gpu_lost`]).
pub fn record_gpu_lost(app: &PhotocraftApp, cause: &str, n: u32, ms: Option<f64>) {
    let api = api_class(app);
    let gpu = crate::telemetry::gpu_vendor_class(0, &app.perf.gpu_info.adapter);
    let phase = crate::perf_span::top(0.0);
    crate::telemetry::with(|r| r.gpu_lost(api, gpu, cause, phase, n, ms));
}

/// Recovery is an explicit choice; the CPU document renderer stays active while the user works.
pub fn show_fallback(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(reason) = app.ui.gpu_fallback_notice.clone() else { return };
    let t = crate::theme::Tokens::get(ctx);
    let mut choice = None;
    egui::Window::new(tl!("Switched to CPU rendering"))
        .id(egui::Id::new("gpu-fallback-warning"))
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(tl!("GPU acceleration could not continue. Your documents are unchanged.")).color(t.warning));
            ui.label(tl!("Capishot is using the CPU image compositor. The window may still use your graphics adapter."));
            ui.collapsing(tl!("Details"), |ui| {
                ui.label(&reason);
            });
            ui.label(tl!("Retry GPU requires a restart. Save your work first."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                use crate::widgets::{ButtonRole, DialogButton};
                let buttons =
                    [DialogButton::new(ButtonRole::Default, tl!("Keep Using CPU"), 150.0), DialogButton::new(ButtonRole::Alternate, tl!("Retry GPU"), 120.0)];
                if let Some(role) = crate::widgets::dialog_buttons(ui, &buttons) {
                    choice = Some(role == ButtonRole::Alternate);
                }
            });
        });
    if let Some(retry) = choice
        && let Err(error) = choose_recovery(app, retry)
    {
        crate::notices::error(app, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_webgl_renderers_start_on_the_cpu() {
        for weak in [
            "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) (0x0000C0DE)), SwiftShader driver)",
            "ANGLE (Microsoft, Microsoft Basic Render Driver Direct3D11 vs_5_0 ps_5_0, D3D11)",
            "ANGLE (Intel, Intel(R) HD Graphics 4000 Direct3D11 vs_5_0 ps_5_0, D3D11-10.18.10.4358)",
            "ANGLE (Intel, Intel(R) HD Graphics Direct3D11 vs_4_1 ps_4_1, D3D11)",
            "Mobile Intel(R) 4 Series Express Chipset Family (GMA 4500MHD)",
            "llvmpipe (LLVM 15.0.7, 256 bits)",
        ] {
            assert!(weak_webgl(weak, 16384), "{weak}");
        }
        for fine in [
            "ANGLE (Intel, Intel(R) HD Graphics 620 Direct3D11 vs_5_0 ps_5_0, D3D11)",
            "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)",
            "ANGLE (NVIDIA, NVIDIA GeForce GTX 1060 Direct3D11 vs_5_0 ps_5_0, D3D11)",
            "Adreno (TM) 640",
            "",
        ] {
            assert!(!weak_webgl(fine, 16384), "{fine}");
        }
        assert!(weak_webgl("Adreno (TM) 640", 4096), "small textures");
    }

    #[test]
    fn the_cpu_url_keeps_the_query_and_hash() {
        assert_eq!(cpu_url("https://x.test/capishot/"), "https://x.test/capishot/?cpu=1");
        assert_eq!(cpu_url("https://x.test/?lang=es#{\"files\":[]}"), "https://x.test/?lang=es&cpu=1#{\"files\":[]}");
        assert_eq!(cpu_url("https://x.test/?cpu=1"), "https://x.test/?cpu=1");
    }

    #[test]
    fn a_lost_page_waits_for_recovery_at_most_a_while() {
        assert!(!ready_to_reload(false, 0.0, 100.0));
        assert!(ready_to_reload(true, 0.0, 100.0));
        assert!(ready_to_reload(false, 0.0, 9000.0));
    }

    /// Too slow pipelines: the CPU canvas from now on, said once, counted.
    #[test]
    fn slow_pipelines_keep_the_cpu_canvas() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        keep_cpu_canvas(&mut app, "slow_pipelines", 1, Some(4840.0));
        assert_eq!(app.session.prefs().performance.effective_rendering_mode(), photocraft_engine::prefs::RenderingMode::Cpu);
        assert_eq!(app.ui.notices.len(), 1);
    }

    #[test]
    fn fallback_warning_keeps_reason_in_inspectable_state() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        queue_fallback_notice(&mut app, "device unavailable");
        let state = serde_json::to_value(&app.ui).unwrap();
        assert_eq!(state["gpu_fallback_notice"], "device unavailable");
        assert_eq!(crate::control::inspect(&app, &egui::Context::default())["gpuFallbackNotice"], "device unavailable");
    }

    #[test]
    fn failed_preference_save_keeps_recovery_warning_open() {
        let services = crate::Services { save_prefs: Some(Box::new(|_| Err("disk full".into()))), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        queue_fallback_notice(&mut app, "GPU fault");
        assert!(choose_recovery(&mut app, true).is_err());
        assert_eq!(app.ui.gpu_fallback_notice.as_deref(), Some("GPU fault"));
        assert!(app.ui.notices.is_empty());
    }

    #[test]
    fn recovery_choices_save_next_launch_mode_without_restarting() {
        let saves = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = saves.clone();
        let services = crate::Services {
            save_prefs: Some(Box::new(move |_| {
                count.set(count.get() + 1);
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        queue_fallback_notice(&mut app, "device unavailable");
        choose_recovery(&mut app, false).unwrap();
        assert!(!app.session.prefs().performance.use_gpu);
        assert_eq!(app.session.prefs().get("performance.renderingMode"), Some(json!("cpu")));
        assert!(app.ui.gpu_fallback_notice.is_none());
        crate::prefs_ui::tick(&mut app, &egui::Context::default());
        assert_eq!(saves.get(), 1, "recovery choice writes once");
        choose_recovery(&mut app, true).unwrap();
        assert!(app.session.prefs().performance.use_gpu);
        assert_eq!(app.session.prefs().get("performance.renderingMode"), Some(json!("gpu")));
        assert!(app.gpu.is_none());
        crate::prefs_ui::tick(&mut app, &egui::Context::default());
        assert_eq!(saves.get(), 2, "retry choice writes once");
    }
}
