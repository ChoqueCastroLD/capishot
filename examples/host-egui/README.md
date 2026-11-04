# Capishot host example (`examples/host-egui`)

A tiny egui app that embeds Capishot in its own process through `capishot-host`, the way an
outside host (Capikoma) does. It is the host contract: **if this builds and runs, a host built the
same way does too.** CI builds it natively and for wasm32 on every change
(`.github/workflows/host-egui.yml`).

What it does, one button each (`src/script.rs`, also run headless by `tests/contract.rs`):

1. opens a PSD from bytes (a page generated in memory: no PSD is committed) and shows it in
   `CapishotView`, next to a layer list the host draws itself (eye and select through commands);
2. sets the editor's tool from the host (`removeAi`, `brush`, `eraser`, `cloneStamp`…);
3. runs Remove with AI over a blob as a background job (progress, cancel), one undo step; on a
   build or device without AI it runs the plain Remove instead;
4. undoes it;
5. adds a text layer;
6. saves the document as PSD bytes (incremental).

The selected layer's bounds are drawn over the canvas by the host, in document coordinates
(`src/view.rs`): the overlay pattern for item boxes, links and other people's cursors.

## It is its own Cargo workspace

`Cargo.toml` starts with an empty `[workspace]`: this directory is **not** part of Capishot's
workspace, has its own `Cargo.lock` and target dir, and gets nothing from Capishot's root manifest
(no workspace dependencies, lints, patches or profiles). Everything a host needs is in this one
file, in three parts to copy:

1. **The dependency.** Here a path (`../../crates/host`) so CI checks the contract against the
   commit being built. An outside host pins a commit:

   ```toml
   [dependencies]
   capishot-host = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<commit>", default-features = false, features = ["view"] }
   ```

2. **The `[patch]` blocks.** Capishot builds on reviewed forks: egui 0.37 at
   `storytold/egui@6d518fc`, winit at `storytold/winit@a517b38`, and a vendored `egui-wgpu`
   (0.37 with 0.36's renderer; `vendor/egui-wgpu/README.md`). Cargo applies `[patch]` only in the
   root workspace, so every host repeats them. Copy `[patch.crates-io]` verbatim, and point
   `egui-wgpu` at the Capishot commit you pin:

   ```toml
   [patch."https://github.com/storytold/egui.git"]
   egui-wgpu = { git = "https://github.com/ChoqueCastroLD/capishot", rev = "<commit>" }
   ```

   When you move the pin, copy the blocks again from that commit's root `Cargo.toml` (they move
   together). Your own egui code then runs on the same fork; its API is egui 0.37's.

3. **The size settings** (next section).

## Binary size

A debug build of a whole Capishot shell was 169 MB. Almost all of that is debug info and features
a host doesn't use. This example's settings, and what to leave out:

| Leave out | How | Saves |
|---|---|---|
| Debug info of dependencies | `[profile.dev.package."*"] debug = false`, `[profile.dev] debug = "line-tables-only"` | most of a debug binary (it is mostly DWARF) |
| Debug info in release | `[profile.release] strip = "debuginfo"` | the rest |
| WebAssembly filter plug-ins (wasmi interpreter, `plugin.*` commands) | `default-features = false` on `capishot-host` (feature `plugins`) | a few MB |
| In-process AI (burn, its wgpu/CubeCL kernels, ONNX Runtime loader) | don't enable `ai` unless you run Remove with AI / upscales in process | tens of MB |
| Camera raw development, OpenEXR, HEIC | not enabled by `capishot-host` (Capishot's own app and CLI turn them on) | several MB |
| Translation catalogs other than Spanish and Portuguese | not enabled (`photocraft-ui-egui/all-catalogs`) | about 1 MB |
| egui's glow renderer | eframe with `wgpu` only (`default-features = false`) | a renderer |

Also keep `opt-level = 2` for dependencies in dev builds (`[profile.dev.package."*"]`): pixel code
at opt-level 0 is too slow to use, and it is no bigger.

The browser build uses the `wasm-release` profile (`index.html`'s `data-cargo-profile`): fat LTO,
size-optimized, pixel crates at `opt-level = 3`.

## Build and run

```sh
# native (Linux needs: libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev)
cargo run --release
cargo test                      # the contract, headless

# browser
rustup target add wasm32-unknown-unknown
trunk build --release           # dist/; `trunk serve --release` serves it on 127.0.0.1:8766
```

With `--features ai`, Remove with AI runs Manga LaMa in process (the model downloads once, about
100 MB, where the host says; see `capishot_host::ai`).

## License

Capishot is MIT OR Apache-2.0: a host may depend on its crates and ship them in its native app and
its wasm build. Ship the license texts and `NOTICE`/`ATTRIBUTION.md` notices with it.
