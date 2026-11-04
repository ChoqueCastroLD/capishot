//! The app's entry, native and wasm32 (Trunk builds this binary for the browser).

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Capishot host example").with_inner_size([1200.0, 800.0]),
        // The canvas draws with wgpu on the host's device.
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native("capishot-host-egui", options, Box::new(|cc| Ok(Box::new(capishot_host_egui::HostApp::new(cc)))))
}

#[cfg(target_arch = "wasm32")]
fn main() {
    capishot_host_egui::start_web();
}
