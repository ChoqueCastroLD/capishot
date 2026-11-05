use super::*;
use serde_json::Value;

fn batch(r: &mut Recorder) -> Value {
    serde_json::from_str(&r.take_batch().expect("a batch")).expect("valid JSON")
}

fn events<'a>(b: &'a Value, kind: &str) -> Vec<&'a Value> {
    b["events"].as_array().expect("events").iter().filter(|e| e["e"] == kind).collect()
}

#[test]
fn commands_are_counted_per_id_and_source_then_cleared() {
    let mut r = Recorder::new("a1", "s1", 0.0);
    assert!(r.take_batch().is_none(), "nothing to send");
    for _ in 0..3 {
        r.command("filter.blur.gaussianBlur", Source::Menu);
    }
    r.command("filter.blur.gaussianBlur", Source::Shortcut);
    r.command("edit.undo", Source::Shortcut);
    let b = batch(&mut r);
    assert_eq!(b["v"], SCHEMA_VERSION);
    assert_eq!(b["iid"], "a1");
    assert_eq!(b["sid"], "s1");
    assert_eq!(b["seq"], 1);
    let cmds = events(&b, "command");
    let n = |id: &str, src: &str| cmds.iter().find(|e| e["id"] == id && e["src"] == src).map(|e| e["n"].as_u64().unwrap_or(0));
    assert_eq!(n("filter.blur.gaussianBlur", "menu"), Some(3));
    assert_eq!(n("filter.blur.gaussianBlur", "shortcut"), Some(1));
    assert_eq!(n("edit.undo", "shortcut"), Some(1));
    assert!(r.is_empty(), "aggregates are cleared after a batch");
    assert!(r.take_batch().is_none());
    r.command("edit.undo", Source::Panel);
    assert_eq!(batch(&mut r)["seq"], 2);
}

/// An id that isn't a known command never leaves the browser as is: a workspace or action name
/// the user typed can't ride along.
#[test]
fn unknown_command_ids_are_reduced_to_their_menu() {
    assert_eq!(sanitize_command_id("filter.blur.gaussianBlur"), "filter.blur.gaussianBlur");
    assert_eq!(sanitize_command_id("window.workspace.My Secret Comic"), "window.other");
    assert_eq!(sanitize_command_id("zzz.private"), "other");
    assert_eq!(sanitize_command_id("ñ.x"), "other");
    let mut r = Recorder::new("a", "s", 0.0);
    r.command("window.workspace.My Secret Comic", Source::Menu);
    r.error("layer.rename.Chapter 12", "Layer \"Chapter 12\" is locked: invalid name");
    let text = r.take_batch().expect("batch");
    assert!(!text.contains("Secret") && !text.contains("Chapter") && !text.contains("locked"), "{text}");
}

#[test]
fn errors_carry_a_class_never_the_message() {
    assert_eq!(error_kind("Gaussian Blur is not available: no document"), "disabled");
    assert_eq!(error_kind("unsupported: CMYK 32-bit"), "unsupported");
    assert_eq!(error_kind("no document open"), "no_document");
    assert_eq!(error_kind("radius must be between 0.1 and 1000"), "invalid_params");
    assert_eq!(error_kind("PSD: unexpected end of data"), "decode");
    assert_eq!(error_kind("the user cancelled"), "cancelled");
    assert_eq!(error_kind("¯\\_(ツ)_/¯"), "other");
    // AI on the GPU (the live report: a matmul over the workgroup memory crashed the worker).
    let crash = "Manga LaMa: inference failed: inference failed: the AI worker crashed: panicked at burn-cubecl-0.22.0/src/ops/tensor.rs:150:66: called `Result::unwrap()` on an `Err` value: Unable to launch matmul because a required feature is unavailable: Shared memory budget exceeded: kernel requests 35840 bytes, hardware allows 32768 bytes per cube.";
    assert_eq!(error_kind(crash), "ai.gpu_limit");
    assert_eq!(error_kind("Manga LaMa: the GPU couldn't run the model: the AI worker crashed: RuntimeError: unreachable"), "ai.worker_crash");
    assert_eq!(error_kind("the GPU couldn't run the model: read-back failed"), "ai.gpu");
    assert_eq!(error_kind("inference failed: WebGPU device lost"), "ai.gpu_lost");
    let mut r = Recorder::new("a", "s", 0.0);
    r.error("filter.blur.gaussianBlur", "radius must be between 0.1 and 1000 (got 5000)");
    r.error("filter.blur.gaussianBlur", "radius must be between 0.1 and 1000 (got 9000)");
    let b = batch(&mut r);
    let errs = events(&b, "error");
    assert_eq!(errs.len(), 1, "same command and class aggregate");
    assert_eq!(errs[0]["kind"], "invalid_params");
    assert_eq!(errs[0]["n"], 2);
    assert!(!b.to_string().contains("5000"));
}

