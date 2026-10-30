//! Render the full Capishot UI offscreen (no window, no focus stealing) and save a PNG.
//!
//! ```sh
//! cargo run --release -p photocraft-ui-egui --example snapshot -- \
//!     --out ui.png --size 1440x900 --scale 2 --open photo.jpg \
//!     --script '[["ui.set", {"tool": "type"}], ["ui.menu.invoke", {"id": "image.imageSize"}]]'
//! ```
//!
//! `--system-theme light|dark|none` supplies a deterministic OS appearance for theme captures.
//!
//! `--fake-ai` injects a stand-in AI upscaler (models "need a download") for Upscale with AI….
//!
//! `--safe-gpu` draws the canvas on the CPU path, like the app's `--safe-gpu` launch.
//! `--hardware-filtering` respects texture samplers; the default predictable renderer forces bilinear filtering.
//! `--wayland-notice` previews the native file drag-and-drop guidance shown in Wayland sessions.
//! `--update-notice` previews a newer release without contacting the network.
//! `--custom-titlebar` draws the Windows/Linux title bar (caption buttons in the top bar).
//!
//! `--monitor 1366x768 --window-top 31` simulates the display the window is on (in points) and
//! where its content starts on it, e.g. a window running under a Windows taskbar.
//!
//! `--script` is a JSON array of `[method, params]` control-protocol calls (see
//! docs/control-protocol.md), applied in order with a few frames between them.
//! `--dev-fonts` attaches a fake Google Fonts backend (no network: a demo index, fonts served from
//! memory; Oswald's download never finishes, to show progress) and turns online fonts on with
//! `--allow-online-fonts`, so the font picker's Find More tab can be captured.
//! `--click-wait-ms N` keeps rendering N ms after each click.
//! `--right-click-at X,Y` opens a screen-space context menu after the script, including panel
//! and document-tab menus that are outside the document-coordinate control pointer.
//! `--click-at X,Y` opens a screen-space menu (for example the top Select menu) after the script.
//! `--drag-from X,Y --drag-to X,Y` captures a screen-space drag with the button held.

// The fake Google Fonts backend of the shell's tests (`--dev-fonts`).
#[path = "../tests/support/dev_fonts.rs"]
mod dev_fonts;

