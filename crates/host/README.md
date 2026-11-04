# capishot-host

Capishot's editor as a library, in the host app's own process, native and
`wasm32-unknown-unknown` with one API:

- **`Host`**: the engine. Open documents from bytes, run any control-protocol command by id,
  read and write pixels, group undo steps, run long commands as jobs, get events, keep a document
  in step between collaborators.
- **`view::CapishotView`** (feature `view`): the editor as an egui widget, drawn in the host's
  own eframe/egui context on its wgpu device, in two modes on the same open documents:
  `Mode::Embedded`, the canvas alone, and `Mode::Full` (feature `full`), Capishot's whole
  interface (menus, panels, dialogs, every tool). It changes no global egui style or fonts.
- **`files`, `fonts`, `render`, `ai`**: options and results for open/save from bytes, the
  host's fonts, rendering a region to RGBA, and in-process AI. The calls are `Host` methods.

Window-level actions and file pickers go to the host as events: the window stays the host's.
`examples/host-egui` is the contract: a separate Cargo workspace that depends on this crate as
an outside host does.

## Depending on it

Pin a commit (never a branch): the API's stability promise is per commit, and `CHANGELOG.md`
lists the changes between them.

```toml
[dependencies]
capishot-host = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<commit>", default-features = false, features = ["view"] }

# Per target, only if you run AI in process (see Features):
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
capishot-host = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<commit>", default-features = false, features = ["view", "ai-native"] }
[target.'cfg(target_arch = "wasm32")'.dependencies]
capishot-host = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<commit>", default-features = false, features = ["view", "ai-web"] }
```

### The `[patch]` block every host copies

