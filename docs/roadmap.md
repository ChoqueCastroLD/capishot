# Roadmap

Status legend: ✅ done · 🟡 in progress · ⬜ not started. Updated 2026-10-05.

**Parity metrics.** `cargo xtask parity` measures how much of Photoshop's menu tree is *wired to a
command* and writes [`parity.md`](parity.md). It does **not** measure whether those commands behave
like Photoshop, feel right, or survive real files. Read it next to the PSD composite oracle, the
test count, and the [honest parity assessment](#honest-parity-assessment-2026-10-05) below, which
is the reference answer to "how close are we really". The weighted feature estimate in
[`parity-estimate.md`](parity-estimate.md) (~82%) counts feature surface and overstates user-facing
readiness; treat it as an upper bound.

| M | Status | Where we are |
|---|---|---|
| M0 Skeleton | ✅ | workspace, xtask (layers / wasm / ci / stats / corpus / parity), CI workflow |
| M1 Foundation | ✅ | geom, color (27 blend modes), raster (COW tiles, any depth), doc, ops, cms (ICC) |
| M2 PSD v1 | ✅ | photocraft-psd: 134/135 real files byte-exact round trip |
| M3 Viewer app | ✅ | egui shell (Pro / Studio / Classic themes), 13+ codecs, native and web (trunk) builds |
| M4 Native format + engine | ✅ | 500+ commands, `.pcraft` (incremental, autosave, crash recovery), CLI, persistent preferences |
| M5 GPU compositor | 🟡 | wgpu compositor drives the canvas, layer effects, vector masks, artboards, pattern fills and every clip case included (≤1/255 vs CPU); Multichannel documents fall back to the CPU |
| M6 Paint + select | 🟡 | brush engine, all selection tools, multi-layer selection, snapping + smart guides, free transform + warp; stylus pressure on Windows (WM_POINTER via winit) and the web (Pointer Events, with tilt/twist); macOS and Linux wait on winit tablet support |
| M7 Adjust + filters | 🟡 | 16 adjustment layers + destructive-only adjustments, 70+ filters incl. Blur Gallery, Actions record/replay, Fade |
| M8 PSD v2 | 🟡 | adjustments (incl. Selective Color, Color Lookup), fills, effects, patterns, text, shapes, smart objects, alpha channels; oracle 111/170 |
| M9 Text, vector, styles | 🟡 | type engine + Warp Text, shapes / pen / paths, all 10 effects on CPU and GPU (parity ≤1/255, 30/31 corpus effect files on the GPU) |
| M10 Smart features | 🟡 | classical Select Subject / Object, content-aware fill and scale, healing, auto-align / auto-blend; ML backend not started |
| M11 Automation + formats | 🟡 | MCP (headless + live bridge), batch, Image Processor, prefs over MCP; DoD test passes (10 agent tasks over MCP, `automation/tests/agent_tasks.rs`); JP2 / DICOM / DPX / C2PA pending |
| M12 Pro parity | 🟡 | CMYK / Lab / Indexed / Bitmap / Duotone, ICC + soft proofing, channels + Quick Mask, smart-object stack modes, artboards, layer comps; print, HDR, photomerge, timeline pending |

**Menu parity: 532 / 625 (85.1%)** on 2026-10-01, up from 224 (35.8%) the day before. See [`parity.md`](parity.md).

## Honest parity assessment (2026-10-05)

Written after the 0.2.0 release and the first wave of real user feedback. **We are far from real
Photoshop parity.** `parity.md` says 625/625 menu items are live, but that only means each item
dispatches *a* command. In the first day of public use, users hit broken basics that all counted as
"live": text selection offset from the cursor, shortcuts that didn't fire after clicking a panel
(214 failures in an audit), panels resizing themselves, an immovable crop frame, folders that
wouldn't collapse, and lag on layout-style PSDs.

| Area | State |
|---|---|
| Core editing (layers, masks, selections, adjustments, filters, transforms, blend modes) | Broad coverage; engine quality decent; interaction/UI quality only started catching up after 0.2.0. |
| PSD fidelity | Composite oracle 113/170 (~66%). All adjustment layers and blend modes round-trip; Photoshop smart filters in PSDs, some effect shapes and text-engine details remain. psd-tools corpus being added (#81). |
| Tools | ~20 Photoshop tools missing entirely: Pencil, Mixer Brush (as a tool), Patch, Content-Aware Move, Red Eye, Pattern Stamp, Art History Brush, Freeform/Curvature Pen, anchor tools, Direct Selection, Magnetic Lasso, single row/column marquee, Color Sampler, Perspective Crop, Rotate View, vertical type and type masks, Frame. |
| Text | Engine works; advanced typography (OpenType features, text-on-path editing, full paragraph composer parity) and PSD text fidelity are partial. |
| Painting | Brush model close to Photoshop after #136/#174; no pen pressure on macOS/Linux (#79), no Mixer Brush tool, no Art History Brush. |
| AI / generative | ~0%: no Generative Fill/Expand (#41, deferred), Neural Filters or Remove tool. Classical Select Subject / Content-Aware Fill only. |
| Ecosystem | No .8BF plug-ins (by decision: sandboxed WebAssembly plug-ins instead), no ExtendScript/UXP/.atn actions, no Adobe Fonts / Libraries / cloud documents. |
| Camera RAW | DNG, CR2, Sony ARW (lossless + compressed), RW2, uncompressed ORF decode; Nikon compressed NEF, CR3, RAF fall back to the embedded preview (clean-room limits, #50). |
| Performance / robustness | Strong on large rasters (14k+ on the GPU, banded memory); layout-style documents were laggy (#125/#128 in progress); real-file testing has only just begun. |

**Estimate.** Feature surface: roughly **55–70%** of what a typical Photoshop user touches exists in
some form. "A working professional could switch today": roughly **20–35%**; professionals live in
the details (exact tool behaviour, PSD fidelity on their own files, typography, AI, plug-ins).
True 1:1 parity is **many months** of focused work, and some areas need product decisions rather
than effort (generative AI backend, the 8BF ecosystem, clean-room RAW limits).

**Making it measurable** (replace guesses with numbers; update this section as they land):
1. **Workflow acceptance tests:** 30–50 real tasks (e.g. retouch a portrait, build a social post
   with text and effects, composite with masks and adjustment layers, prepare a CMYK print file),
   each scripted end to end and checked against Photoshop's output, run on every build. The pass
   rate becomes the headline parity number.
2. **Real-file corpus:** users' shareable PSDs plus the psd-tools set (#81), with pass-rate floors
   that only go up (`crates/io/tests/corpus.rs`).
3. **Recurring visual QA:** screenshot sweeps of realistic documents (`cargo run -p photocraft-engine
   --example designer_psd`). The first sweep found 14 defects (#147–#157) that unit tests missed.
4. **User reports:** fast turnaround from report → issue → fix; ask reporters for OS, document
   size, layer count and a screenshot.

## Current focus (infrastructure before the long tail)

Landed on 2026-10-01:
- multi-layer selection, live smart objects + smart filters, alpha channels + Quick Mask;
- patterns, Warp, preferences, snapping, ~33 filters, the remaining core adjustments;
- Layer Comps and Artboards, GPU layer effects, Liquify / Puppet Warp / Perspective Warp;
- the PSD fidelity pass (oracle 102 → 111), the M11 MCP acceptance test, and the release pipeline
  (`docs/releasing.md`).

Next:
1. **Quality of what exists** (from user feedback after 0.2.0, see the assessment above): workflow
   acceptance tests, real-file corpus, visual QA, and the open user issues. 0.2.0 shipped signed and
   notarized on 2026-10-05 (`docs/releasing.md`); Windows code-signing material still needs to be
   obtained.
2. **Fidelity**: the PSD oracle (113/170). Modern Brightness/Contrast and grayscale Levels
   curves; chisel-soft / stroke-emboss bevel shapes; Photoshop's 8-bit blend rounding; non-Normal
   modes in Lab documents; Photoshop smart filters in `SoLd`.
3. **GPU**: only Multichannel documents and regions over the texture limit fall back to the CPU.
4. **Vanishing Point, Camera Raw / Lens Correction, Face-Aware Liquify** (needs a landmark model).
5. **Panels**: Patterns, Styles, Glyphs, Character/Paragraph Styles, Timeline; Custom Shape tool.
6. Print, Photomerge, Merge to HDR, video layers.

## Milestone definitions

Each milestone has a **definition of done (DoD)** and must leave `main` green on all Tier-1 platforms plus a wasm build.

| M | Name | Scope (key items) | DoD / acceptance |
|---|---|---|---|
| **M0** | Skeleton | Workspace, all crate stubs, lints, `xtask` (layers check, ci), CI matrix, testkit | `cargo test --workspace` green; `cargo check --target wasm32-unknown-unknown -p photocraft-engine -p photocraft-ui-egui` green; layering check passes |
| **M1** | Foundation | geom, color (formats, blend math for all 27 modes, sRGB/linear), raster (sparse COW tiles, U8/U16/F32), doc (full type model incl. CMYK/Lab/adjust/smart), ops (history) | Property tests (proptest) on tile COW and history; blend-mode reference tests against published formulas |
| **M2** | PSD v1 | `photocraft-psd`: header, resources, layer records, channel data (raw/RLE/ZIP/ZIP+pred), masks, groups (lsct), unicode names, unknown-block passthrough, merged image, PSB; writer | Round-trip byte-stability tests; synthetic PSD generator tests; fuzz target; ≥150 unit tests |
| **M3** | Viewer app | codecs (png/jpeg/tiff/webp/gif/bmp/tga/pnm/qoi/exr/hdr, all read+write), CPU compositor, egui shell (menu from registry, canvas pan/zoom, layers panel, history), open/save, native + web build | Opens PNG/JPEG/PSD, shows layers, toggles visibility, undo/redo, saves PNG/PSD; web build loads a file from the browser |
| **M4** | Native format + engine | `.pcraft` bundle, command registry with schemas, jobs/cancellation, snapshots via arc-swap, CLI (`convert`, `run`, `inspect`) | CLI parity tests: every GUI command also runs headless |
| **M5** | GPU compositor | wgpu planner backend, tile residency, parity tests vs CPU (all blend modes), viewport on GPU, mips | GPU vs CPU ≤1/255; pan/zoom 60 fps on a 100 MP / 20-layer document |
| **M6** | Paint + select | platform input (pen: macOS/Windows/Linux/web), brush engine v1, eraser, marquee/lasso/wand, move, free transform, crop | Brush latency <1 frame; selection-restricted paint tests |
| **M7** | Adjust + filters | 16 adjustment layers, first 30 filters, schema-generated dialogs + live preview, actions record/replay | Golden tests at 8/16/32f; action replay determinism |
| **M8** | PSD v2 | adjustment layers, fill layers, lfx2 effects (descriptor parser), text (EngineData) preserve+render, smart objects, vector masks | Composite-oracle pass rate ≥90% on corpus |
| **M9** | Text, vector, styles | parley text layers, shapes/pen/paths, all 10 layer effects on GPU | Visual goldens; PSD text round-trip |
| **M10** | Smart features | `ml` (ort native / ort-web on the web), Select Subject/Object/Sky, Remove BG, Remove tool, content-aware fill, healing, AI denoise, RAW develop | Quality benchmarks on a public dataset; timing budgets |
| **M11** | Automation + formats | MCP server, batch, scripting, remaining formats (JP2, DICOM, DPX…), C2PA | An agent completes 10 scripted edit tasks via MCP |
| **M12** | Pro parity | CMYK/Lab UI, print, HDR display, photomerge/HDR merge, timeline, layer comps, artboards, symmetry, neural filters | `xtask parity` ≥ 90% of Photoshop menu checklist |