use photocraft_ui_egui::control::{ControlRequest, Outcome, handle};
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::Value;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = arg(&args, "--out").unwrap_or_else(|| "snapshot.png".into());
    let (w, h) = arg(&args, "--size")
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))))
        .unwrap_or((1440.0, 900.0));
    let scale: f32 = arg(&args, "--scale").and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let wayland_notice = args.iter().any(|a| a == "--wayland-notice");
    let update_notice = args.iter().any(|a| a == "--update-notice");
    let script: Vec<(String, Value)> =
        arg(&args, "--script").map(|s| serde_json::from_str::<Vec<(String, Value)>>(&s).expect("--script must be [[method, params], …]")).unwrap_or_default();

    let mut services = Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
            photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc: &photocraft_doc::Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let mut opts = photocraft_io::ExportOptions::default();
            if let Some(q) = settings.jpeg_quality {
                opts.encode.jpeg_quality = q;
            }
            opts.encode.jpeg_format = settings.jpeg_format;
            opts.encode.webp_lossless = settings.webp_lossless;
            if let Some(q) = settings.webp_quality {
                opts.encode.webp_quality = q;
            }
            opts.tga_bits = settings.tga_bits;
            opts.encode.dds = settings.dds;
            photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        write: Some(Box::new(|path: &str, bytes: &[u8]| photocraft_format::atomic_write(std::path::Path::new(path), bytes).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    // Exercise the actual autosave error/status path without a server or window.
    let recovery_error = arg(&args, "--recovery-error");
    if let Some(error) = recovery_error.clone() {
        services.autosave = Some(Box::new(move |_, _, _, _, _, _| Err(error.clone())));
    }
    if let Some(dir) = arg(&args, "--recover-dir") {
        services.recover = Some(Box::new(move || {
            let (entries, errors) = photocraft_format::list_recovery_checked(std::path::Path::new(&dir));
            let mut batch = photocraft_ui_egui::RecoveryBatch { documents: Vec::new(), errors };
            for entry in entries {
                match photocraft_format::recover_checkpoint_with_context(&entry) {
                    Ok((document, history, context)) => batch.documents.push(photocraft_ui_egui::RecoveredDocument {
                        key: entry.info.key,
                        path: entry.info.original_path,
                        document,
                        history: Some(history),
                        context,
                    }),
                    Err(error) => batch.errors.push(error.to_string()),
                }
            }
            batch
        }));
    }
    let open = arg(&args, "--open");
    let safe_gpu = args.iter().any(|a| a == "--safe-gpu");
    // `--background-jobs`: long commands run as background jobs, as in the desktop app (#210).
    let background_jobs = args.iter().any(|a| a == "--background-jobs");
    let custom_titlebar = args.iter().any(|a| a == "--custom-titlebar");
    let dev_fonts = args.iter().any(|a| a == "--dev-fonts");
    let fake_ai = args.iter().any(|a| a == "--fake-ai");
    let allow_online_fonts = args.iter().any(|a| a == "--allow-online-fonts");
    // `--settle-ms N`: keep rendering frames for N ms before the capture (e.g. mid-job).
    let click_wait_ms: u64 = arg(&args, "--click-wait-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let settle_ms: u64 = arg(&args, "--settle-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut render_options = eframe::egui_wgpu::RendererOptions::PREDICTABLE;
    render_options.predictable_texture_filtering = !args.iter().any(|a| a == "--hardware-filtering");
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(w, h))
        .with_pixels_per_point(scale)
        .with_max_steps(64)
        .with_render_options(render_options)
        .wgpu()
        .build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut services = services;
            services.is_wayland = wayland_notice;
            services.xwayland_command = wayland_notice.then(|| "WAYLAND_DISPLAY= photocraft".into());
            let mut session = photocraft_engine::Session::new();
            if dev_fonts {
                let mk = |name, dir, files, size, category: &'static str, variable| dev_fonts::Fam {
                    category,
                    variable,
                    ..dev_fonts::Fam::listed(name, dir, files, size)
                };
                let mut oswald = mk("Oswald", "oswald", 3, 0, "SANS_SERIF", true);
                oswald.hang_on = Some(1);
                let fams = [
                    mk("Noto Sans JP", "notosansjp", 1, 9_589_900, "SANS_SERIF", true),
                    mk("Roboto Mono", "robotomono", 2, 540 << 10, "MONOSPACE", true),
                    mk("Lora", "lora", 2, 560 << 10, "SERIF", true),
                    oswald,
                    mk("Pacifico", "pacifico", 1, 330 << 10, "HANDWRITING", false),
                    mk("Playfair Display", "playfairdisplay", 2, 1_100_000, "SERIF", true),
                    dev_fonts::Fam::real("Anton", "anton"),
                    dev_fonts::Fam::real("Karla", "karla"),
                ];
                let fake = dev_fonts::DevFonts::new("snapshot", &fams);
                fake.attach(&mut session);
            }
            if allow_online_fonts {
                session.edit_prefs(|p| p.type_.allow_online_fonts = true);
            }
            // `--fake-ai`: a stand-in model runner whose models need a download, so Image › Upscale
            // with AI…'s dialog can be captured with its consent text.
            if fake_ai {
                let fake = photocraft_ai::service::FakeUpscaler::default();
                fake.download_needed.store(true, std::sync::atomic::Ordering::Relaxed);
                session.set_ai_upscaler(Some(std::sync::Arc::new(fake)));
            }
            let mut app = PhotocraftApp::new(session, services);
            app.background_jobs = background_jobs;
            app.custom_titlebar = custom_titlebar;
            // `--safe-gpu`: the CPU canvas, as the desktop app's `--safe-gpu` launch.
            if safe_gpu {
                app.perf.gpu_info.selected = "cpu".into();
                app.perf.gpu_info.canvas = "cpu".into();
                app.perf.gpu_info.fallback = Some("--safe-gpu: CPU renderer for this launch".into());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.perf.gpu_info.set_adapter(&rs.adapter.get_info());
                }
            } else if let Some(rs) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(rs.clone());
            }
            if let Some(path) = &open {
                app.open_path(path).expect("open --open file");
            }
            app
        });
    if let Some((mw, mh)) = arg(&args, "--monitor").and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))) {
        let top: f32 = arg(&args, "--window-top").and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let v = harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
        v.monitor_size = Some(egui::vec2(mw, mh));
        v.inner_rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, top), egui::vec2(w, h)));
        v.maximized = Some(false);
    }
    if let Some(appearance) = arg(&args, "--system-theme") {
        harness.input_mut().system_theme = match appearance.as_str() {
            "light" => Some(egui::Theme::Light),
            "dark" => Some(egui::Theme::Dark),
            "none" => None,
            _ => {
                eprintln!("--system-theme must be light, dark or none");
                std::process::exit(2);
            }
        };
    }
    harness.run_steps(4);
    let ctx = harness.ctx.clone();
    let timing = std::env::var_os("SNAPSHOT_TIMING").is_some();
    for (method, params) in script {
        let t0 = std::time::Instant::now();
        let label = format!("{method} {}", params.get("command").and_then(Value::as_str).unwrap_or(""));
        let (req, _rx) = ControlRequest::new(&method, params);
        let outcome = handle(harness.state_mut(), &ctx, &req);
        if let Outcome::Done(v) = &outcome
            && v.get("ok") == Some(&Value::Bool(false))
        {
            eprintln!("{method}: {v}");
        }
        // The harness doesn't call raw_input_hook: feed queued synthetic input step by step.
        loop {
            let step = harness.state_mut().take_synthetic_step();
            if step.is_empty() {
                break;
            }
            for e in step {
                harness.event(e);
            }
            harness.step();
        }
        harness.run_steps(4);
        if timing {
            eprintln!("{:>8.1} ms  {label}", t0.elapsed().as_secs_f64() * 1000.0);
        }
    }
    // Every `--click-at X,Y` / `--right-click-at X,Y` (in order, each followed by a few frames).
    let clicks: Vec<(&str, egui::PointerButton)> = args
        .windows(2)
        .filter_map(|w| match w[0].as_str() {
            "--click-at" => Some((w[1].as_str(), egui::PointerButton::Primary)),
            "--right-click-at" => Some((w[1].as_str(), egui::PointerButton::Secondary)),
            _ => None,
        })
        .collect();
    for (at, button) in clicks {
        let Some((x, y)) = at.split_once(',').and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?))) else {
            continue;
        };
        let pos = egui::pos2(x, y);
        harness.event(egui::Event::PointerMoved(pos));
        harness.step();
        harness.event(egui::Event::PointerButton { pos, button, pressed: true, modifiers: egui::Modifiers::NONE });
        harness.step();
        harness.event(egui::Event::PointerButton { pos, button, pressed: false, modifiers: egui::Modifiers::NONE });
        harness.run_steps(4);
        // `--click-wait-ms N`: keep rendering N ms after each click (background jobs finishing).
        let t_wait = std::time::Instant::now();
        while t_wait.elapsed() < std::time::Duration::from_millis(click_wait_ms) {
            harness.step();
        }
    }
    let point = |name| arg(&args, name).and_then(|s| s.split_once(',').and_then(|(x, y)| Some(egui::pos2(x.parse().ok()?, y.parse().ok()?))));
    if let (Some(from), Some(to)) = (point("--drag-from"), point("--drag-to")) {
        harness.event(egui::Event::PointerMoved(from));
        harness.step();
        harness.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        harness.step();
        for step in 1..=6 {
            harness.event(egui::Event::PointerMoved(from.lerp(to, step as f32 / 6.0)));
            harness.step();
        }
    }
    if recovery_error.is_some() {
        photocraft_ui_egui::prefs_ui::autosave_now(harness.state_mut());
    }
    let t_settle = std::time::Instant::now();
    while t_settle.elapsed() < std::time::Duration::from_millis(settle_ms) {
        harness.step();
    }
    // Let fade animations settle.
    for _ in 0..12 {
        harness.step();
    }
    let (req, _rx) = ControlRequest::new("ui.inspect", Value::Null);
    if let Outcome::Done(v) = handle(harness.state_mut(), &ctx, &req) {
        println!("perf: {}", v["result"]["perf"]["timings"]);
    }
    println!(
        "language: {}",
        serde_json::json!({"preference": harness.state().session.prefs().interface.language, "resolved": photocraft_ui_egui::i18n::current().code()})
    );
    let img = harness.render().expect("render");
    img.save(&out).expect("save png");
    println!("wrote {out} ({}×{})", img.width(), img.height());
}
