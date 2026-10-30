//! Capishot in the browser.
//!
//! Runs the same [`photocraft_ui_egui::PhotocraftApp`] as the desktop app through eframe's web
//! runner (wgpu: WebGPU where available, WebGL2 otherwise). Build with `trunk build --release`
//! from this directory; see `docs/development.md` ("Web build").
//!
//! Differences from the desktop app:
//! - no TCP control server (browsers can't listen on sockets);
//! - file pickers return browser file contents asynchronously through `Services::file_dialog`;
//!   this includes File → Open and commands such as Place Embedded;
//! - saving/exporting triggers a browser download;
//! - dropped files are read asynchronously by `web::WebShell` and delivered through the inbox
//!   (opened), or placed as layers when dropped on an open document's canvas;
//! - the system clipboard is read from the page's `paste` event (Ctrl/⌘+V) or, for Edit › Paste
//!   from a menu, `navigator.clipboard.read()`, and copies go out as PNG (`clipboard.rs`).
//!
//! URL query flags: `?cpu` forces the CPU canvas path (same as `PHOTOCRAFT_CPU_CANVAS=1`);
//! `?webgl` forces the WebGL2 backend instead of WebGPU; `?perf=1` logs frame timings per canvas
//! gesture to the console and exposes `photocraftPerf.take()` (`frame_perf.rs`).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(any(target_arch = "wasm32", test))]
mod preset_bridge;

#[cfg(target_arch = "wasm32")]
mod font_files;
#[cfg(target_arch = "wasm32")]
mod frame_perf;
#[cfg(target_arch = "wasm32")]
mod gpu_lost;
#[cfg(target_arch = "wasm32")]
mod indexed_fonts;
#[cfg(target_arch = "wasm32")]
mod indexed_presets;

#[cfg(target_arch = "wasm32")]
mod ai_bridge;
#[cfg(target_arch = "wasm32")]
mod breadcrumb;
#[cfg(target_arch = "wasm32")]
mod clipboard;
#[cfg(target_arch = "wasm32")]
mod code_split;
#[cfg(any(target_arch = "wasm32", test))]
mod clipboard_data;
#[cfg(target_arch = "wasm32")]
mod embed;
#[cfg(any(target_arch = "wasm32", test))]
mod embed_protocol;
#[cfg(any(target_arch = "wasm32", test))]
mod open_formats;
#[cfg(target_arch = "wasm32")]
mod recovery;
#[cfg(target_arch = "wasm32")]
mod save_handles;
#[cfg(target_arch = "wasm32")]
mod startup;
#[cfg(target_arch = "wasm32")]
mod usage_stats;
#[cfg(target_arch = "wasm32")]
mod watchdog;
#[cfg(target_arch = "wasm32")]
mod web;

// The versioned embedding API for host apps such as Capistudio (`embed/README.md`).
#[cfg(target_arch = "wasm32")]
mod embed_api;
#[cfg(any(target_arch = "wasm32", test))]
mod embed_api_core;

#[cfg(target_arch = "wasm32")]
mod wheel_guard;

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn main() {
    // A host page that loads the module to embed it has no app canvas: it calls `init`.
    if web::own_page() {
        web::start();
    }
}

/// The threads build (`atomics`, packaging/web-threads) starts from its page instead: it first
/// awaits `initThreadPool`, then calls the exported `start`, so nothing touches rayon before the
/// Web Worker pool exists (rayon would otherwise build its one-thread fallback pool for good).
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("photocraft-web only runs in the browser: build it with `trunk build --release` in apps/photocraft-web");
}

