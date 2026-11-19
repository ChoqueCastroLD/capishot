//! Keeps automatic AI runs off a GPU that can't be trusted with them.
//!
//! A model's run is one long piece of GPU work. On a weak integrated GPU (an old AMD APU's
//! Radeon R2..R7 or Vega 3/6, Intel HD/UHD Graphics, little shared memory) Windows resets the GPU
//! when it takes too long, and the reset takes the window's drawing with it: the app starts again
//! and the run is lost (usage statistics: five resets in a day on a Radeon R4, during Remove with
//! AI). Two guards send automatic runs to the CPU instead ([`photocraft_engine::ai_cmds::GpuGuard`]):
//!
//! * **LostBefore**: the GPU was lost while an AI job ran ([`note_gpu_lost`], from the shell's
//!   GPU-loss handling), remembered per install in `prefs.dialogs["ai.gpu"]`.
//! * **WeakGpu**: the window's adapter is such a GPU ([`weak_integrated_gpu`]), natively.
//!
//! The first run kept on the CPU in a session says so once ([`after_run`]), and AI Options…
//! has the way back ([`options_row`]: Use the GPU Again), which clears the first and overrides
//! the second.

use std::sync::atomic::{AtomicBool, Ordering};

use photocraft_engine::ai_cmds::GpuGuard;
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// Where the guard's state lives: `{"lostBefore": bool, "gpuAnyway": bool}`.
const PREF: &str = "ai.gpu";

/// The notice of a run kept on the CPU was shown in this run of the app.
static SAID: AtomicBool = AtomicBool::new(false);

/// Whether a GPU of `device_type` (wgpu's, as `GpuInfo::device_type`: `integratedGpu`, …) named
/// `adapter`, with `vram_mb` of its own memory where known, is too weak to run AI models: an
/// integrated GPU of an old APU (Radeon R2..R7, Vega 3/6), Intel HD/UHD Graphics, or one with
/// under 1 GB. Newer integrated GPUs (Iris Xe, Arc, Radeon 680M/780M) aren't.
pub fn weak_integrated_gpu(device_type: &str, adapter: &str, vram_mb: Option<u64>) -> bool {
    if !device_type.eq_ignore_ascii_case("integratedGpu") {
        return false;
    }
    let a = adapter.to_ascii_lowercase().replace("(tm)", "").replace("(r)", "");
    let a = a.split_whitespace().collect::<Vec<_>>().join(" ");
    const WEAK: &[&str] = &[
        "radeon r2 ",
        "radeon r3 ",
        "radeon r4 ",
        "radeon r5 ",
        "radeon r6 ",
        "radeon r7 ",
        "radeon vega 3 ",
        "radeon vega 6 ",
        "intel hd graphics",
        "intel uhd graphics",
        "hd graphics ",
        "uhd graphics ",
        "intel graphics media",
    ];
    let padded = format!("{a} ");
    WEAK.iter().any(|w| padded.contains(w)) || vram_mb.is_some_and(|mb| mb > 0 && mb < 1024)
}

fn state(app: &PhotocraftApp) -> (bool, bool) {
    let v = app.session.prefs().dialogs.get(PREF);
    let flag = |k: &str| v.and_then(|v| v.get(k)).and_then(Value::as_bool).unwrap_or(false);
    (flag("lostBefore"), flag("gpuAnyway"))
}

fn store(app: &mut PhotocraftApp, lost_before: bool, gpu_anyway: bool) {
    let new = json!({"lostBefore": lost_before, "gpuAnyway": gpu_anyway});
    if app.session.prefs().dialogs.get(PREF) == Some(&new) {
        return;
    }
    app.session.edit_prefs(|p| {
        p.dialogs.insert(PREF.into(), new);
    });
    if let Err(e) = crate::prefs_ui::save_preferences(app) {
        log::warn!("couldn't save the AI device guard: {e}");
    }
}

/// The window's GPU is weak for AI ([`weak_integrated_gpu`]); never in the browser, whose adapter
/// says too little.
fn weak_gpu(app: &PhotocraftApp) -> bool {
    let info = &app.perf.gpu_info;
    !cfg!(target_arch = "wasm32") && weak_integrated_gpu(&info.device_type, &info.adapter, info.video_memory_mb)
}

/// The guard this install's runs get now.
pub fn guard(app: &PhotocraftApp) -> Option<GpuGuard> {
    let (lost_before, gpu_anyway) = state(app);
    if gpu_anyway {
        None
    } else if lost_before {
        Some(GpuGuard::LostBefore)
    } else if weak_gpu(app) {
        Some(GpuGuard::WeakGpu)
    } else {
        None
    }
}

