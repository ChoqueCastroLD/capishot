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

The module ships as `photocraft_bg.wasm.gz` (about 6 MB instead of about 13 MB) and the page inflates
it with `DecompressionStream` while it compiles, so no server-side compression or wasm MIME type is
needed for it. It holds what the first frames and the basic tools run; the rest of the code is in
`photocraft_bg.c<N>.wasm` (about 5.5 MB each, plain wasm), which the glue fetches and instantiates in
the background once the app runs, or on the spot when something calls into one first
(`packaging/web-split`; `PHOTOCRAFT_WEB_SPLIT=0` builds one module). `build.sh` fails if any file
passes 32 MiB. The page checks the first bytes
for the gzip header, so a host that serves the file with `Content-Encoding: gzip` (and so has the
browser inflate it first) works too.

The site works from any path (all URLs are relative). As with the single-threaded web build, everything runs
on the visitor's device: Open uses the file picker, Save/Export download files, preferences use
localStorage, and there is no server-side storage.

Browser support: current Chrome, Edge, Firefox and Safari (Wasm threads, SIMD and cross-origin
isolation). WebGPU is used where available, WebGL2 otherwise; `?webgl` and `?cpu` work as in the single-threaded build.

## Building both for a deploy (`packaging/web-release.sh`)

```sh
packaging/web-release.sh             # -> dist/web (st) + dist/web-threads, both with their AI workers
packaging/web-release.sh --preview   # quick test build: thin LTO (not for a release, see below)
```

The same output as Trunk, `build.sh` (with `PHOTOCRAFT_WEB_FALLBACK=st/`) and
`packaging/web-ai/build.sh` for each site, one after the other, but faster:

- **Concurrent.** The single-threaded build, the threads build and the AI workers build at the
  same time. Each ends in a fat-LTO link and a `wasm-opt` run on one core, so in sequence the
  machine mostly idles. `PHOTOCRAFT_WEB_JOBS` (default 10) is the total of cargo jobs, split
  between them.
- **AI workers cached.** They are built once (not once per site) and kept in
  `<target>/web-release/ai/<key>`, keyed by the content of everything they are built from: their
  workspace crates (from cargo's resolve), `Cargo.lock`, `Cargo.toml`, `.cargo/`,
  `packaging/web-ai/build.sh`, the toolchains and `wasm-bindgen`/`wasm-opt` versions. A hit skips
  their two fat-LTO links and `wasm-opt -O3` runs. `PHOTOCRAFT_AI_CACHE=0` rebuilds them anyway.
- **`wasm-opt` cached** (when it is on `PATH`): the same input module and arguments reuse the
  earlier output (`<target>/web-release/wasm-opt`, 3 days), so a rebuild whose Rust didn't change
  skips its `-Oz` passes.
- **Fails fast.** The first stage that fails stops the others and the script exits non-zero
  with that stage's log tail: `dist/` is then incomplete, never deploy it. Each stage's full log
  is in `<target>/web-release/logs/`, and its time is printed.

- **Memory-aware.** Each site's fat-LTO link peaks near 10 GB. With less than
  `PHOTOCRAFT_WEB_MEM_GB` (default 24) available the two sites build one after the other instead
  (swapping costs far more than the overlap saves).

Measured on a 14-core server shared with other builds (`PHOTOCRAFT_WEB_JOBS=6`), against the
four commands in sequence: no change 285 s → 38 s; a one-line `ui-egui` change 1068 s → 459 s;
a one-line `crates/ai` change 1194 s → 442 s. The fat-LTO link of the slower module (one core,
about 7 minutes) is the floor for any Rust change.

`--preview` builds both sites with thin LTO and 16 codegen units: a one-line change relinks in
about 1.5 minutes instead of 6.5, but the module is about 10% bigger (gzip +9%; the split's
first-load part 14.6 → 18.2 MB, as the cold-function lists were profiled on fat-LTO builds) and
`wasm-opt` takes twice as long, so it stays out of releases (fat LTO keeps the module small). Switching
between the two recompiles every crate once.

It is also slower at run time, so releases stay on fat LTO (measured 2026-11-03, the same commit
both ways, 3 interleaved runs each, medians): a one-line `ui-egui` change rebuilds both sites in
207 s instead of 431 s, and the served first-load module grows 5.26 → 5.98 MB Brotli (+13.5%;
gzip +20%), but on the phone tour (`e2e/no-freeze.mjs`, CPU 4x) the time to interactive went
1514 → 1798 ms (+19%), opening a JPEG +11%, a PSD +8.5%, a PNG export +10-14%; on the desktop
frame times went up 5-8% at the median (brush, eraser, marquee) and filters 7-12% (Add Noise,
Image Size, Flatten). Preview deploys (`deploy-preview.sh` outside the repository, served at
`/capishot/preview/`) use it: they carry the PREVIEW badge and send no usage statistics.

## Release checklist

Before building a release of either web build ([`docs/releasing.md`](../../docs/releasing.md)):

1. Refresh the module split's profile if the web app changed since the last one, and commit what
   it writes (`packaging/web-split/cold-functions*.txt.gz`, `hot-commands*.txt`):
   `CHROME=<chromium> CAPISHOT_PROFILE_PAGE=<a page image> packaging/web-split/profile.sh st`, then
   the same with `threads`.
2. Build both from the same commit (Trunk for `st/`, then `build.sh`) and run the checks below,
   `e2e/load-failures.mjs` included.

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
- `e2e/load-failures.mjs`: a bad network on a phone: every module file failing to download
  (the reload message after one retry), chunks arriving late (a command waits for its chunk, no
  synchronous load on the page) or never (the command fails cleanly with a Retry notice), and a
  failed last-resort synchronous load (`?split=sync`: the toast). Works on either build.
- `e2e/hidden-tab-boot.mjs`: the app opened in a background tab (never shown) boots, `window.photocraft`
  appears and answers commands. Raw DevTools protocol (Playwright keeps its pages visible), so it needs
  only Node and a Chromium: `CHROME=<chromium> node e2e/hidden-tab-boot.mjs <url>`.
- `e2e/save-to-device.mjs`: where the browser can save to a picked file, Save As asks once and
  File › Save writes that file again in place (no download); elsewhere saves download.
- `e2e/embed-hidden-batch.mjs`: a host page prepares pages in hidden documents (`openDocument`/
  `newDocument` `{hidden}`, `batch`, `layer.putPixels {src}` from base64, a Blob, an ArrayBuffer and
  a URL, `layer.setPropsMany`) and copies a layer out with `layer.copyToDocument {fromDocument,
  toDocument}`, while every screenshot of the shown page must stay identical (no blank, partial or
  flickering frame) and no switch or view event goes out; prints each call's time and the longest
  animation frame. Serves the build itself: `CHROME=<chromium> node e2e/embed-hidden-batch.mjs <dist dir>`.
- `e2e/embed-doc-switch.mjs`: one visible document (`editor.setLayout {single}`) switched between
  views (`document.views.apply`) and pages (`prewarmDocument`, `showDocument {viewport: keep}`, a
  cold page, a page whose textures `editor.setMemoryBudget` evicted); every animation frame the page
  reads the canvas back and counts blank or partial frames, and times the call to the final frame.
  `CHROME=<chromium> node e2e/embed-doc-switch.mjs <dist dir>`.
