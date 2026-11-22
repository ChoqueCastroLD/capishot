//! The compositor's spans reach the frame's spans in the app. The app makes its canvas with a
//! tile side (`GpuCanvas::with_tile`), and the hook that hands the compositor's spans
//! (`gpu.plan`, `gpu.record`, `gpu.upload`, `gpu.submit`...) to `perf_span` was installed by
//! `GpuCanvas::new` alone: on the owner's PC every slow frame said `gpu.composite` and nothing
//! under it (15 to 400 ms of unnamed time), and so did the usage statistics.

use super::*;
use photocraft_doc::{Document, Size};

#[test]
fn a_canvas_made_with_a_tile_side_names_the_compositors_spans() {
    let _gpu = photocraft_testkit::gpu_lock();
    let rs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| egui_kittest::wgpu::create_render_state(wgpu_setup(), Default::default())));
    let Ok(rs) = rs else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    // As `PhotocraftApp` makes it (`app/gpu.rs`).
    let gpu = GpuCanvas::with_tile(&rs, Some(4096));
    let doc = {
        use photocraft_color::{Color, ColorMode, SampleType};
        Document::with_background("page", Size::new(256, 256), ColorMode::Rgb, SampleType::U8, Color::WHITE)
    };
    // The compositor may still be on its way (made on a worker, or a pipeline a frame on GL): the
    // first composite that draws is the one looked at.
    let t0 = now_ms();
    loop {
        crate::perf_span::begin_frame();
        match gpu.composite(&doc, doc.bounds(), false) {
            Ok(_) => break,
            Err(e) => assert_eq!(e.0, WARMING_UP, "the canvas refused the page"),
        }
        while gpu.warm_up() {
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert!(now_ms() - t0 < 300_000.0, "the compositor never arrived");
        }
        assert!(now_ms() - t0 < 300_000.0, "the compositor never arrived");
    }
    let spans = crate::perf_span::frame();
    for name in ["gpu.plan", "gpu.record", "gpu.submit"] {
        assert!(spans.iter().any(|(n, _)| *n == name), "no `{name}` span in the frame: {spans:?}");
    }
}
