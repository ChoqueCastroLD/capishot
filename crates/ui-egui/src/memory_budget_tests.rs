//! What N open documents cost in memory on a 4 GB device, on the GPU canvas and on the CPU canvas
//! (telemetry: a web tab of 3 pages of 4-12 MP on a 4 GB laptop was killed at 2.6 GB; a 4 GB
//! Celeron with 7 pages died in Remove with AI). Each document is a page of 4 to 12 MP with art,
//! a cleaning layer, styled type and a few undo steps; every one is opened, edited and drawn, then
//! all are shown again in turn. The process's resident memory is read after each step (lavapipe
//! and llvmpipe keep "GPU" textures in it, as integrated graphics do).

use serde_json::json;

use crate::PhotocraftApp;
use crate::move_repeat_tests::{Device, harness, stripes, styled_text};

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

/// Resident memory of this process (MB), from the kernel.
fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb: f64 = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0.0);
    kb / 1024.0
}

/// Frames until the active document is drawn and the canvas has nothing left to make.
fn settle(h: &mut Harness, device: Device) {
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_secs(60) {
        h.step();
        let app = h.state();
        let Some(st) = app.session.active() else { break };
        let done = match device {
            Device::Cpu => crate::cpu_canvas::settled(app),
            _ => {
                let g = app.gpu.clone().expect("the GPU canvas");
                let current = app.canvases.get(&(st.doc.id, crate::canvas::GPU_OUTPUT)).is_some_and(|c| c.on_gpu && c.revision == st.revision);
                current && !g.has_stale(st.doc.id.0) && !g.shows_before(st.doc.id.0)
            }
        };
        if done && app.session.job_on(st.doc.id).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    h.run_steps(3);
    if let Some(g) = h.state().gpu.clone() {
        g.finish();
    }
}

/// Page `i` of a chapter: 4, 8 or 12 MP, art over the page, a cleaning layer with patches, three
/// styled type layers and some undo steps.
fn page(app: &mut PhotocraftApp, i: usize) {
    let (w, h) = [(2000, 2000), (2400, 3400), (3000, 4000)][i % 3];
    app.run("file.new", json!({"width": w, "height": h, "background": "white", "name": format!("page {i}")})).expect("doc");
    stripes(app, "art", w, h, i as i32);
    app.run("layer.new.layer", json!({"name": "clean"})).expect("layer");
    for k in 0..4 {
        let (x, y) = (100 + k * 400, 200 + k * 500);
        app.run("select.rect", json!({"x": x, "y": y, "width": 380, "height": 240})).expect("select");
        app.run("edit.fill", json!({"color": "#ffffff"})).expect("fill");
    }
    app.run("select.deselect", json!({})).expect("deselect");
    for k in 0..3u32 {
        styled_text(app, 120 + k * 500, 260 + k * 600, &format!("GLOBO {k}: ¿QUÉ?"), 44);
    }
}

/// Bytes the CPU canvases of document `id` keep besides their tile textures.
fn cpu_caches(app: &PhotocraftApp, id: photocraft_doc::DocId) -> usize {
    app.canvases.iter().filter(|((d, _), _)| *d == id).filter_map(|(_, c)| c.tiles.as_ref()).map(|t| t.cache_bytes()).sum()
}

/// What the app holds for the open pages (bytes): their pixels and undo history in memory, the
/// canvases' textures and caches, the layer-effect cache and the GPU canvas.
fn held(app: &PhotocraftApp) -> u64 {
    let docs = app.session.documents();
    let canvases: u64 = docs.iter().map(|st| crate::doc_switch::canvas_bytes(app, st.doc.id) + cpu_caches(app, st.doc.id) as u64).sum();
    let gpu = app.gpu.as_ref().map_or(0, |g| g.held_bytes() + g.compositor_bytes().map_or(0, |(_, c)| c as u64));
    app.session.history_resident_bytes() as u64 + canvases + photocraft_compose::effect_cache_bytes() as u64 + gpu
}

/// What the app's caches hold (MB): per document the canvas textures and the CPU canvas's other
/// caches, then the layer-effect cache and the GPU canvas.
fn caches(app: &PhotocraftApp) -> String {
    let mb = |b: u64| b as f64 / 1048576.0;
    let mut out = String::new();
    for st in app.session.documents() {
        let id = st.doc.id;
        out += &format!(" [{} tex {:.0} cpu {:.0}]", id.0, mb(crate::doc_switch::canvas_bytes(app, id)), mb(cpu_caches(app, id) as u64));
    }
    let gpu = app.gpu.as_ref().map_or(0, |g| g.held_bytes());
    let (comp, comp_caches) = app.gpu.as_ref().and_then(|g| g.compositor_bytes()).unwrap_or_default();
    out + &format!(
        " fx {:.0} gpu {:.0} (compositor {:.0} + caches {:.0}) held {:.0}",
        mb(photocraft_compose::effect_cache_bytes() as u64),
        mb(gpu),
        mb(comp),
        mb(comp_caches as u64),
        mb(held(app))
    )
}

/// A 4 GB laptop, as the desktop shell sets it up.
fn four_gb_device() {
    crate::gpu_canvas::set_physical_memory(4 << 30);
    photocraft_engine::prefs::set_memory_cap(photocraft_engine::prefs::desktop_memory_cap(4 << 30));
}

/// Seven pages of 4 to 12 MP on a 4 GB device: each opened, edited and drawn, the 12 MP one
/// lettered (forty type edits), then all shown again twice. What the app holds stays within
/// Memory Usage (a quarter of the device's memory) and a half of it for the caches, and the
/// process's resident memory within half the device's; the pages not in front keep no CPU canvas
/// caches. Returns (the most the app held, the most the process grew), MB.
fn measure(device: Device) -> Option<(f64, f64)> {
    if std::env::var_os("NEXTEST").is_none() {
        eprintln!("skipped: reads the process's resident memory, which needs a process of its own (cargo nextest)");
        return None;
    }
    four_gb_device();
    let Some(mut h) = harness(device) else {
        eprintln!("skipping: no adapter");
        return None;
    };
    if device != Device::Cpu && h.state().gpu.is_none() {
        eprintln!("skipping: no GPU canvas");
        return None;
    }
    h.run_steps(4);
    let base = rss_mb();
    let (mut most_held, mut most_rss) = (0u64, 0f64);
    let mut check = |h: &Harness, what: &str| {
        let app = h.state();
        let r = app.session.memory_report();
        let active = app.session.active().map(|st| st.doc.id);
        let behind: usize = app.session.documents().iter().filter(|st| Some(st.doc.id) != active).map(|st| cpu_caches(app, st.doc.id)).sum();
        eprintln!(
            "MEM {device:?} {what}: rss {:.0} MB (+{:.0}) pixels {} history {} asleep {} |{}",
            rss_mb(),
            rss_mb() - base,
            r["pixelsMB"],
            r["historyMB"],
            r["asleep"],
            caches(app)
        );
        assert_eq!(behind, 0, "{what}: pages not in front keep CPU canvas caches");
        most_held = most_held.max(held(app));
        most_rss = most_rss.max(rss_mb() - base);
    };
    let n = 7;
    for i in 0..n {
        page(h.state_mut(), i);
        h.state_mut().sync_views();
        settle(&mut h, device);
        check(&h, &format!("open {i}"));
    }
    // Lettering the 12 MP page: forty edits of its type (each a new state of a styled layer).
    h.state_mut().session.set_active(5);
    h.state_mut().sync_views();
    settle(&mut h, device);
    let types: Vec<u64> = h
        .state()
        .session
        .active()
        .expect("doc")
        .doc
        .layers
        .iter()
        .filter(|l| matches!(l.content, photocraft_doc::LayerContent::Text(_)))
        .map(|l| l.id.0)
        .collect();
    for k in 0..40 {
        let id = types[k % types.len()];
        h.state_mut().run("type.edit", json!({"layer": id, "text": format!("GLOBO {k}: ¿QUÉ PASA AQUÍ?")})).expect("type.edit");
        settle(&mut h, device);
    }
    check(&h, "40 type edits");
    for round in 0..2 {
        for i in 0..n {
            h.state_mut().session.set_active(i);
            h.state_mut().sync_views();
            settle(&mut h, device);
        }
        check(&h, &format!("shown all, round {round}"));
    }
    let mb = |b: u64| b as f64 / 1048576.0;
    let allowance = h.state().session.prefs().performance.history_budget_bytes() as u64;
    eprintln!("MEM {device:?} most held {:.0} MB, most grown {most_rss:.0} MB (Memory Usage {:.0} MB)", mb(most_held), mb(allowance));
    Some((mb(most_held), most_rss))
}

#[test]
fn open_documents_stay_within_a_4_gb_devices_budget_gpu() {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some((held, grown)) = measure(Device::Gpu) else { return };
    // Memory Usage (1 GB here) for the pages and their history, half as much again for caches.
    assert!(held <= 1536.0, "held {held:.0} MB");
    assert!(grown <= 2048.0, "the process grew {grown:.0} MB");
}

#[test]
fn open_documents_stay_within_a_4_gb_devices_budget_cpu() {
    let Some((held, grown)) = measure(Device::Cpu) else { return };
    assert!(held <= 1536.0, "held {held:.0} MB");
    assert!(grown <= 2048.0, "the process grew {grown:.0} MB");
}

/// On the CPU canvas, a page that goes behind another lets go of its splits and reduced layers at
/// the next frame and keeps its tile textures; shown again, it is on screen in its first frame
/// and makes its caches again. Before, each page kept its own (77 MB for a 4 MP page here).
#[test]
fn a_page_behind_another_keeps_no_cpu_canvas_caches() {
    let Some(mut h) = harness(Device::Cpu) else { return };
    for i in 0..3 {
        page(h.state_mut(), i);
        h.state_mut().sync_views();
        settle(&mut h, Device::Cpu);
    }
    let ids: Vec<photocraft_doc::DocId> = h.state().session.documents().iter().map(|st| st.doc.id).collect();
    let active = h.state().session.active().expect("doc").doc.id;
    assert!(cpu_caches(h.state(), active) > 0, "the page in front keeps what the next edit needs");
    for id in ids.iter().filter(|d| **d != active) {
        assert_eq!(cpu_caches(h.state(), *id), 0, "page {} behind keeps CPU canvas caches", id.0);
        assert!(crate::doc_switch::canvas_bytes(h.state(), *id) > 0, "page {} behind keeps its tiles", id.0);
    }
    h.state_mut().session.set_active(0);
    h.state_mut().sync_views();
    h.step();
    assert!(crate::cpu_canvas::shows_document(h.state()), "shown again, it is on screen at once");
    settle(&mut h, Device::Cpu);
    assert!(cpu_caches(h.state(), ids[0]) > 0, "and makes its caches again");
    assert_eq!(cpu_caches(h.state(), active), 0, "while the page it left lets go of its own");
}