#[test]
fn heartbeats_count_only_visible_periods_with_interaction() {
    let mut r = Recorder::new("a", "s", 0.0);
    r.tick(30_000.0, true);
    assert!(r.is_empty(), "before a full period nothing happens");
    r.tick(60_000.0, true);
    assert!(r.is_empty(), "no interaction: idle, no heartbeat");
    r.interaction(70_000.0);
    r.tick(120_000.0, false);
    assert!(r.is_empty(), "hidden tab: not active");
    r.interaction(130_000.0);
    r.tick(180_000.0, true);
    assert_eq!(r.active_seconds(), 60);
    let b = batch(&mut r);
    let hb = events(&b, "heartbeat");
    assert_eq!(hb.len(), 1);
    assert_eq!(hb[0]["active_s"], 60);
    assert_eq!(hb[0]["total_s"], 180);
    // A timer that fires late never credits more than two periods.
    r.interaction(500_000.0);
    r.tick(900_000.0, true);
    assert_eq!(r.active_seconds(), 180);
}

#[test]
fn session_end_is_sent_once_and_closes_the_session() {
    let mut r = Recorder::new("a", "s", 1_000.0);
    r.interaction(2_000.0);
    r.tick(61_000.0, true);
    r.end(91_000.0);
    r.end(99_000.0);
    r.command("edit.undo", Source::Menu);
    r.event(json!({"e": "doc_open"}));
    let b = batch(&mut r);
    let end = events(&b, "session_end");
    assert_eq!(end.len(), 1);
    assert_eq!(end[0]["active_s"], 60, "no interaction since the last heartbeat: nothing more");
    assert_eq!(end[0]["total_s"], 90);
    assert!(events(&b, "doc_open").is_empty(), "no events after the end");
}

/// A short visit still counts the time the user was active before the first heartbeat.
#[test]
fn session_end_credits_the_minute_in_progress() {
    let mut r = Recorder::new("a", "s", 0.0);
    r.interaction(5_000.0);
    r.end(42_000.0);
    let b = batch(&mut r);
    assert_eq!(events(&b, "session_end")[0]["active_s"], 42);
    let mut r = Recorder::new("a", "s", 0.0);
    r.interaction(1.0);
    r.end(30.0 * 86_400_000.0);
    let b = batch(&mut r);
    assert_eq!(events(&b, "session_end")[0]["total_s"], MAX_SECONDS, "capped at the collector's limit");
}

#[test]
fn queues_are_bounded_and_overflow_is_counted() {
    let mut r = Recorder::new("a", "s", 0.0);
    for _ in 0..MAX_EVENTS + 7 {
        r.event(json!({"e": "doc_open", "kind": "png"}));
    }
    let b = batch(&mut r);
    assert_eq!(events(&b, "doc_open").len(), MAX_EVENTS);
    assert_eq!(b["dropped"], 7);
    let ids: Vec<String> = photocraft_engine::commands::command_specs().iter().map(|c| c.id.to_string()).collect();
    assert!(ids.len() > MAX_KEYS);
    for id in &ids {
        r.command(id, Source::Panel);
    }
    let text = r.take_batch().expect("batch");
    assert!(text.len() <= MAX_BATCH_BYTES, "{} bytes", text.len());
    let b: Value = serde_json::from_str(&text).expect("json");
    assert!(events(&b, "command").len() <= MAX_KEYS);
    assert!(b["dropped"].as_u64().unwrap_or(0) >= (ids.len() - MAX_KEYS) as u64);
}

#[test]
fn only_slow_commands_are_sampled_a_few_per_batch() {
    let mut r = Recorder::new("a", "s", 0.0);
    r.perf("filter.blur.gaussianBlur", 20.0, Some(30.0));
    r.perf("filter.blur.gaussianBlur", f64::NAN, None);
    assert!(r.is_empty());
    for _ in 0..MAX_PERF + 5 {
        r.perf("filter.blur.gaussianBlur", 812.4, Some(30.0));
    }
    let b = batch(&mut r);
    let perf = events(&b, "perf");
    assert_eq!(perf.len(), MAX_PERF);
    assert_eq!(perf[0]["ms"], 812);
    assert_eq!(perf[0]["mp"], "24-50");
    r.perf("filter.blur.gaussianBlur", 500.0, None);
    assert_eq!(events(&batch(&mut r), "perf").len(), 1, "the cap is per batch");
}

#[test]
fn buckets_and_kinds() {
    assert_eq!(file_kind("Capítulo 12/página 3.PSD"), "psd");
    assert_eq!(file_kind("a.jpeg"), "jpg");
    assert_eq!(file_kind("scan.CR3"), "raw");
    assert_eq!(file_kind("noext"), "other");
    assert_eq!(file_kind("weird.secretformat"), "other");
    assert!(is_document_kind("psd") && !is_document_kind("png"));
    assert_eq!(mp_bucket(800, 600), "<1");
    assert_eq!(mp_bucket(6000, 6000), "24-50");
    assert_eq!(mp_bucket(u32::MAX, u32::MAX), "50+");
    assert_eq!(layers_bucket(0), "0");
    assert_eq!(layers_bucket(7), "6-20");
    assert_eq!(layers_bucket(5000), "100+");
    assert_eq!(bytes_bucket(5 << 20), "1-10MB");
    assert_eq!(screen_bucket(1080, 1920), "1920-2559");
    assert_eq!(screen_bucket(0, 0), "unknown");
}

