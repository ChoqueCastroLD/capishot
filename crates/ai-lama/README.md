# photocraft-ai-lama

Manga LaMa inpainting for Capishot: erase text and SFX under a selection or a Remove-tool
stroke and redraw plausible manga background (screentone, hatching, line art). Pure Rust on
[burn](https://burn.dev) 0.22: WebGPU in the browser, Vulkan/Metal/DX12 natively, or the
pure-Rust CPU backend (`flex`, wasm included). No Python, no ONNX Runtime.

## Public API

| Item | Feature | What |
|---|---|---|
| `tiles::{plan, extract, paste, resample}` | none | Crop-around-mask tiling: one tile per group of masked blobs (margin `max(margin, side/4)`, overlapping crops merge), mirror-padded to a multiple of 8 at full resolution, or resized down to `max_side` (default 512) and back. `paste` changes only masked pixels of the tile's core (`orig + (gen − orig)·coverage`). |
| `service::{Inpainter, Hooks, ModelState, Request, FakeInpainter}` | none | The engine ↔ runner seam (callback-based like `photocraft_ai::service::Upscaler`). |
| `dft::Dft2` | none | The real 2-D DFT matrices (see below). |
| `Lama::{from_safetensors, generate, generate_async, check_size, arch}` | `model` (`flex` / `wgpu`) | The generator. |
| `native::NativeInpainter` | `model`, not wasm | Desktop/CLI runner: weights from `CAPISHOT_LAMA_WEIGHTS` or `<config>/models/manga-lama-f16.safetensors`; wgpu unless the adapter is software (or `CAPISHOT_AI_DEVICE=cpu`), else CPU. |

Without features the crate has no ML dependency: the engine (and the main web bundle) uses only
the tiling and service halves.

## Weights and licences

Never commit weights. Candidates checked 2026-10-15:

| File | Architecture | Licence | Use |
|---|---|---|---|
| `lama_large_512px.ckpt` — huggingface.co/dreMaz/AnimeMangaInpainting (sha256 of the converted f16: `969b3abd…d449733`) | big-lama, fine-tuned on 300k manga/anime images | **MIT** (model card); training data provenance not stated | **Chosen** |
| `anime-manga-big-lama.pt` — github.com/Sanster/models release `AnimeMangaInpainting` (IOPaint "anime-lama") | the same weights as TorchScript (verified bit-identical) | as above (mirror) | reference only |
| `big-lama.pt` — advimman/lama | big-lama, Places365 | Apache-2.0 (code repo); Places365 is research-use data | fallback, worse on manga |
| IOPaint "manga" (`manga_inpaintor.jit` + `erika.jit`) | **not LaMa**: "Seamless Manga Inpainting with Semantics Awareness" (SIGGRAPH 2021) | CUHK custom: free incl. commercial but commercial use requires written notice to the author; derivatives restricted to the same purposes | not used |
| `lama_mpe` (manga-image-translator) | LaMa + positional encoding | repo GPL-3.0, weights unstated | not used |

Conversion (outside the repo, PyTorch reference venv): `convert_lama.py` folds every BatchNorm
into its convolution (in f64), reorders the Fourier unit's 1×1 conv from interleaved
`(re0, im0, re1, im1, …)` to blocked `(re…, im…)` channels and writes safetensors with metadata
`arch=lama-ffc`, `layout=bn-folded-v1`, `ngf`, `blocks`, `license`, `source`. f16: 101.9 MB;
f32: 203.8 MB.

## FFT approach

LaMa's spectral transform needs `rfft2`/`irfft2` (orthonormal) on 192-channel bottleneck maps
(`H/8 × W/8`, e.g. 64×64 at 512 px). burn 0.22's `burn-signal` FFT only takes power-of-two sizes
(and panics otherwise), so the Fourier unit is four dense matrix products instead
(`dft.rs`): rows `X·[C_W | −S_W]`, then the transposed columns `·[[C_H, −S_H], [S_H, C_H]]`, the
1×1 conv + ReLU on `[re, im]` channels, and the inverse pair (half-spectrum weights 1/2/…/2/1,
imaginary DC/Nyquist dropped like `irfft`). Any size, any backend (just matmul), built in f64.
Cost at 512²: about 22 GFLOP of the model's ~400 GFLOP (≈ 6 %).

## Verification

Same input and mask through the PyTorch checkpoint and this port (`examples/lama.rs raw`,
mirror-padded like IOPaint; 7 crops: 4 synthetic text-over-screentone, 3 from a local page):

| Weights / device | raw generator max diff | final 8-bit |
|---|---|---|
| f32, CPU (flex) | ≤ 0.006/255 | max 1/255, PSNR ≥ 100 dB |
| f32, wgpu on lavapipe | ≤ 0.004/255 | max 1/255 |
| f32, wasm CPU in headless Chromium | 0.003/255 | — |
| f32, WebGPU in headless Chromium (SwiftShader) | 0.004/255 | — |
| f16 | ≤ 5.1/255 (a few pixels), mean ≤ 0.016/255 | PSNR ≥ 72 dB (masked area ≥ 57 dB) |

`tests/tiny_model.rs` checks a tiny random-weight LaMa against an independent PyTorch
implementation using `torch.fft` (`tests/tiny_reference.py`) to 2e-5.

Run it:

```sh
cargo run --release -p photocraft-ai-lama --features flex,rayon,wgpu --example lama -- \
    manga-lama-f16.safetensors page.png mask.png out [flex|wgpu] [raw|tiles] [max_side]
```
