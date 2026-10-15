//! Capishot's AI Web Worker (see `worker.rs`). Natively there is nothing to run.

#[cfg(target_arch = "wasm32")]
mod worker;

fn main() {
    #[cfg(target_arch = "wasm32")]
    worker::start();
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("photocraft-ai-worker runs only in a browser, as a Web Worker of Capishot's web build");
}
