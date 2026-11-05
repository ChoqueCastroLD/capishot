//! A theme's surface pattern must not cost a frame anything noticeable, even on a slow phone: the
//! per-frame path (finding the tile, adding its rectangle) stays in the microseconds and allocates
//! nothing once the tile exists, and a whole frame barely differs from one without a pattern.

use std::alloc::System;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use photocraft_ui_egui::theme::pattern::{self, Surface};
use photocraft_ui_egui::theme::{self, ThemeKind, Tokens};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The allocation counters are global: one measurement at a time.
static MEASURING: Mutex<()> = Mutex::new(());

#[test]
fn finding_a_tile_allocates_nothing_and_takes_microseconds() {
    let _one = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let ctx = egui::Context::default();
    theme::apply(&ctx, ThemeKind::MangaLight);
    let t = Tokens::for_kind(ThemeKind::MangaLight);
    let bar = t.surface_pattern(Surface::Bar).unwrap();
    let canvas = t.surface_pattern(Surface::Canvas).unwrap();
    pattern::tile(&ctx, &bar).unwrap();
    pattern::tile(&ctx, &canvas).unwrap();
    let calls = 100_000u32;
    let region = Region::new(GLOBAL);
    let t0 = Instant::now();
    for _ in 0..calls {
        std::hint::black_box(pattern::tile(&ctx, std::hint::black_box(&bar)));
    }
    let per_call = t0.elapsed() / calls;
    let stats = region.change();
    println!("tile lookup: {per_call:?} per call, {} allocations in {calls}", stats.allocations);
    assert_eq!(stats.allocations, 0, "a hit must not allocate");
    // Measured on a loaded server, debug build: 280 ns. The margin is for the load.
    assert!(per_call.as_micros() < 5, "{per_call:?} per lookup");
}

/// Five surfaces, as many as a whole window shows (title, options, status, tab strip, canvas).
fn frame(ctx: &egui::Context, patterned: bool) {
    let mut out = ctx.run_ui(Default::default(), |ui| {
        if patterned {
            for _ in 0..4 {
                pattern::paint_bar(ui);
            }
            let t = Tokens::get(ui.ctx());
            let tile = pattern::tile(ui.ctx(), &t.surface_pattern(Surface::Canvas).unwrap()).unwrap();
            pattern::paint(ui.painter(), ui.max_rect(), &tile);
        }
        ui.label("Bar");
    });
    out.textures_delta.clear();
    let ppp = out.pixels_per_point;
    std::hint::black_box(ctx.tessellate(out.shapes, ppp));
}

#[test]
fn a_patterned_frame_costs_next_to_nothing_more() {
    let _one = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let ctx = egui::Context::default();
    theme::apply(&ctx, ThemeKind::MangaLight);
    for _ in 0..20 {
        frame(&ctx, true);
    }
    let frames = 5_000u32;
    let measure = |patterned: bool| {
        let region = Region::new(GLOBAL);
        let t0 = Instant::now();
        for _ in 0..frames {
            frame(&ctx, patterned);
        }
        (t0.elapsed() / frames, region.change().allocations as f64 / f64::from(frames))
    };
    let (plain, plain_allocs) = measure(false);
    let (patterned, patterned_allocs) = measure(true);
    let (plain, patterned_again) = (plain.min(measure(false).0), patterned.min(measure(true).0));
    let extra = patterned_again.saturating_sub(plain);
    println!("frame: {plain:?} plain, {patterned_again:?} with the pattern (+{extra:?}); allocations {plain_allocs:.1} vs {patterned_allocs:.1}");
    // Measured on a loaded server, debug build: +6 µs. The margin is for the load; 0.1 ms is still
    // 0.6% of a 60 Hz frame.
    assert!(extra.as_micros() < 100, "a pattern adds {extra:?} per frame");
    // Measured: 24 -> 43, about 4 per surface, from the meshes tessellation starts where the
    // texture changes. One more per surface is the margin.
    assert!(patterned_allocs - plain_allocs <= 24.0, "{plain_allocs} -> {patterned_allocs} allocations per frame");
}