Capishot builds on reviewed forks: egui 0.37 at `storytold/egui@6d518fc`, winit at
`storytold/winit@a517b38`, and `egui-wgpu` 0.37 with 0.36's renderer, vendored in this
repository (`vendor/egui-wgpu`). Cargo applies `[patch]` only from the root manifest, so your
workspace root repeats it. Your own egui code then runs on the same fork (egui 0.37's API).
Upstream egui 0.37 is not an option: the canvas uses fork APIs (`Context::run_ui`,
`global_style`, the IME and screenshot hooks), and its renderer needs the vendored `egui-wgpu`.

```toml
[patch.crates-io]
ecolor = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
emath = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
epaint = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
epaint_default_fonts = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui-winit = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui_glow = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui_extras = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui_kittest = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui_system_font_provider = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
eframe = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui_demo_lib = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
egui_inspection = { git = "https://github.com/storytold/egui.git", rev = "6d518fc799681571963842f95b55a6b68431b15a" }
winit = { git = "https://github.com/storytold/winit.git", rev = "a517b3846ee8457d84ee4975110bfda6797c44dc" }

[patch."https://github.com/storytold/egui.git"]
egui-wgpu = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<the same commit>" }
```

These blocks are the root `Cargo.toml`'s at that commit; when you move the pin, copy them again
(they change together, and `CHANGELOG.md` says when). Cargo warns that some patches are unused
(`egui_demo_lib`, `egui_inspection`…): harmless.

### Features

| Feature | Default | What it adds |
|---|---|---|
| `view` | yes | `view::CapishotView`, the canvas widget (pulls `photocraft-ui-egui` without its plug-ins or extra catalogs) |
| `full` | yes | `view::Mode::Full`, Capishot's whole interface in the widget |
| `camera-raw` | no | Camera Raw development. Without it a raw file opens as its embedded preview, with a warning |
| `exr`, `heif` | no | OpenEXR and HEIC/HEIF files. Without them they are recognised and refused with a clear error |
| `all-catalogs` | no | Every translation catalog in the binary (default: English, Spanish, Brazilian Portuguese) |
| `plugins` | no | Sandboxed WebAssembly filter plug-ins (`plugin.*` commands; the wasmi interpreter) |
| `ai-native` | no | In-process AI natively: burn on wgpu/Vulkan/Metal/DX12, DirectML on Windows, CPU fallback. **Native targets only** |
| `ai-web` | no | In-process AI in the browser through the AI Web Worker the host serves (`packaging/web-ai/build.sh`). Empty natively |

A host build: `default-features = false, features = ["view", "full"]` (or only `view` for the
canvas alone), plus `ai-native`/`ai-web` per target when it runs Remove with AI or the upscales in
process. Sizes and what to trim: [Binary size](#binary-size).

## Using it

```rust
use capishot_host::{Host, OpenOptions, ExecOptions, InspectOptions, SaveOptions};
use serde_json::json;

let mut host = Host::new();
let page = host.open(psd_bytes, OpenOptions { name: Some("012.psd".into()), hidden: true })?.document;
let tree = host.inspect(page, InspectOptions { text_style: true })?;      // document.inspect
host.execute_with("layer.setProps", json!({"layer": 3, "visible": false}), ExecOptions::on(page))?;
host.group(Some(page), |h| {                                               // one undo step
    h.execute("layer.new.layer", json!({"name": "ES"}))?;
    h.execute("type.create", json!({"text": "¡Hola!", "x": 120, "y": 80}))
})?;
host.undo(page)?;
let saved = host.save(page, &SaveOptions::default())?;                     // incremental PSD
```

With the widget (one call per frame; it polls the host, so don't also call `Host::poll`):

```rust
use capishot_host::view::CapishotView;

// In eframe's app creator: the canvas on eframe's wgpu device.
CapishotView::install(&cc.egui_ctx);
let mut view = CapishotView::new(cc.wgpu_render_state.clone());

// Each frame, in the rect the host gives it:
let out = view.ui(ui, &mut host);
if let Some(overlay) = out.overlay() {
    overlay.doc_rect_stroke([10.0, 10.0, 200.0, 80.0], (2.0, egui::Color32::YELLOW)); // document coordinates
}
for event in &out.events { /* layerPixelsChanged, toolChanged, jobFinished… */ }
```

### The two modes

```rust
use capishot_host::view::{CapishotEditor, Keys, Mode, Shortcut, ViewEvent, WindowRequest};

let mut editor = CapishotEditor::new(cc.wgpu_render_state.clone());   // one object: `editor.host` + the view
editor.view.options_mut().keys = Keys::deny(Shortcut::opacity_digits()); // the host keeps 0-9

// Each frame, the one call: no forwarding, no sync calls.
let out = editor.ui_in_rect(ui, rect);
for e in &out.view_events {
    match e {
        ViewEvent::Window(WindowRequest::Title(t)) => { /* the host's window, the host's choice */ }
        ViewEvent::Window(WindowRequest::Close) => editor.set_mode(Mode::Embedded), // File › Exit
        ViewEvent::FileDialog(d) => { /* its own picker, then d.contents(..) / d.save_to(..) / d.cancel() */ }
        ViewEvent::WriteFile { name, bytes } => { /* store the saved file */ }
        ViewEvent::DocumentReady { document, first_frame_ms, final_frame_ms } => { /* fully drawn */ }
        _ => {}
    }
}
editor.set_mode(Mode::Full); // any frame: same documents, nothing reopened
```

- **Style.** Capishot's style is its own drawing's for the length of the call (the context's styles
  are swapped and put back, also on a panic); the host's widgets before and after it, its theme
  preference and its fonts stay as they are. Capishot's text uses the host's fonts.
  `ViewOptions::full_theme`: `FullTheme::Host` (default) builds Capishot's interface from the
  host's colours, `FullTheme::Capishot` keeps Capishot's own theme.
- **Window.** The window close prompt and the fit-to-display step don't run. File › Exit, the
  window title, Screen Mode, maximize, focus and "zoom resizes window" arrive as
  `ViewEvent::Window(WindowRequest::…)`. `WindowRequest::Close` asks nothing about unsaved work:
  `Host::documents()` says what is dirty.
- **Files.** Decoding and encoding are built in; a picker is `ViewEvent::FileDialog` (the host
  answers later, with contents natively and in the browser), a save is `ViewEvent::WriteFile`, a
  link is `ViewEvent::OpenUrl`.
- **Keys.** `ViewOptions::keys` in both modes: `Keys::deny([...])`, `Keys::allow_only([...])` or
  `Keys::filter(|event| ...)`; `.routing(KeyRouting::Never | WhenActive | Always)` says when. In full
  mode `WhenActive` follows the last press: inside the rect (or on Capishot's windows) the keyboard
  is Capishot's until a press on the host's UI. Withheld keys stay in egui's input for the host's
  widgets drawn after the editor; draw those after it. Keep `options.zoom_with_keyboard = false`
  if ⌘+/⌘- should zoom only the canvas (egui handles those before any widget, and it is the
  host's option to set).
- **Tools, rulers, zoom**: `Host::set_tool`, `ViewOptions::rulers`, `CapishotView::set_camera`
  (`Fit`, `Fill`, `Set`, `ZoomBy`).
- **Optional:** forward eframe's `raw_input_hook` to `CapishotView::raw_input_hook` for Windows and
  Wayland pinch zoom and finger gestures (they must run before egui reads the input). Nothing
  else needs it.
- **Not carried over in `Mode::Full`:** Capishot's preferences file, crash recovery and auto-update
  (`Services` left empty: preferences last the session). `CapishotView::with_services` can set them
  (unstable).

### Raw pixels, hidden documents

`Host::open(bytes, OpenOptions { hidden: true, .. })` opens a hidden document in the host's own
session (`start_open` does it on a worker), so nothing has to be built elsewhere and moved.
`Host::open_rgba(&RgbaImage { .. }, OpenOptions { hidden, .. })` makes a document from raw rows.
`Host::put_rgba(doc, layer, &RgbaImage { width, height, stride, data }, PutRgba { .. })` and
`Host::add_rgba(doc, &image, AddRgba { .. })` take raw 8-bit straight RGBA rows (sRGB, any stride),
with no PNG or base64.

### Reference

- **Commands**: `execute(id, params)` / `execute_with(id, params, ExecOptions { document, history })`
  take the ids and params of [`docs/control-protocol.md`](../../docs/control-protocol.md) ›
  Engine commands. `execute_request(&json)` takes an `engine.execute` request as is.
  `History::None` is the protocol's `history: "none"`: no undo step, kept through the person's
  Undo/Redo.
- **Jobs**: `start(id, params, document)` → `Started::Job(handle)` for long commands (filters,
  Content-Aware Fill, Remove with AI…); `job(handle)` (progress), `cancel(handle)`,
  `Event::JobFinished`. `wait(handle)` blocks and exists natively only. `start_open` and
  `start_save` / `take_saved` decode and encode in the background.
- **Files**: `open(bytes, OpenOptions) -> Opened {document, warnings, damaged, missing_fonts}`,
  `save(doc, &SaveOptions { format, maximize_compatibility, image_previews, mark_saved }) ->
  Saved {bytes, warnings, reused_layers, encoded_layers}` (PSD/PSB incremental, `.pcraft`, PNG,
  JPEG, WebP, TIFF), `save_psd(doc) -> Vec<u8>`.
- **Rendering**: `render(doc, &RenderOptions { rect, scale, max_side, layers, uids }) -> Rgba`;
  `snapshot(doc)` to render on another thread (`Snapshot::render`, `Snapshot::thumbnail`).
- **Fonts**: `add_fonts(files)` registers font files and redraws the type that waited for them;
  `add_document_fonts(doc, files)` holds them for one document (unregistered when the last holder
  closes; the registry is the process's, so type elsewhere can use them meanwhile);
  `fonts_ready(doc)`, `Event::FontsReady {document, ready}` (once per document, when it has them
  all), `request_missing_fonts(doc, &provider)`, `layout_digest(doc, layer, hashes)`;
  `fonts::use_system_fonts(false)` at start-up for the same layout on native and wasm.
- **AI**: `install_ai(&AiConfig { models_dir, models_url, device, worker_url, .. })`,
  `ai_availability()`, `prepare_remove_ai()`, `ai_model_status(model)`, `download_ai_model(model)`;
  typed params (`ai::RemoveAi`, `InpaintAi`, `RemoveBubbleText`, `UpscaleAi`) and replies
  (`ai::AiReply`: device, cpu, fellBack, notices, ms). Remove with AI is one undo step.
- **Other helpers**: `get_pixels`, `put_pixels`, `open_rgba`, `put_rgba`, `add_rgba`, `batch`, `undo`/`redo`,
  `begin_group`/`end_group`/`group`, `views`/`define_view`/`apply_view`, `show`, `prewarm`,
  `close`, `tool`/`set_tool`.
- **Events**: `poll()` returns them (the widget polls for you); `subscribe(callback)` and
  `events()` (an `mpsc` channel) get them too. Kinds: `layerPixelsChanged {document, layer, uid,
  rect, pixelRev, origin}`, `layersChanged {…, what}`, `layersReordered`, `textChanged`,
  `selectionChanged`, `toolChanged`, `historyChanged`, `documentChanged`, `documentsChanged`,
  `jobFinished`, `fontsReady`.
- **Live sync** (experimental, module `live`): `live_start(doc, peer)`, `live_take_local(doc)` →
  `Vec<Envelope>` (serde JSON) to send, `live_apply(doc, &envelope)` for what arrives; no undo
  step, per layer last writer wins, layers addressed by `uid`.

What is stable and what is experimental is listed at the top of the crate docs (`cargo doc -p
capishot-host --open`).

## Binary size

Where the size goes, measured on Capishot's own 0.13.7 release builds (`capishot` Linux x86-64,
the same code as the 101 MB Windows exe; the web app's `wasm-bindgen` output before the split),
by crate from the symbols (`nm` natively, `twiggy` on wasm):

| Part | Native app (109 MB of code and data) | Web app (45.6 MB wasm) |
|---|---|---|
| In-process AI: burn, CubeCL/cubek kernels, pliron, `photocraft-ai*` | 25.2 MB (23%) | none (the AI worker is its own wasm) |
| Rust std/core/alloc, serde, rayon, hashbrown (mostly generic code instantiated for Capishot) | 21.7 MB (20%) | 4.9 MB (11%) |
| Engine, document model, paint, algorithms, compositor, GPU, vector | 15.7 MB (14%) | 8.1 MB (18%) |
| Capishot's interface (`photocraft-ui-egui`, `craft-ui`, the app shell) | 15.1 MB (14%) | 6.1 MB (13%) |
| File formats (PSD, TIFF, EXR, JPEG XL, raw, Affinity, PDF, image codecs) | 10.1 MB (9%) | 5.2 MB (11%) |
| egui, eframe, winit, wgpu, naga | 8.6 MB (8%) | 2.4 MB (5%) |
| Type (shaping, font parsing) | 3.0 MB (3%) | 2.0 MB (4%) |
| Networking and D-Bus (rustls, ureq, zbus: updates, model downloads, portals) | 2.6 MB (2%) | none |
| Vector rasterizing (vello, tiny-skia, usvg) | 1.5 MB (1%) | 2.2 MB (5%) |
| Plug-ins (wasmi) | 0.6 MB | none |
| Data: fonts (Inter, the UI's fallbacks), icons, ICC profiles, presets, catalogs | in 11.5 MB `.rodata` | 4.8 MB (10%) |
| Function names (the wasm `name` section) | stripped | 9.0 MB (20%), stripped by the web build's split step |

Natively, unwinding tables add 11 MB (`.eh_frame`, `.gcc_except_table`): they are what lets a
host survive a panic in the editor (the view catches it and reports `ViewEvent::Failed`), so keep
`panic = "unwind"`. The symbol table adds 27 MB to an unstripped binary. No test assets are linked
in; debug info is already off in Capishot's release profile.

What a host leaves out, and how:

| Leave out | How | Saves |
|---|---|---|
| In-process AI | don't enable `ai-native` / `ai-web` (run Remove with AI on a server, or use the plain Remove) | about 40 MB natively, measured (`examples/host-egui/README.md`) |
| Capishot's full interface | `features = ["view"]` without `full` (the canvas alone) | see the table below |
| Camera raw, OpenEXR, HEIC | off unless you enable `camera-raw`, `exr`, `heif` | several MB |
| WebAssembly plug-ins | off unless you enable `plugins` | 0.6 MB |
| Translation catalogs beyond English, Spanish and Brazilian Portuguese | off unless you enable `all-catalogs` (others can be fetched at run time) | about 1 MB |
| The symbol table | `[profile.release] strip = true` (`"symbols"`) | 15–25% of the file (panic backtraces lose function names) |
| Debug info | `strip = "debuginfo"` at least; in dev, `[profile.dev.package."*"] debug = false` | most of a debug build |
| The wasm `name` section | `wasm-opt --strip-debug`, or `wasm-bindgen` without `--keep-debug` | 20% of the wasm |

Release profile advice (what `examples/host-egui/Cargo.toml` uses):

```toml
[profile.release]
lto = "thin"          # "fat" is a few % smaller and links much slower
codegen-units = 1
strip = true          # or "debuginfo" to keep symbol names in backtraces
# panic stays "unwind": the view catches an editor panic and the host keeps running

[profile.wasm-release] # the browser build (Trunk: data-cargo-profile="wasm-release")
inherits = "release"
opt-level = "z"
lto = "fat"

[profile.wasm-release.package."*"]
opt-level = "s"
# pixel crates fast: photocraft-compose, -raster, -algo, -paint at opt-level = 3
```

`opt-level = "s"` natively makes the binary smaller and the pixel code slower; keep 3 for the
pixel crates if you try it. Nothing above drops a feature from Capishot's own app or web build:
they enable what they ship.

HOST_SIZE_TABLE

## License

MIT OR Apache-2.0, like the whole workspace (`LICENSE-MIT`, `LICENSE-APACHE` at the repository
root). You may depend on these crates and ship them inside your app, native and wasm, closed or
open. Ship the notices: include both license texts (or the one you choose) and the third-party
notices of the dependency tree (for example generated with `cargo about` or `cargo-license`) in
your app's About box or bundle. Assets with their own licenses are listed in `ATTRIBUTION.md`; the
brand logos in `docs/brand/` are not covered (see `docs/brand/LICENSE-brand.txt`) and no font
files are bundled.