#[test]
fn user_agents_reduce_to_families() {
    let chrome_win = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
    let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.3537.57";
    let firefox_linux = "Mozilla/5.0 (X11; Linux x86_64; rv:143.0) Gecko/20100101 Firefox/143.0";
    let safari_mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
    let chrome_android = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36";
    let safari_ios = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Mobile/15E148 Safari/604.1";
    assert_eq!(browser_family(chrome_win), ("chrome", 141));
    assert_eq!(browser_family(edge), ("edge", 141));
    assert_eq!(browser_family(firefox_linux), ("firefox", 143));
    assert_eq!(browser_family(safari_mac), ("safari", 26));
    assert_eq!(browser_family(""), ("other", 0));
    assert_eq!(os_family(chrome_win), "windows");
    assert_eq!(os_family(firefox_linux), "linux");
    assert_eq!(os_family(safari_mac), "macos");
    assert_eq!(os_family(chrome_android), "android");
    assert_eq!(os_family(safari_ios), "ios");
    assert_eq!(gpu_vendor_class(0x10de, ""), "nvidia");
    assert_eq!(gpu_vendor_class(0, "ANGLE (Intel, Intel(R) UHD Graphics 620)"), "intel");
    assert_eq!(gpu_vendor_class(0, "SwiftShader"), "software");
    assert_eq!(gpu_vendor_class(0, ""), "unknown");
}

#[test]
fn session_start_holds_classes_not_the_user_agent() {
    let env = Environment {
        threads: true,
        cross_origin_isolated: true,
        cores: 9999,
        gpu: "webgpu",
        gpu_vendor: "amd",
        user_agent: "Mozilla/5.0 (Windows NT 10.0) Chrome/141.0 Safari/537.36 unique-fingerprint-123".into(),
        screen: (2560, 1440),
        language: "pt-br".into(),
        device: "tablet",
        memory_gb: 4.0,
        ..Default::default()
    };
    let e = session_start_event(&env);
    assert_eq!(e["device"], "tablet");
    assert_eq!(e["mem"], "4");
    assert_eq!(e["e"], "session_start");
    assert_eq!(e["browser"], "chrome");
    assert_eq!(e["browser_major"], 141);
    assert_eq!(e["os"], "windows");
    assert_eq!(e["cores"], 256);
    assert_eq!(e["screen"], "2560+");
    assert_eq!(e["lang"], "pt-br");
    assert!(!e.to_string().contains("fingerprint"), "{e}");
}

/// Tests that touch the process-wide recorder take turns (other tests may still run commands
/// meanwhile, so these look for their own ids).
static GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn nothing_is_recorded_until_started_and_stopping_drops_the_queue() {
    let _turn = GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop();
    assert!(with(|_| ()).is_none(), "off by default: no recorder");
    let scope = Scope::enter(false);
    scope.finish("edit.undo", &Ok::<(), String>(()), || None);
    assert!(take_batch().is_none());

    start(Recorder::new("a", "s", 0.0));
    with(|r| r.command("image.crop", Source::Menu));
    stop();
    assert!(!is_active());
    start(Recorder::new("a", "s2", 0.0));
    let text = take_batch().unwrap_or_default();
    assert!(!text.contains("image.crop"), "a stopped session's queue is gone: {text}");
    stop();
}

#[test]
fn the_app_counts_each_dispatch_once_with_its_source() {
    let _turn = GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let ctx = egui::Context::default();
    start(Recorder::new("a", "s", 0.0));
    app.run("file.new", json!({"width": 64, "height": 48, "background": "white"})).expect("new");
    // A menu item that runs engine commands underneath counts once, as a menu click.
    with_source(Source::Menu, || crate::menus::invoke(&mut app, &ctx, "filter.noise.addNoise", json!({"amount": 5, "seed": 1}))).expect("noise");
    let _ = with_source(Source::Shortcut, || crate::menus::invoke(&mut app, &ctx, "filter.blur.gaussianBlur", json!({"radius": -5})));
    observe_tool(crate::state::Tool::Move);
    observe_tool(crate::state::Tool::Brush);
    observe_tool(crate::state::Tool::Brush);
    let b: Value = serde_json::from_str(&take_batch().expect("batch")).expect("json");
    stop();
    let count =
        |id: &str, src: &str| events(&b, "command").iter().filter(|e| e["id"] == id && e["src"] == src).map(|e| e["n"].as_u64().unwrap_or(0)).sum::<u64>();
    assert_eq!(count("file.new", "panel"), 1, "{b}");
    assert_eq!(count("filter.noise.addNoise", "menu"), 1, "{b}");
    assert_eq!(count("filter.noise.addNoise", "panel"), 0, "the nested engine run isn't counted again: {b}");
    assert_eq!(count("tool.Brush", "tool"), 1, "{b}");
    assert_eq!(count("tool.Move", "tool"), 0, "the first observation is the starting tool: {b}");
    let failed = count("filter.blur.gaussianBlur", "shortcut");
    assert_eq!(failed, 1, "{b}");
    assert!(
        events(&b, "error").iter().any(|e| e["id"] == "filter.blur.gaussianBlur" && e["kind"].is_string()),
        "a failed command is counted with its class: {b}"
    );
    assert!(!b.to_string().contains("\"radius\""), "params never leave: {b}");
}