/// Hands the guard to the session when it changed (per frame: a map lookup and a short string).
pub fn sync(app: &mut PhotocraftApp) {
    let g = guard(app);
    if app.session.ai_gpu_guard() != g {
        app.session.set_ai_gpu_guard(g);
    }
}

/// The GPU was lost: when an AI job was running, its models go to the CPU from now on.
pub fn note_gpu_lost(app: &mut PhotocraftApp) {
    if crate::telemetry::stage_name() != crate::telemetry::Stage::Ai.name() {
        return;
    }
    log::warn!("the GPU was lost during an AI run: AI runs on the CPU from now on (AI Options can switch the GPU back on)");
    store(app, true, false);
    sync(app);
}

/// What the notice and AI Options say for `g`.
pub fn message(g: GpuGuard) -> &'static str {
    match g {
        GpuGuard::LostBefore => tl!("AI runs on the CPU: the GPU was reset during an AI run on this computer. AI Options can switch the GPU back on."),
        GpuGuard::WeakGpu => tl!("AI runs on the CPU: this integrated GPU can reset during a model's run. AI Options can switch the GPU back on."),
    }
}

/// A finished AI job's reply: the first one kept on the CPU in this run of the app says so.
pub fn after_run(app: &mut PhotocraftApp, reply: &Value) {
    let g = match reply.get("gpuGuard").and_then(Value::as_str) {
        Some("gpuLostBefore") => GpuGuard::LostBefore,
        Some("weakGpu") => GpuGuard::WeakGpu,
        _ => return,
    };
    if !SAID.swap(true, Ordering::Relaxed) {
        crate::notices::post(app, message(g), Vec::new(), false, None);
    }
}

/// The AI Options row: why runs are on the CPU, and Use the GPU Again.
pub fn options_row(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(g) = guard(app) else { return };
    let tokens = crate::theme::Tokens::get(ui.ctx());
    ui.add_space(4.0);
    ui.label(egui::RichText::new(message(g)).color(tokens.warning));
    if ui.button(tl!("Use the GPU again")).clicked() {
        use_gpu_again(app);
    }
}

/// Automatic runs may use the GPU again (and a weak one too).
pub fn use_gpu_again(app: &mut PhotocraftApp) {
    store(app, false, true);
    sync(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_apus_and_intel_hd_are_weak_newer_integrated_gpus_are_not() {
        for name in ["AMD Radeon(TM) R4 Graphics", "AMD Radeon R5 Graphics", "Intel(R) HD Graphics 530", "Intel(R) UHD Graphics 600", "AMD Radeon(TM) Vega 3 Graphics"] {
            assert!(weak_integrated_gpu("integratedGpu", name, None), "{name}");
        }
        for name in ["Intel(R) Iris(R) Xe Graphics", "AMD Radeon(TM) 780M", "AMD Radeon(TM) Graphics", "Intel(R) Arc(TM) Graphics", "AMD Radeon R7 200 Series"] {
            assert!(!weak_integrated_gpu("integratedGpu", name, None), "{name}");
        }
        // A discrete GPU never is, whatever its name; little memory of its own makes one weak.
        assert!(!weak_integrated_gpu("discreteGpu", "AMD Radeon(TM) R4 Graphics", None));
        assert!(weak_integrated_gpu("integratedGpu", "Some GPU", Some(512)));
        assert!(!weak_integrated_gpu("integratedGpu", "Some GPU", Some(2048)));
    }

    #[test]
    fn a_gpu_lost_during_an_ai_run_keeps_ai_on_the_cpu_until_the_user_says_otherwise() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        sync(&mut app);
        assert_eq!(app.session.ai_gpu_guard(), None);
        // Lost while idle: nothing changes.
        note_gpu_lost(&mut app);
        assert_eq!(app.session.ai_gpu_guard(), None);
        crate::telemetry::set_stage(crate::telemetry::Stage::Ai);
        note_gpu_lost(&mut app);
        crate::telemetry::leave_stage(crate::telemetry::Stage::Ai);
        assert_eq!(app.session.ai_gpu_guard(), Some(GpuGuard::LostBefore));
        assert_eq!(guard(&app), Some(GpuGuard::LostBefore), "remembered in the preferences");
        use_gpu_again(&mut app);
        assert_eq!(app.session.ai_gpu_guard(), None);
        // A weak integrated GPU is guarded until the user asks for it.
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.perf.gpu_info.device_type = "integratedGpu".into();
        app.perf.gpu_info.adapter = "AMD Radeon(TM) R4 Graphics".into();
        sync(&mut app);
        assert_eq!(app.session.ai_gpu_guard(), Some(GpuGuard::WeakGpu));
        use_gpu_again(&mut app);
        assert_eq!(app.session.ai_gpu_guard(), None);
    }
}
