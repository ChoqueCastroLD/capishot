# capishot-host

Capishot's editor as a library, in the host app's own process, native and
`wasm32-unknown-unknown` with one API:

- **`Host`**: the engine. Open documents from bytes, run any control-protocol command by id,
  read and write pixels, group undo steps, run long commands as jobs, get events, keep a document
  in step between collaborators.
- **`view::CapishotView`** (feature `view`): the canvas as an egui widget, drawn in the host's
  own eframe/egui context on its wgpu device. It changes no global egui style or fonts.
- **`files`, `fonts`, `render`, `ai`**: options and results for open/save from bytes, the
  host's fonts, rendering a region to RGBA, and in-process AI. The calls are `Host` methods.

Nothing of the app shell (menus, file dialogs, windows) is in it.
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
| `plugins` | no | Sandboxed WebAssembly filter plug-ins (`plugin.*` commands; the wasmi interpreter) |
| `ai-native` | no | In-process AI natively: burn on wgpu/Vulkan/Metal/DX12, DirectML on Windows, CPU fallback. **Native targets only** |
| `ai-web` | no | In-process AI in the browser through the AI Web Worker the host serves (`packaging/web-ai/build.sh`). Empty natively |

A host build: `default-features = false, features = ["view"]`, plus `ai-native`/`ai-web` per
target when it runs Remove with AI or the upscales in process. Release builds should strip debug
info (`examples/host-egui/Cargo.toml` › profiles; sizes in its README).

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
  `fonts_ready(doc)`, `request_missing_fonts(doc, &provider)`, `layout_digest(doc, layer, hashes)`;
  `fonts::use_system_fonts(false)` at start-up for the same layout on native and wasm.
- **AI**: `install_ai(&AiConfig { models_dir, models_url, device, worker_url, .. })`,
  `ai_availability()`, `prepare_remove_ai()`, `ai_model_status(model)`, `download_ai_model(model)`;
  typed params (`ai::RemoveAi`, `InpaintAi`, `RemoveBubbleText`, `UpscaleAi`) and replies
  (`ai::AiReply`: device, cpu, fellBack, notices, ms). Remove with AI is one undo step.
- **Other helpers**: `get_pixels`, `put_pixels`, `batch`, `undo`/`redo`,
  `begin_group`/`end_group`/`group`, `views`/`define_view`/`apply_view`, `show`, `prewarm`,
  `close`, `tool`/`set_tool`.
- **Events**: `poll()` returns them (the widget polls for you); `subscribe(callback)` and
  `events()` (an `mpsc` channel) get them too. Kinds: `layerPixelsChanged {document, layer, uid,
  rect, pixelRev, origin}`, `layersChanged {…, what}`, `layersReordered`, `textChanged`,
  `selectionChanged`, `toolChanged`, `historyChanged`, `documentChanged`, `documentsChanged`,
  `jobFinished`.
- **Live sync** (experimental, module `live`): `live_start(doc, peer)`, `live_take_local(doc)` →
  `Vec<Envelope>` (serde JSON) to send, `live_apply(doc, &envelope)` for what arrives; no undo
  step, per layer last writer wins, layers addressed by `uid`.

What is stable and what is experimental is listed at the top of the crate docs (`cargo doc -p
capishot-host --open`).

## License

MIT OR Apache-2.0, like the whole workspace (`LICENSE-MIT`, `LICENSE-APACHE` at the repository
root). You may depend on these crates and ship them inside your app, native and wasm, closed or
open. Ship the notices: include both license texts (or the one you choose) and the third-party
notices of the dependency tree (for example generated with `cargo about` or `cargo-license`) in
your app's About box or bundle. Assets with their own licenses are listed in `ATTRIBUTION.md`; the
brand logos in `docs/brand/` are not covered (see `docs/brand/LICENSE-brand.txt`) and no font
files are bundled.
