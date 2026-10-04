//! Photocraft desktop app.
//!
//! Usage: `photocraft [--control <port>] [--control-token <64-hex> |
//! --control-token-file <path>] [--automation-read-root <dir>]
//! [--automation-write-root <dir>] [files…]`
//!
//! `--control <port>` (or `PHOTOCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server.
//! The first line must authenticate; subsequent request lines get reply lines.
//! `{"id":1,"ok":true,"result":…}`. See `photocraft_ui_egui::control` for the methods.

// Release builds on Windows are GUI-subsystem apps, so launching from the Start Menu or Explorer
// doesn't open a console window. (`--version` output then only shows when redirected.)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "macos")]
mod apple_events;
mod control_server;
mod crash_guard;
mod monitor_profile;
mod services;

use photocraft_engine::Session;
use photocraft_ui_egui::PhotocraftApp;

/// Matches the `.desktop` file and hicolor icon name, so Wayland docks pick up the icon.
const APP_ID: &str = "ai.storyteller.photocraft";

/// Window, taskbar and (when running unbundled) Dock icon. macOS gets the padded 1024 px render
/// on Apple's icon grid; elsewhere the tighter 256 px hicolor render reads better at small sizes.
fn app_icon() -> egui::IconData {
    #[cfg(target_os = "macos")]
    const PNG: &[u8] = include_bytes!("../../../assets/app-icon/photocraft-1024.png");
    #[cfg(not(target_os = "macos"))]
    const PNG: &[u8] = include_bytes!("../../../assets/app-icon/hicolor/256x256/apps/ai.storyteller.photocraft.png");
    eframe::icon_data::from_png_bytes(PNG).unwrap_or_default()
}

fn main() -> eframe::Result {
    crash_guard::install_hook();
    let mut control_port: Option<u16> = std::env::var("PHOTOCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse().ok());
    let mut control_token = None;
    let mut control_token_file = None;
    let mut automation_read_root = std::env::var_os("PHOTOCRAFT_AUTOMATION_READ_ROOT").map(std::path::PathBuf::from);
    let mut automation_write_root = std::env::var_os("PHOTOCRAFT_AUTOMATION_WRITE_ROOT").map(std::path::PathBuf::from);
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => control_port = args.next().and_then(|p| p.parse().ok()),
            "--control-token" => control_token = args.next(),
            "--control-token-file" => control_token_file = args.next().map(std::path::PathBuf::from),
            "--automation-read-root" => automation_read_root = args.next().map(std::path::PathBuf::from),
            "--automation-write-root" => automation_write_root = args.next().map(std::path::PathBuf::from),
            "--version" => {
                println!("photocraft {}", photocraft_engine::build_info::long_version());
                return Ok(());
            }
            // Old macOS passes a process serial number when launched from Finder.
            _ if a.starts_with("-psn_") => {}
            _ => files.push(a),
        }
    }

    let control = if let Some(port) = control_port {
        let (supplied, token_file) = photocraft_automation::security::token_inputs(control_token, control_token_file);
        let token = match photocraft_automation::security::server_token(supplied.as_deref(), token_file.as_deref()) {
            Ok(token) => token,
            Err(e) => {
                eprintln!("photocraft: cannot configure control authentication: {e}");
                return Ok(());
            }
        };
        if let Some(path) = token_file {
            eprintln!("photocraft: control token file: {}", path.display());
        } else if supplied.is_none() {
            eprintln!("photocraft: control token: {token}");
        } else {
            eprintln!("photocraft: using supplied control token");
        }
        let workspace = match photocraft_automation::AuthorizedWorkspace::new(automation_read_root.as_deref(), automation_write_root.as_deref()) {
            Ok(workspace) => workspace,
            Err(error) => {
                eprintln!("photocraft: cannot configure automation workspace: {error}");
                return Ok(());
            }
        };
        Some((port, token, workspace))
    } else {
        None
    };
    // Finder / Dock / Open With deliver files as Apple events, not arguments; catch the one that
    // launched us as well as later ones. Lives until the event loop returns.
    #[cfg(target_os = "macos")]
    let apple_events = apple_events::AppleEvents::install();
    #[cfg(target_os = "macos")]
    let apple_events = &apple_events;

    // Read the main display's ICC profile while the window opens (colour-managed canvas).
    let monitor = monitor_profile::detect_async();
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(app_icon())
            .with_app_id(APP_ID)
            .with_title("PhotoCraft")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true)
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    // The adapter's real texture limits (egui asks for 8192 px), so big documents stay on the GPU.
    photocraft_ui_egui::gpu_canvas::use_adapter_limits(&mut options.wgpu_options.wgpu_setup);
    eframe::run_native(
        "Photocraft",
        options,
        Box::new(move |cc| {
            let automation = control.as_ref().map(|(_, _, workspace)| workspace.clone());
            let mut app = PhotocraftApp::new(Session::new(), services::native(automation));
            app.integrated_titlebar = cfg!(target_os = "macos");
            if let Ok(Some(icc)) = monitor.recv_timeout(std::time::Duration::from_secs(2)) {
                app.session.color.monitor_profile = Some(std::sync::Arc::new(icc));
            }
            // Preferences › Performance › Use Graphics Processor.
            if let Some(rs) = cc.wgpu_render_state.clone()
                && std::env::var_os("PHOTOCRAFT_CPU_CANVAS").is_none()
                && app.session.prefs().performance.use_gpu
            {
                app.set_wgpu(rs);
            }
            if let Some((port, token, _)) = control {
                let rx = control_server::start(port, token, cc.egui_ctx.clone());
                app = app.with_control(rx);
            }
            #[cfg(target_os = "macos")]
            {
                app.services.os_events = Some(apple_events.connect(&cc.egui_ctx));
            }
            // Paths on the command line (Linux/Windows file associations, `photocraft a.psd`).
            app.open_paths(&files);
            Ok(Box::new(app))
        }),
    )
}