#[cfg(test)]
mod tests {
    /// A WebAssembly module that never downloads (or doesn't match the page) runs no Rust, so the
    /// page itself replaces the endless "Loading" with what may have gone wrong and a way to
    /// retry.
    #[test]
    fn loading_page_offers_a_reload_when_startup_stalls() {
        let html = include_str!("../index.html");
        // Trunk injects the generated loader; the page's one script of its own is boot.js, the
        // loader around it (progress, a broken load's reason, one cache-busting retry).
        assert_eq!(html.matches("<script").count(), 1, "only boot.js");
        assert!(html.contains(r#"<script src="boot.js" data-build="st"></script>"#) && html.contains(r#"rel="copy-file" href="boot.js""#));
        assert!(html.contains(r#"<p id="photocraft_status">"#), "the line boot.js writes the progress to");
        let boot = include_str!("../boot.js");
        assert!(boot.contains("photocraft_reload") && boot.contains("photocraft_load_details") && !boot.contains("setTimeout("), "no time limits");
    }

    /// The threads page starts the module's download before importing the glue (on a phone
    /// network the glue's round trip is otherwise time the module isn't downloading), and the
    /// glue's snippet imports are versioned and cached forever, so the worker pool starts from
    /// the cache instead of revalidating them per worker.
    #[test]
    fn the_threads_page_downloads_the_module_and_snippets_early() {
        let page = include_str!("../../../packaging/web-threads/index.html");
        let fetch = page.find("const wasm = wasmResponse(").expect("the module's fetch");
        let import = page.find("await import(`${dir}photocraft.js").expect("the glue's import");
        assert!(fetch < import, "the module's fetch starts first");
        let build = include_str!("../../../packaging/web-threads/build.sh");
        assert!(build.contains("?v=$snip"), "snippet imports versioned");
        let headers = include_str!("../../../packaging/web-threads/_headers");
        let versioned = headers.split_once("/v/*").expect("a /v/* rule").1;
        assert!(versioned.trim_start().lines().take(2).any(|l| l.contains("immutable")), "a build's glue, module and snippets cached forever");
        assert!(build.contains(r#"mv "$OUT/snippets" "$OUT/v/$version/""#), "snippets in the build's directory");
    }

    /// Every catalog the web build doesn't compile in is shipped next to the page by both builds
    /// (Trunk's index.html and packaging/web-threads/build.sh), where `fetch_catalogs` asks for it.
    #[test]
    fn both_web_builds_ship_the_catalogs_they_fetch() {
        let html = include_str!("../index.html");
        let threads = include_str!("../../../packaging/web-threads/build.sh");
        assert!(threads.contains(r#"cp "$ROOT"/crates/ui-egui/src/i18n/*.tsv"#), "threads build copies the catalogs");
        // build.rs compiles only these in without `all-catalogs` (the web build); this test may
        // run with every catalog compiled in (features unify across a workspace test run).
        let build_rs = include_str!("../../../crates/ui-egui/build.rs");
        assert!(build_rs.contains(r#"const ALWAYS_COMPILED_IN: [&str; 2] = ["es.tsv", "pt-br.tsv"];"#), "compiled-in catalogs changed");
        let fetched: Vec<_> = photocraft_ui_egui::i18n::LANGUAGES.iter().map(|l| l.code).filter(|c| !matches!(*c, "en" | "es" | "pt-br")).collect();
        assert!(fetched.len() >= 10, "{fetched:?}");
        for code in fetched {
            let link = format!(r#"href="../../crates/ui-egui/src/i18n/{code}.tsv" data-target-path="i18n""#);
            assert!(html.contains(&link), "index.html doesn't copy {code}.tsv");
        }
    }

    /// Both web builds split their module (packaging/web-split): the first-load part, and chunks
    /// fetched in the background. The split finds the cold functions by name, so both keep the
    /// name section for it (the split drops it).
    #[test]
    fn both_web_builds_split_the_module() {
        let html = include_str!("../index.html");
        let trunk = include_str!("../Trunk.toml");
        assert!(html.contains(r#"data-wasm-opt-params="--debuginfo""#), "Trunk's wasm-opt keeps the names");
        assert!(trunk.contains(r#"stage = "post_build""#) && trunk.contains("packaging/web-split/trunk-post-build.sh"), "Trunk runs the split");
        let threads = include_str!("../../../packaging/web-threads/build.sh");
        assert!(threads.contains("--debuginfo") && threads.contains("web-split/split.py\" --threads"), "the threads build splits");
        let hook = include_str!("../../../packaging/web-split/trunk-post-build.sh");
        assert!(hook.contains("rehash.py") && hook.contains("sri.py"), "renamed by content, integrity refreshed");
        let loader = include_str!("../../../packaging/web-split/loader.js");
        assert!(loader.contains("firstFrame()") && loader.contains("whenLoaded(onProgress)") && loader.contains("priority: 'high'"), "chunks fetched at start, in after the first frame, awaited by commands");
        let web = include_str!("web.rs");
        assert!(web.contains("code_wait: Some(crate::code_split::code_wait())") && web.contains("crate::code_split::first_frame();"), "the app waits for chunks instead of a synchronous load");
    }

    /// The threads build's page must start the Web Worker pool before the app can touch rayon
    /// (or rayon keeps its one-thread fallback pool): the app starts alongside it but is built
    /// only once the page reports the pool up (`threadPoolReady`, web.rs `WebRoot`). Its host
    /// must send the cross-origin isolation headers shared WebAssembly memory needs.
    #[test]
    fn threads_page_starts_the_worker_pool_before_the_app() {
        let html = include_str!("../../../packaging/web-threads/index.html");
        let isolated = html.find("self.crossOriginIsolated").expect("cross-origin isolation check");
        let init = html.find("await pc.default(").expect("module instantiation");
        let pool = html.find("pc.initThreadPool(").expect("worker pool start");
        let start = html.find("pc.start()").expect("app start");
        let pool_up = html.find("await pool;").expect("waits for the pool");
        let ready = html.find("pc.threadPoolReady()").expect("reports the pool up");
        assert!(
            isolated < init && init < pool && pool < start && start < pool_up && pool_up < ready,
            "isolation check, instantiate, pool, start, then pool ready"
        );
        let web = include_str!("web.rs");
        assert!(web.contains("AtomicBool::new(cfg!(not(target_feature = \"atomics\")))"), "the threads build waits for threadPoolReady");
        let headers = include_str!("../../../packaging/web-threads/_headers");
        assert!(headers.contains("Cross-Origin-Opener-Policy: same-origin"), "{headers}");
        assert!(headers.contains("Cross-Origin-Embedder-Policy: require-corp"), "{headers}");
    }
}
