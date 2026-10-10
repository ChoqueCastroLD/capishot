<p align="center">
  <img src="assets/app-icon/hicolor/128x128/apps/ai.storyteller.photocraft.png" width="112" alt="Capishot">
</p>

<h1 align="center">Capishot</h1>

<p align="center">
  <b>An open-source Photoshop replacement for scanlation.</b><br>
  Photoshop's menus, tools and shortcuts, real PSD files, and PSDs rendered by
  <a href="https://github.com/ChoqueCastroLD/psd-compiler-cli">psd-compiler</a>. Native on desktop, and in the browser.
</p>

<p align="center">
  <a href="https://capichan.com/capishot/"><b>Try it in the browser</b></a> ·
  <a href="#build">Build</a> ·
  <a href="#psd-fidelity">PSD fidelity</a> ·
  <a href="#roadmap">Roadmap</a>
</p>

> Capishot es un editor de imágenes libre pensado para scans: los mismos menús, herramientas y
> atajos de Photoshop, PSD reales y el render fiel de psd-compiler. Funciona en escritorio y en el navegador.

## Why

Scanlation teams live in Photoshop: cleaning raws, redrawing, typesetting, exporting chapters.
Capishot keeps everything where a Photoshop user's hands expect it (menus, tool keys, ⌘J, ⇧⌘N,
`[` `]`, Alt-click to sample) and adds what scan work needs.

- **Photoshop parity first.** Every Photoshop menu item is wired to a command; tools, options
  bars, panels and default shortcuts follow Photoshop's.
- **PSDs that look like Photoshop's.** Saved PSDs carry a merged image rendered by psd-compiler,
  and File › Export › Compile PSD and File › Automate › Compile Folder render whole chapters.
- **Native and fast.** Rust, a GPU compositor on wgpu (Metal, Vulkan, DX12, WebGPU), copy-on-write
  tiles and multithreaded filters. The same code runs in the browser through WebAssembly.
- **Scriptable.** Every action is a command: drive it from the UI, `capishot-cli`, a JSON control
  channel or an MCP server.

## PSD fidelity

Measured on 515 Photoshop-saved files (psd-tools, ag-psd and six other open test suites),
comparing the render with the composite Photoshop stored in each file (mean difference ≤ 2/255
and at most 1% of pixels off by more than 16):

| Renderer | Files that match |
|---|---|
| PhotoCraft's compositor (Capishot's base) | 395 / 515 (76.7%) |
| psd-compiler | 508 / 515 (98.6%) |

psd-compiler matches every file the base compositor does, plus 113 more. Capishot uses it for
everything other programs see:

- the merged image of every 8-bit RGB PSD/PSB it saves;
- `File › Export › Compile PSD…` and `File › Automate › Compile Folder…`;
- `capishot-cli compile page.psd` or `capishot-cli compile chapter/ --format webp`.

Bringing the on-canvas render to psd-compiler's models is the next step (see the roadmap).

## Build

```sh
git clone https://github.com/ChoqueCastroLD/capishot
cd capishot
cargo run --release -p photocraft -- page.psd         # the desktop app (binary: capishot)
cargo run --release -p photocraft-cli -- compile chapter/ --format png
cargo test --workspace
```

The web build: `cd apps/photocraft-web && trunk build --release` (static files in `dist/web`).
Crates keep their `photocraft-*` names so Capishot can keep merging PhotoCraft's work;
`scripts/capishot-rebrand.py` re-applies the branding after each merge.

New contributors and AI agents: start with [`AGENTS.md`](AGENTS.md), then [`docs/`](docs/).

## Roadmap

1. **Fidelity:** the canvas renders with psd-compiler's calibrated models (adjustments, layer
   effects, type), and CI runs the 734-file reference suite.
2. **Scanlation tools:** a typesetting panel (paste the script, click a bubble, the line goes in
   fitted to it), text styles and fonts per series, one-click bubble cleaning, chapter folders,
   webtoon stitch/split, raw clean-up, Spanish and Portuguese spell check.
3. **Format parity with Photopea:** PDF, AI/EPS, XCF, KRA, Sketch, XD and Figma import, animated
   GIF frames, sprite sheets.

## Credits and license

Capishot is a fork of [PhotoCraft](https://github.com/storytold/photocraft) by the ArtCraft team
and the PhotoCraft contributors, and keeps merging its work. Code is MIT OR Apache-2.0
([`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE), [`NOTICE`](NOTICE)).
psd-compiler is MIT. Third-party assets are listed in [`ATTRIBUTION.md`](ATTRIBUTION.md).

The app icon (Capibara's capybara face) is a trademark of Capibara Traductor and is not covered
by the code license; see [`assets/app-icon/LICENSE.txt`](assets/app-icon/LICENSE.txt).

Capishot is clean-room like PhotoCraft: built from public specifications and observed behaviour,
with no code, shaders or assets from Adobe or Photopea.
