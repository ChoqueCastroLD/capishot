//! The upscalers and Manga LaMa in one process: each built its own ONNX Runtime environment, and
//! the second one failed with "only one environment is allowed per process" (the first AI command
//! of a session decided which of the two then lost DirectML).

#![cfg(any(windows, target_os = "linux"))]

/// Skipped without ONNX Runtime (`PHOTOCRAFT_AI_ONNXRUNTIME` or next to the test binary).
#[test]
fn the_upscalers_and_lama_share_the_environment() {
    if let Err(e) = photocraft_ai::directml::available() {
        assert!(e.contains("not found"), "{e}");
        return eprintln!("no ONNX Runtime: skipped");
    }
    photocraft_ai_lama::directml::available().unwrap();
}
