//! The usage statistics must not cost a frame anything: the whole per-frame path stays under
//! 0.1 ms and allocates nothing once its keys exist.

use std::alloc::System;
use std::time::Instant;

use photocraft_ui_egui::telemetry::{self, FrameParts, FrameProbe, FrameSample, Recorder};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn frame(app: &photocraft_ui_egui::PhotocraftApp, ctx: &egui::Context, probe: &mut FrameProbe, cpu: f32) {
    probe.begin(Some(cpu));
    probe.end_logic();
    probe.end_update(app, ctx);
    telemetry::observe_frame(app, ctx);
    let sample = FrameSample {
        key: "tool.Brush",
        ms: 7.0,
        latency_ms: Some(9.0),
        interval_ms: Some(16.6),
        megapixels: Some(12.0),
        layers: Some(30),
        gesture_start: false,
        parts: Some(FrameParts { input: 1.0, logic: 2.0, ui: 2.0, paint: 2.0 }),
    };
    telemetry::with(|r| r.frame_sample(&sample));
}

#[test]
fn a_frame_costs_under_a_tenth_of_a_millisecond_and_allocates_nothing() {
    let app = photocraft_ui_egui::PhotocraftApp::new(photocraft_engine::Session::new(), photocraft_ui_egui::Services::default());
    let ctx = egui::Context::default();
    let mut probe = FrameProbe::default();
    telemetry::start(Recorder::new("a1", "s1", 0.0).desktop());
    // The first frames make the keys and histograms.
    for i in 0..10 {
        frame(&app, &ctx, &mut probe, i as f32 * 0.001);
    }
    let region = Region::new(GLOBAL);
    let frames = 20_000u32;
    let t0 = Instant::now();
    for i in 0..frames {
        frame(&app, &ctx, &mut probe, (i % 1000) as f32 * 0.001);
    }
    let per_frame_ms = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);
    let stats = region.change();
    println!("{per_frame_ms:.5} ms per frame, {} allocations in {frames} frames", stats.allocations);
    assert!(per_frame_ms < 0.1, "{per_frame_ms} ms per frame");
    assert_eq!(stats.allocations + stats.reallocations, 0, "a frame must not allocate: {stats:?}");
    // Building a batch happens off the frame; it still has to work.
    assert!(telemetry::take_batch().is_some_and(|b| b.contains("frames")));
    telemetry::stop();
}
