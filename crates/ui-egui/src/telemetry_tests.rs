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
    };
    let e = session_start_event(&env);
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
/// without it keeps it on, it can be turned off, and only the web build shows it.
#[test]
fn the_preference_is_on_by_default_and_web_only() {
    let mut s = photocraft_engine::Session::new();
    assert!(s.prefs().general.send_anonymous_usage_statistics);
    s.load_prefs_json(r#"{"general": {"beepWhenDone": true}}"#).expect("old prefs");
    assert!(s.prefs().general.send_anonymous_usage_statistics);
    s.execute("prefs.set", json!({"path": "general.sendAnonymousUsageStatistics", "value": false})).expect("set");
    assert!(!s.prefs().general.send_anonymous_usage_statistics);
    assert_eq!(
        photocraft_engine::prefs::is_hidden("general.sendAnonymousUsageStatistics"),
        !cfg!(target_arch = "wasm32"),
        "shown only where it does something"
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
