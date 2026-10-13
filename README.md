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
  <a href="https://capishot.75-119-145-29.sslip.io/"><b>Try it in the browser</b></a> ·
  <a href="#everything-in-the-box">Everything in the box</a> ·
  <a href="#build">Build</a> ·
  <a href="#psd-fidelity">PSD fidelity</a> ·
  <a href="#roadmap">Roadmap</a> ·
  <a href="#privacy">Privacy</a>
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

## Everything in the box

<table>
  <tr>
    <td width="33%" valign="top">
      <h4>🧰 67 tools</h4>
      Move · Artboard · Rectangular and Elliptical Marquee · Single Row Marquee · Single Column Marquee · Lasso · Polygonal Lasso · Magnetic Lasso · Object Selection · Quick Selection · Magic Wand · Crop · Perspective Crop · Slice · Slice Select · Frame · Eyedropper · Color Sampler · Ruler · Note · Count · Remove · Spot Healing Brush · Healing Brush · Patch · Content-Aware Move · Red Eye · Brush · Pencil · Mixer Brush · Color Replacement · Clone Stamp · Pattern Stamp · History Brush · Eraser · Background Eraser · Magic Eraser · Gradient · Paint Bucket · Blur · Sharpen · Smudge · Dodge · Burn · Sponge · Pen · Freeform Pen · Curvature Pen · Add Anchor Point · Delete Anchor Point · Convert Point · Horizontal and Vertical Type · Horizontal and Vertical Type Mask · Path Selection · Direct Selection · Rectangle · Ellipse · Triangle · Polygon · Line · Custom Shape · Hand · Rotate View · Zoom
    </td>
    <td width="33%" valign="top">
      <h4>🖌️ A real brush engine</h4>
      Shape Dynamics, Scattering, Texture, Dual Brush, Color Dynamics, Transfer, Brush Pose, Wet Edges, Build-up and Smoothing (including Pulled String), driven by pen pressure, tilt, rotation and direction. Brush presets, Define Brush from Selection, and deterministic, replayable strokes.
    </td>
    <td width="33%" valign="top">
      <h4>🗃️ Layers, done properly</h4>
      Groups, clipping masks, pixel and vector masks, fill layers (solid, gradient and pattern), adjustment layers, live smart objects with smart filters and lossless transforms and warps, multi-layer selection with align, distribute and link, alpha channels and Quick Mask, 27 Photoshop blend modes plus six Paint.NET variants, opacity and fill, locks, colour labels, layer filters, merge, flatten, rasterize, Layer via Copy/Cut, Paste Into.
    </td>
  </tr>
  <tr>
    <td width="33%" valign="top">
      <h4>🎨 Any colour, any depth</h4>
      RGB, Grayscale, CMYK and Lab documents at 8, 16 and 32 bits per channel. Bit depth and colour model are runtime data, so every tool works at every depth.
      <br><br>
      Real ICC colour management in pure Rust: embedded profiles, Assign and Convert to Profile with all four rendering intents and black point compensation, soft proofing (⌘Y) and Gamut Warning (⇧⌘Y) on the GPU.
    </td>
    <td width="33%" valign="top">
      <h4>🗂️ Formats</h4>
      PSD and PSB, layered TIFF (Photoshop's layer data in the TIFF, read and written in either byte order), <a href="docs/pdn.md">Paint.NET PDN3 (read, with editable layers and all 14 blend modes)</a>, <a href="docs/ora.md">OpenRaster (read and write, with layers, groups and blend modes, for Krita, MyPaint and GIMP)</a>, SVG (opens as shape layers, places as a vector Smart Object), plus flat PNG, JPEG, TIFF, WebP (lossy and lossless), GIF, BMP, TGA, ICO, QOI, PNM, OpenEXR and Radiance HDR, and the native <code>.pcraft</code> format. Flat formats retain the depths they support: PNG keeps 8/16-bit samples, TIFF keeps 8/16-bit and 32-bit float samples, and OpenEXR keeps half/float samples; JPEG, WebP and the other 8-bit formats reduce higher-depth samples with an export warning. See the <a href="crates/codecs/README.md#capability-matrix-default-build">codec capability matrix</a> for each format. HEIC photos from iPhone and Mac are read-only (included in official builds; optional <code>--features heif</code> for source builds). AVIF is write-only with the optional <code>avif</code> build feature; AVIF decoding and JPEG XL are unavailable. Affinity documents (<code>.af</code>, <code>.afdesign</code>, <code>.afphoto</code>, <code>.afpub</code>) open natively, read only, with artboards, layers, vectors, text, images and masks; effects, adjustments and other unsupported parts are listed in a warning, and Affinity export is unavailable.
    </td>
    <td width="33%" valign="top">
      <h4>🪄 The everyday essentials</h4>
      Auto Tone, Contrast and Color · Equalize · Image and Canvas Size · Crop and Trim · Reveal All · Edit › Fill and Stroke · Copy Merged · Paste in Place · guides, rulers, grid and snapping · Actions record and replay · a command palette (⌘F).
    </td>
  </tr>
</table>

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
- `File › Export › Compile PSD…` and `File › Automate › Compile Folder…` (in the browser: a
  download, and a ZIP of the PSD files you pick; type re-renders with the fonts the page has);
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

## Privacy

The desktop app sends nothing. The hosted web app (capichan.com/capishot and its mirrors) sends
anonymous usage statistics to its own server, so we know how many people use it, for how long and
which commands and tools matter: a random install id (kept in your browser) and session id, the
command ids you run counted per batch (never their settings), tool switches, active and total
session time, opened, saved and exported file *kinds* with coarse size buckets and timings, error
classes (never messages), and coarse device classes (browser and OS family, GPU vendor class,
screen size bucket, UI language, threads build). The server keeps at most the country from
Cloudflare, never your IP address. Never sent: file names, paths, document content, text you type.
Raw events are deleted after 180 days. Turn it off in **Preferences › General › Send anonymous
usage statistics**; a browser that sends Do Not Track or Global Privacy Control is never counted.
Code: `crates/ui-egui/src/telemetry.rs` (what is recorded) and
`apps/photocraft-web/src/usage_stats.rs` (sending).

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
