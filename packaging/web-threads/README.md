# Capishot for the web, multithreaded

A second browser build next to the single-threaded one (`apps/photocraft-web` + Trunk,
`packaging/web/`). It is the same app, compiled with WebAssembly
threads: one shared memory, and the rayon work (filters, compositing, colour transforms, resampling,
selections, RAW and HEIC decoding) spread over a pool of Web Workers instead of running on the page's
single thread.

```sh
packaging/web-threads/build.sh            # -> dist/web-threads
packaging/web-threads/build.sh --serve    # then http://127.0.0.1:8777 with the right headers
```

Needs a nightly toolchain with `rust-src` and the `wasm32-unknown-unknown` target (std is rebuilt
with atomics), and `wasm-bindgen-cli` at the version in `Cargo.lock`. `wasm-opt` is taken from
`PATH`, or a pinned binaryen release is downloaded (sha256-checked) into `target/tools` on first use.
The build uses its own target dir (`target/web-threads`), so it doesn't invalidate other builds.

## What is different from the single-threaded web build

| | `packaging/web` | `packaging/web-threads` |
|---|---|---|
| Toolchain | stable, Trunk | nightly, `-Z build-std` (std with `atomics`) |
| Target features | default | `+atomics,+bulk-memory,+simd128` |
| Memory | private, grows to 4 GiB | shared, imported, max 4 GiB |
| Parallel code | sequential on wasm | rayon on a Web Worker pool (`wasm-bindgen-rayon`) |
| Hosting | any static host | static host that sends COOP + COEP (cross-origin isolation) |

How it fits together:

- **Gates.** Each parallel step is written as a rayon branch for native and a sequential branch
  for `wasm32`. Those gates now read `any(not(target_arch = "wasm32"), target_feature = "atomics")`
  and `all(target_arch = "wasm32", not(target_feature = "atomics"))`, so the threads build takes the
  rayon branch and the single-threaded wasm build is unchanged. The crates that only pulled in
  rayon on native also pull it in for wasm with `atomics`; HEIC decoding turns on heic-rs `parallel`
  there too. Code that spawns `std::thread`s (background jobs, autosave, a few `thread::scope`
  splits) stays sequential: `std::thread::spawn` is unsupported on `wasm32-unknown-unknown`.
- **The main thread never blocks.** The egui UI runs on the browser's main thread, which may not
  block (`Atomics.wait` traps there). rayon's `web_spin_lock` feature makes the main thread spin
  instead while it waits for the workers to finish a parallel step.
- **GPU objects stay on the main thread.** wgpu's objects are not `Send`/`Sync` with atomics, but
  egui-wgpu's `callback_resources` map requires both, so the GPU canvas keeps them in a
  `send_wrapper::SendWrapper` (runtime-checked: only the main thread may touch them).
- **Start-up order.** `index.html` checks `crossOriginIsolated`, instantiates the module, awaits
  `initThreadPool(n)` and only then calls `start()`. Started any earlier, rayon would build its
  one-thread fallback pool and the worker pool could no longer be installed.
  `n` is one worker per core minus one (the UI thread), at most 16; `?threads=N` overrides it.

## Hosting

Upload the contents of `dist/web-threads/` to any static host that can send these headers on
**every** response (the page, the scripts and the wasm):

```text
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Without them the page is not cross-origin isolated (browsers only allow shared WebAssembly memory
on isolated pages): it reports that and stops, or, when built with a fallback, loads the
single-threaded build instead. Host both builds side by side for that:

```sh
(cd apps/photocraft-web && trunk build --release)              # -> dist/web
PHOTOCRAFT_WEB_FALLBACK=st/ packaging/web-threads/build.sh    # -> dist/web-threads
cp -R dist/web dist/web-threads/st
```

`_headers` sets them for Cloudflare Workers static
assets and Cloudflare/Netlify Pages, along with caching: `photocraft.js` and `photocraft_bg.wasm.gz`
are requested as `?v=<content hash>` and cached for a year; everything else revalidates.

The module ships as `photocraft_bg.wasm.gz` (about 10 MB instead of about 25 MB) and the page inflates
it with `DecompressionStream` while it compiles, so no server-side compression or wasm MIME type is
needed, and it stays far below the 25 MiB per-file cap of Cloudflare Workers/Pages (the raw module
is already close to it; `build.sh` fails if any file passes 24 MiB). The page checks the first bytes
for the gzip header, so a host that serves the file with `Content-Encoding: gzip` (and so has the
browser inflate it first) works too.

The site works from any path (all URLs are relative). As with the single-threaded web build, everything runs
on the visitor's device: Open uses the file picker, Save/Export download files, preferences use
localStorage, and there is no server-side storage.

Browser support: current Chrome, Edge, Firefox and Safari (Wasm threads, SIMD and cross-origin
isolation). WebGPU is used where available, WebGL2 otherwise; `?webgl` and `?cpu` work as in the single-threaded build.

## Browser checks (`e2e/`)

Playwright scripts run against a build served by `serve.py` (they need `playwright-core` and a
Chromium in `CHROME`):

- `e2e/memory-flat.mjs`: a type-heavy phone session (360 × 780, 4 GB, CPU 4×) on two real pages
  (`CAPISHOT_MEMORY_PAGES`, not committed) must keep the WebAssembly memory flat. Serve a copy of
  the build whose `index.html` sets `self.__wasmExports` to what `pc.default(…)` returns.
- `e2e/recovery-crash.mjs`: a page killed in the middle of an autosave must still recover its last
  good checkpoint.
- `e2e/no-freeze.mjs`: the phone tour, a type session, saving and exporting (and, when the pages
  folder `CAPISHOT_FREEZE_PAGES` holds a `page.psd`, opening and saving a 12-24 MP layered page)
  with no long task over 500 ms. It prints each step's longest task and the app's slow frames
  with the part of the frame that took longest. `CAPISHOT_E2E_WEBGPU=1` runs the canvas on WebGPU
  (SwiftShader) instead of the CPU canvas headless Chrome falls back to.
- `e2e/save-to-device.mjs`: where the browser can save to a picked file, Save As asks once and
  File › Save writes that file again in place (no download); elsewhere saves download.
