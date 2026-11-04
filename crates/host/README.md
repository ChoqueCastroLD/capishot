# capishot-host

Capishot's engine as a library, in the host app's own process: open documents from bytes, run any
control-protocol command by id, read and write pixels, group undo steps, run long commands as
jobs, get events, and keep a document in step between collaborators. One API for native and
`wasm32-unknown-unknown`. Nothing of the app shell (menus, file dialogs, windows) is in it; the
egui widget (`CapishotView`, coming next) builds on `Host`.

## Depending on it

Pin a commit (never a branch): the API's stability promise is per commit, and `CHANGELOG.md`
lists breaking changes between them.

```toml
[dependencies]
capishot-host = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<commit>" }
```

The workspace patches some upstream crates (`[patch]` in the root `Cargo.toml`, such as egui's
fork). Cargo applies patches from the root manifest only, so a host that also uses those crates
copies the same `[patch]` entries into its own root `Cargo.toml` (the widget will need egui's).

Build for the browser with the same code: `cargo build --target wasm32-unknown-unknown`. In the
single-threaded browser build jobs run inline; with the threads build (atomics, a rayon Web Worker
pool) they run on the pool.

## Using it

```rust
use capishot_host::{Host, OpenOptions, ExecOptions, GetPixels, InspectOptions};
use serde_json::json;

let mut host = Host::new();
let (page, _warnings) = host.open_psd(psd_bytes, OpenOptions { name: Some("012.psd".into()), hidden: true })?;
let tree = host.inspect(page, InspectOptions { text_style: true })?;      // document.inspect
host.execute_with("layer.setProps", json!({"layer": 3, "visible": false}), ExecOptions::on(page))?;
host.group(Some(page), |h| {                                               // one undo step
    h.execute("layer.new.layer", json!({"name": "ES"}))?;
    h.execute("type.create", json!({"text": "¡Hola!", "point": [120, 80]}))
})?;
host.undo(page)?;
let bytes = host.save_psd(page)?;                                          // incremental

// Once per frame (the widget does it): finished jobs are applied, events come out.
for event in host.poll() {
    println!("{}", event.to_json());                                       // {"type": "layerPixelsChanged", …}
}
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
- **Typed helpers**: `open`/`open_psd`, `save`/`save_psd`, `inspect`, `get_pixels`,
  `put_pixels`, `batch`, `undo`/`redo`, `begin_group`/`end_group`/`group`, `views`/`define_view`/
  `apply_view`, `show`, `prewarm`, `close`, `set_tool`.
- **Events**: `poll()` returns them; `subscribe(callback)` and `events()` (an `mpsc` channel)
  get them too. Kinds: `layerPixelsChanged {document, layer, uid, rect, pixelRev, origin}`,
  `layersChanged {…, what}`, `layersReordered`, `textChanged`, `selectionChanged`, `toolChanged`,
  `historyChanged`, `documentChanged`, `documentsChanged`, `jobFinished`.
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
