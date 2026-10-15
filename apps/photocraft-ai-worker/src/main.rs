//! Capishot's AI Web Worker (see `worker.rs`). Natively there is nothing to run.

#[cfg(target_arch = "wasm32")]
mod worker;

fn main() {
    // The threads variant starts from its bootstrap (`startAi`) instead: `main` also runs in
    // every rayon pool worker that instantiates the module.
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    worker::start();
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("photocraft-ai-worker runs only in a browser, as a Web Worker of Capishot's web build");
}