/// Preferences › General › Send anonymous usage statistics: on by default, an old settings file
/// without it keeps it on, it can be turned off, and every build that sends statistics shows it.
#[test]
fn the_preference_is_on_by_default_and_shown_where_it_works() {
    let mut s = photocraft_engine::Session::new();
    assert!(s.prefs().general.send_anonymous_usage_statistics);
    s.load_prefs_json(r#"{"general": {"beepWhenDone": true}}"#).expect("old prefs");
    assert!(s.prefs().general.send_anonymous_usage_statistics);
    s.execute("prefs.set", json!({"path": "general.sendAnonymousUsageStatistics", "value": false})).expect("set");
    assert!(!s.prefs().general.send_anonymous_usage_statistics);
    assert_eq!(
        photocraft_engine::prefs::is_hidden("general.sendAnonymousUsageStatistics"),
        cfg!(target_os = "macos"),
        "the web build and the Windows and Linux apps honour it"
    );
}

/// Commands an embedding page or control client runs (`photocraft.request("engine.execute")`,
/// `ui.menu.invoke`) count as `automation`, not as panel clicks.
#[test]
fn control_requests_count_as_automation() {
    let _turn = GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let ctx = egui::Context::default();
    start(Recorder::new("a", "s", 0.0));
    for (method, params) in [
        ("engine.execute", json!({"command": "file.new", "params": {"width": 32, "height": 32, "background": "white"}})),
        ("ui.menu.invoke", json!({"id": "layer.new.layer", "params": {"name": "x"}})),
    ] {
        let (req, _reply) = crate::control::ControlRequest::new(method, params);
        let _ = crate::control::handle(&mut app, &ctx, &req);
    }
    // A local click afterwards is a panel's again.
    app.run("edit.undo", json!({})).expect("undo");
    let b: Value = serde_json::from_str(&take_batch().expect("batch")).expect("json");
    stop();
    let src = |id: &str| events(&b, "command").iter().filter(|e| e["id"] == id).map(|e| e["src"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>();
    assert_eq!(src("file.new"), ["automation"], "{b}");
    assert_eq!(src("layer.new.layer"), ["automation"], "{b}");
    assert_eq!(src("edit.undo"), ["panel"], "{b}");
}

/// Frame times per canvas tool leave as percentiles over 1 ms buckets, with the hitches counted,
/// and nothing past the key cap.
#[test]
fn frame_times_are_summarised_per_tool() {
    let mut r = Recorder::new("a", "s", 0.0);
    for i in 0..100 {
        // 90 quick frames (8.x ms), 10 slow ones (60.x ms); latencies 20..
        let ms = if i < 90 { 8.4 } else { 60.2 };
        r.frame("tool.Brush", ms, Some(20.0 + (i % 5) as f64), Some(6.0));
    }
    r.frame(VIEW_FRAMES, 300.0, None, None);
    r.frame("tool.Move", f64::NAN, None, None);
    let b = batch(&mut r);
    let frames = events(&b, "frames");
    assert_eq!(frames.len(), 2, "{b}");
    let brush = frames.iter().find(|e| e["tool"] == "tool.Brush").expect("brush");
    assert_eq!(brush["n"], 100);
    assert_eq!(brush["p50"], 9, "upper edge of the 8 ms bucket: {brush}");
    assert_eq!(brush["p95"], 60, "the 60 ms bucket, capped at the maximum: {brush}");
    assert_eq!(brush["max"], 60);
    assert_eq!(brush["long"], 10);
    assert_eq!(brush["lat50"], 23);
    assert_eq!(brush["mp"], "4-12");
    let view = frames.iter().find(|e| e["tool"] == "view").expect("view");
    assert_eq!(view["p50"], 300, "the open last bucket reports the maximum: {view}");
    assert!(view.get("lat50").is_none() && view.get("mp").is_none());
    assert!(r.is_empty(), "cleared after the batch");

    let mut r = Recorder::new("a", "s", 0.0);
    for i in 0..(MAX_FRAME_KEYS + 5) {
        r.frame(&format!("tool.T{i}"), 5.0, None, None);
    }
    assert_eq!(events(&batch(&mut r), "frames").len(), MAX_FRAME_KEYS);
}

/// Schema 3: the device class and memory bucket are coarse classes from a fixed list.
#[test]
fn device_class_and_memory_are_buckets() {
    let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1";
    let pixel = "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 Chrome/141.0 Mobile Safari/537.36";
    let tab = "Mozilla/5.0 (Linux; Android 14; SM-X710) AppleWebKit/537.36 Chrome/141.0 Safari/537.36";
    let ipad_as_mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15";
    let windows = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/141.0 Safari/537.36";
    assert_eq!(device_class(iphone, 5, 390), "mobile");
    assert_eq!(device_class(pixel, 5, 412), "mobile");
    assert_eq!(device_class(tab, 10, 800), "tablet");
    assert_eq!(device_class(ipad_as_mac, 5, 820), "tablet", "iPadOS poses as a Mac with touch points");
    assert_eq!(device_class(ipad_as_mac, 0, 900), "desktop");
    assert_eq!(device_class(windows, 0, 1080), "desktop");
    assert_eq!(device_class(windows, 10, 1080), "desktop", "a touch laptop is a desktop");
    assert_eq!(device_class("", 0, 0), "unknown");
    for (gb, b) in [(0.0, "unknown"), (f64::NAN, "unknown"), (0.5, "<1"), (1.0, "1"), (2.0, "2"), (4.0, "4"), (8.0, "8+"), (32.0, "8+")] {
        assert_eq!(memory_bucket(gb), b, "{gb}");
    }
    let mut env = Environment { device: "phablet-x", ..Default::default() };
    assert_eq!(session_start_event(&env)["device"], "unknown", "only known classes leave");
    env.device = "mobile";
    assert_eq!(session_start_event(&env)["device"], "mobile");
}

/// Schema 3 frames: the display's frame rate and missed frames from the intervals between painted
/// frames, the gestures, the layer-count bucket and the worst frame's split.
#[test]
fn frames_carry_rate_drops_layers_and_the_worst_split() {
    let mut r = Recorder::new("a", "s", 0.0);
    let parts = |ms: f64| FrameParts { input: 1.0, logic: ms - 4.0, ui: 2.0, paint: 1.0 };
    for g in 0..3 {
        for i in 0..20 {
            // 60 Hz with a 3-period interval (two missed frames) in each gesture; the gesture's
            // first frame has no meaningful interval.
            let interval = if i == 10 { 50.0 } else { 16.7 };
            let ms = if g == 1 && i == 10 { 40.0 } else { 6.0 };
            r.frame_sample(&FrameSample {
                key: "tool.Eraser",
                ms,
                latency_ms: None,
                interval_ms: Some(if i == 0 { 900.0 } else { interval }),
                megapixels: Some(24.0),
                layers: Some(150),
                gesture_start: i == 0,
                parts: Some(parts(ms)),
            });
        }
    }
    let b = batch(&mut r);
    let e = events(&b, "frames")[0];
    assert_eq!(e["g"], 3, "{e}");
    assert_eq!(e["drop"], 6, "two missed frames per gesture: {e}");
    assert_eq!(e["layers"], "100+");
    assert_eq!(e["mp"], "24-50");
    let fps = e["fps"].as_u64().expect("fps");
    assert!((50..=58).contains(&fps), "57 intervals of 16.7 ms and 3 of 50 ms: {e}");
    assert_eq!(e["int95"], 50, "3 of 57 intervals (over 5 %) are 50 ms: {e}");
    assert_eq!(
        (e["w_logic"].as_u64(), e["w_in"].as_u64(), e["w_ui"].as_u64(), e["w_paint"].as_u64()),
        (Some(36), Some(1), Some(2), Some(1)),
        "the 40 ms frame's split: {e}"
    );
    // Frames without the schema 3 data leave none of its fields.
    let mut r = Recorder::new("a", "s", 0.0);
    r.frame("tool.Brush", 5.0, None, None);
    let b = batch(&mut r);
    let e = events(&b, "frames")[0];
    for k in ["fps", "int95", "drop", "layers", "g", "w_in", "w_logic", "w_ui", "w_paint"] {
        assert!(e.get(k).is_none(), "{k}: {e}");
    }
}

/// A 120 Hz display: once the 8.3 ms period has been seen, a 16.7 ms interval is a missed frame.
#[test]
fn missed_frames_follow_the_display_period() {
    let mut r = Recorder::new("a", "s", 0.0);
    let f = |r: &mut Recorder, i: f64| r.frame_sample(&FrameSample { key: "view", ms: 3.0, interval_ms: Some(i), ..Default::default() });
    for _ in 0..10 {
        f(&mut r, 8.3);
    }
    f(&mut r, 16.6);
    f(&mut r, 2.0);
    let b = batch(&mut r);
    let e = events(&b, "frames")[0];
    assert_eq!(e["drop"], 1, "{e}");
}

/// A freeze says what ran: the tool as it is (tools aren't command ids: every one used to read
/// `tool.other`), the part of the frame that took longest and the user's last action, all from
/// fixed lists; anything else is reduced as command ids are.
#[test]
fn freezes_name_the_tool_the_phase_and_the_last_action() {
    let mut r = Recorder::new("a", "s", 0.0);
    let ctx = FreezeContext {
        command: None,
        tool: Some(tool_id(crate::state::Tool::RemoveAi)),
        job: Some("file.open".into()),
        megapixels: Some(16.6),
        phase: Some("gpu.composite"),
        action: Some(tool_id(crate::state::Tool::Type)),
    };
    r.freeze(FreezeSource::Frame, 9_180.0, &ctx);
    r.freeze(
        FreezeSource::LongTask,
        250.0,
        &FreezeContext { tool: Some("tool.Secret".into()), action: Some("layer.rename.Chapter 12".into()), ..Default::default() },
    );
    r.freeze(FreezeSource::LongTask, 120.0, &ctx);
    let b = batch(&mut r);
    let f = events(&b, "freeze");
    assert_eq!(f.len(), 2, "only blocks of at least 200 ms");
    assert_eq!(
        (f[0]["tool"].as_str(), f[0]["phase"].as_str(), f[0]["action"].as_str(), f[0]["job"].as_str()),
        (Some("tool.RemoveAi"), Some("gpu.composite"), Some("tool.Type"), Some("file.open"))
    );
    assert_eq!((f[1]["tool"].as_str(), f[1]["action"].as_str()), (Some("tool.other"), Some("layer.other")));
    assert_eq!(b["v"], SCHEMA_VERSION);
}

/// `gpu_lost`: the context-lost (black canvas) class and the others, once each per batch.
#[test]
fn gpu_lost_events_carry_their_class() {
    let mut r = Recorder::new("a", "s", 0.0);
    r.gpu_lost("webgl", "intel", "context_lost", Some("gpu.pipelines"), 1, None);
    r.gpu_lost("webgl", "intel", "context_lost", Some("gpu.pipelines"), 2, None);
    r.gpu_lost("webgl", "intel", "slow_pipelines", None, 1, Some(4840.4));
    let body: serde_json::Value = serde_json::from_str(&r.take_batch().unwrap()).unwrap();
    let ev: Vec<&serde_json::Value> = body["events"].as_array().unwrap().iter().filter(|e| e["e"] == "gpu_lost").collect();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0]["cause"], "context_lost");
    assert_eq!(ev[0]["api"], "webgl");
    assert_eq!(ev[0]["phase"], "gpu.pipelines");
    assert_eq!(ev[1]["ms"], 4840);
}

/// Names only the user's files can contain: none of them may ever be in a batch.
const SECRETS: &[&str] = &["SecretPage", "SecretLayer", "SecretFont", "secretuser", "página"];

fn secret_document() -> photocraft_doc::Document {
    let size = photocraft_doc::Size { width: 800, height: 600 };
    let mut doc = photocraft_doc::Document::new("SecretPage", size, photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8);
    doc.layers.push(photocraft_doc::Layer::group("SecretLayer SecretFont", Vec::new()));
    doc
}

/// Everything that takes something the user named, fed with such names: a file name or a layer
/// name that could reach a batch fails here.
#[test]
fn no_file_or_layer_name_can_reach_a_batch() {
    let _guard = GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = "/home/secretuser/SecretPage página.psd";
    let doc = secret_document();
    let env = Environment {
        native: true,
        os: "linux",
        channel: "appimage",
        os_version: "/home/secretuser/SecretPage.psd".into(),
        cpu: "/home/secretuser/SecretPage.psd".into(),
        gpu_model: "C:\\Users\\secretuser\\SecretPage.psd".into(),
        gpu_driver: "/home/secretuser:SecretPage".into(),
        language: "/secretuser".into(),
        ..Default::default()
    };
    start(Recorder::new("a1", "s1", 0.0).desktop());
    with(|r| r.event(session_start_event(&env)));
    record_doc_open(path, 100, Ok(&doc), 5.0);
    let failed = format!("could not read {path}");
    record_doc_open(path, 100, Err(&failed), 5.0);
    record_doc_open_stats(path, 100, Ok(Some((800, 600, 1))), 5.0);
    record_doc_open_stats(path, 100, Ok(None), 5.0);
    record_doc_saved(path, (800, 600), 1, Some(10), Ok(()), 5.0);
    record_doc_saved(path, (800, 600), 1, None, Err(&failed), 5.0);
    record_ai_job("edit.inpaintAi", Some(&json!({"model": path, "device": path, "cpu": false, "region": [0, 0, 10, 10], "layerName": "SecretLayer"})), None, 1.0);
    let context = FreezeContext { command: Some(path.into()), tool: Some(path.into()), job: Some(path.into()), megapixels: None, phase: Some("SecretPage/x"), action: Some(path.into()) };
    with(|r| {
        r.command(path, Source::Menu);
        r.command("SecretLayer", Source::Tool);
        r.error(path, &failed);
        r.perf(path, 500.0, None);
        r.command_time(path, 500.0, Some(1.0));
        r.frame(path, 5.0, None, None);
        r.freeze(FreezeSource::Frame, 500.0, &context);
        r.gpu_lost(path, path, path, Some(path), 1, None);
        r.event(unclean_exit_event(Some(path), Some(path), Some(1), Some(1), Some(1), Some(true)));
    });
    let body = take_batch().expect("a batch");
    stop();
    for secret in SECRETS {
        assert!(!body.contains(secret), "{secret} in {body}");
    }
    assert!(body.contains("doc_open"), "the events themselves are kept: {body}");
    assert!(body.contains("\"kind\":\"psd\""), "{body}");
}

/// The last line of defence: whatever a future hook queues, an unknown field or a value that looks
/// like a path or a file name doesn't leave.
#[test]
fn an_event_with_an_unknown_field_or_a_file_name_is_dropped() {
    let mut r = Recorder::new("a1", "s1", 0.0);
    r.event(json!({"e": "doc_open", "kind": "psd", "file": "page.psd"}));
    r.event(json!({"e": "doc_open", "kind": "psd", "name": "x"}));
    r.event(json!({"e": "doc_open", "kind": "/home/me/page.psd"}));
    r.event(json!({"e": "doc_open", "kind": "page01.psd"}));
    r.event(json!({"e": "doc_open", "kind": "C:\\page"}));
    r.event(json!({"e": "doc_open", "layers": ["a"]}));
    r.event(json!({"e": "doc_open", "kind": "psd", "ok": true}));
    let b = batch(&mut r);
    assert_eq!(events(&b, "doc_open").len(), 1, "{b}");
    assert_eq!(b["dropped"], 6, "{b}");
    assert_eq!(b["v"], SCHEMA_VERSION);
    assert_eq!(b["client"], "web");
}

#[test]
fn hardware_names_never_carry_a_path() {
    assert_eq!(hardware_name("NVIDIA GeForce GTX 1050 Ti/PCIe/SSE2").as_deref(), Some("NVIDIA GeForce GTX 1050 Ti"));
    assert_eq!(hardware_name("Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz").as_deref(), Some("Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz"));
    assert_eq!(hardware_name("  llvmpipe (LLVM 15.0.7, 256 bits) ").as_deref(), Some("llvmpipe (LLVM 15.0.7, 256 bits)"));
    assert_eq!(hardware_name("/home/me/x"), None);
    assert_eq!(hardware_name("").as_deref(), None);
    assert_eq!(hardware_name("C:\\Users\\me").as_deref(), Some("C"));
    assert_eq!(hardware_name(&"x".repeat(200)).map(|n| n.len()), Some(64));
    assert_eq!(hardware_name("日本語 ÑÑ"), None);
    assert_eq!(os_version_token("Windows 11 (26100)").as_deref(), Some("Windows 11 26100"));
    assert_eq!(os_version_token("/home/me/x"), None);
    assert_eq!(os_version_token("C:\\Users\\me"), None);
    assert_eq!(os_version_from_ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"), "10.0");
    assert_eq!(os_version_from_ua("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)"), "10.15.7");
    assert_eq!(os_version_from_ua("Mozilla/5.0 (Linux; Android 14; Pixel 8)"), "14");
    assert_eq!(os_version_from_ua("Mozilla/5.0 (X11; Linux x86_64)"), "");
}

#[test]
fn a_desktop_session_start_says_what_the_collector_expects() {
    let env = Environment {
        native: true,
        os: "windows",
        channel: "msi",
        gpu: "dx12",
        gpu_vendor: "amd",
        cores: 16,
        device: "desktop",
        language: "es".into(),
        os_version: "Windows 11 (26100)".into(),
        cpu: "AMD Ryzen 7 5800X".into(),
        gpu_model: "AMD Radeon RX 6600".into(),
        gpu_driver: "31.0.24027".into(),
        ram_gb: 31.9,
        free_disk_gb: Some(120.0),
        screen: (2560, 1440),
        ..Default::default()
    };
    let e = session_start_event(&env);
    assert_eq!((&e["browser"], &e["browser_major"], &e["os"]), (&json!("native"), &json!(0), &json!("windows")));
    assert_eq!((&e["channel"], &e["ram"], &e["disk"]), (&json!("msi"), &json!("16-32"), &json!("50-200GB")));
    assert_eq!((&e["cpu"], &e["os_ver"]), (&json!("AMD Ryzen 7 5800X"), &json!("Windows 11 26100")));
    assert!(is_clean_event(&e), "{e}");
    let web = session_start_event(&Environment { channel: "web-threads", user_agent: "Mozilla/5.0 (Windows NT 10.0) Chrome/141.0".into(), ..Default::default() });
    assert_eq!((&web["browser"], &web["channel"]), (&json!("chrome"), &json!("web-threads")));
    assert!(web.get("ram").is_none() && web.get("disk").is_none(), "{web}");
}

#[test]
fn every_command_gets_percentiles_per_page_size() {
    let mut r = Recorder::new("a1", "s1", 0.0);
    for ms in [1.0, 2.0, 3.0, 4.0, 100.0] {
        r.command_time("edit.undo", ms, Some(2.0));
    }
    r.command_time("edit.undo", 7.0, Some(30.0));
    r.command_time("unknown.SecretPage", 7.0, None);
    r.command_time("edit.undo", f64::NAN, None);
    let b = batch(&mut r);
    let times = events(&b, "cmd_perf");
    let undo = times.iter().find(|e| e["id"] == "edit.undo" && e["mp"] == "1-4").expect("edit.undo");
    assert_eq!(undo["n"], 5);
    assert_eq!(undo["max"], 100);
    let (p50, p95) = (undo["p50"].as_u64().unwrap_or(0), undo["p95"].as_u64().unwrap_or(0));
    assert!((2..=4).contains(&p50), "{undo}");
    assert_eq!(p95, 100, "capped by the slowest: {undo}");
    assert!(times.iter().any(|e| e["id"] == "edit.undo" && e["mp"] == "24-50"));
    assert!(!b.to_string().contains("SecretPage"), "{b}");
    for i in 0..(MAX_COMMAND_TIMES + 3) {
        r.command_time(&format!("x{i}"), 1.0, None);
    }
    let b = batch(&mut r);
    assert_eq!(b["dropped"], 3, "bounded: {b}");
}

#[test]
fn memory_is_reported_when_a_high_water_mark_grows() {
    let mut r = Recorder::new("a1", "s1", 0.0);
    r.note_ram(1000);
    r.note_gpu(200);
    r.command("edit.undo", Source::Menu);
    let b = batch(&mut r);
    let mem = events(&b, "memory");
    assert_eq!((&mem[0]["ram_mb"], &mem[0]["gpu_mb"]), (&json!(1000), &json!(200)));
    r.note_ram(900);
    r.note_ram(1010);
    r.command("edit.undo", Source::Menu);
    assert!(events(&batch(&mut r), "memory").is_empty(), "no growth worth a line");
    r.note_ram(2000);
    r.command("edit.undo", Source::Menu);
    assert_eq!(events(&batch(&mut r), "memory")[0]["ram_mb"], 2000);
}

#[test]
fn an_ai_job_reports_model_device_and_region_only() {
    let _guard = GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    start(Recorder::new("a1", "s1", 0.0));
    let reply = json!({"model": "manga-lama", "device": "DirectML (AMD Radeon RX 6600)", "cpu": false, "fellBack": true, "lite": false, "region": [0, 0, 2000, 1000], "layer": 3});
    record_ai_job("edit.inpaintAi", Some(&reply), Some(30.0), 1234.0);
    record_ai_job("image.upscaleAi", None, Some(30.0), 10.0);
    record_ai_job("edit.undo", Some(&reply), None, 1.0);
    let body = take_batch().expect("a batch");
    stop();
    let b: Value = serde_json::from_str(&body).expect("JSON");
    let runs = events(&b, "ai_run");
    assert_eq!(runs.len(), 2, "{b}");
    assert_eq!((&runs[0]["model"], &runs[0]["device"], &runs[0]["region"], &runs[0]["fell_back"]), (&json!("manga-lama"), &json!("directml"), &json!("1-4"), &json!(true)));
    assert_eq!((&runs[1]["ok"], &runs[1]["region"]), (&json!(false), &json!("24-50")));
    assert_eq!(ai_device_class("anything", true), "cpu");
    assert_eq!(ai_device_class("WebGPU", false), "webgpu");
    assert_eq!(ai_device_class("NVIDIA", false), "other");
}

/// A name that isn't one of ours is no tool, whatever it starts with.
#[test]
fn only_real_tools_are_named() {
    let mut r = Recorder::new("a1", "s1", 0.0);
    r.command("tool.SecretLayer", Source::Tool);
    r.frame("tool.SecretLayer", 5.0, None, None);
    let b = batch(&mut r);
    assert!(!b.to_string().contains("SecretLayer"), "{b}");
    assert_eq!(events(&b, "command")[0]["id"], "tool.other");
    assert_eq!(events(&b, "frames")[0]["tool"], "tool.other");
}

/// The collector refuses a whole batch over its limit: a flood loses the excess, not the batch.
#[test]
fn a_batch_never_has_more_events_than_the_collector_takes() {
    let mut r = Recorder::new("a1", "s1", 0.0);
    for _ in 0..MAX_EVENTS {
        r.event(json!({"e": "edited"}));
    }
    for spec in photocraft_engine::commands::command_specs().iter().take(MAX_KEYS) {
        r.command(spec.id, Source::Menu);
    }
    let b = batch(&mut r);
    let sent = b["events"].as_array().map_or(0, Vec::len);
    assert!(sent <= MAX_BATCH_EVENTS, "{sent} events");
}

/// A background open waiting for a prompt has no document yet: no invented size or layer count.
#[test]
fn an_open_without_a_document_states_no_size() {
    let _guard = GLOBAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    start(Recorder::new("a1", "s1", 0.0));
    record_doc_open_stats("a.psd", 100, Ok(None), 5.0);
    let body = take_batch().expect("a batch");
    stop();
    let b: Value = serde_json::from_str(&body).expect("JSON");
    let open = events(&b, "doc_open");
    assert!(open[0].get("mp").is_none() && open[0].get("layers").is_none(), "{b}");
    assert_eq!(open[0]["ok"], true);
}

/// A dropped press leaves as a `command`, which the strict collector takes: no new event or field.
#[test]
fn tool_drops_go_out_as_commands_with_fixed_ids() {
    let mut r = Recorder::new("a", "s", 0.0);
    r.tool_drop("no_effect", "tool.Remove");
    r.tool_drop("no_effect", "tool.Remove");
    r.tool_drop("overlay", "a typed/name");
    assert!(!r.is_empty());
    let b = batch(&mut r);
    let cmds = events(&b, "command");
    let n = |id: &str| cmds.iter().find(|e| e["id"] == id && e["src"] == "tool").map(|e| e["n"].as_u64().unwrap_or(0));
    assert_eq!(n("toolDrop.no_effect.tool.Remove"), Some(2));
    assert_eq!(n("toolDrop.overlay.tool.other"), Some(1));
    assert!(r.is_empty(), "cleared after a batch");
}
