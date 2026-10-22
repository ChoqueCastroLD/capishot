# photocraft-ai-sd

Generative Fill for Capishot: Stable Diffusion 1.5 inpainting ported to
[burn](https://burn.dev) 0.22 in pure Rust (no Python, no ONNX). It runs on WebGPU in the
browser, on Vulkan/Metal/DX12 natively, or on the pure-Rust CPU backend.

## Public API

| Item | Feature | What |
|---|---|---|
| `tokenizer::Tokenizer` | none | CLIP byte-level BPE (vocabulary derived from `merges.txt`, which ships inside the text-encoder file as a `U8` tensor). |
| `scheduler::{Schedule, Sampler}` | none | LCM / TCD few-step samplers, reduced to per-step scalars `x' = a·x + b·ε + c·z`. |
| `rng::Noise` | none | Deterministic SplitMix64 + Box–Muller noise: a seed gives the same variant on every device. |
| `prompt::{check, translate}` | none | Safety check (sexual content involving minors, counterfeit money, forged IDs; EN/ES/PT, obfuscation-normalised) and the ES/PT → EN dictionary translation. |
| `crop::{plan, Crop}` | none | Selection bounds + context, resized to the working size (multiples of 64), and back. |
| `stream::StreamParser` | none | Incremental safetensors parsing: tensors arrive while a file streams. |
| `service::{GenFill, Request, Tier, FakeGenFill}` | none | The engine ↔ runner seam. |
| `files::tier` | none | The catalog files of each tier (`photocraft_ai::catalog::GENFILL_STANDARD`). |
| `clip`, `unet`, `vae`, `taesd`, `lora`, `nn`, `pipeline`, `runner` | `model` (`flex` / `wgpu`) | The networks, LoRA fusion on the device, the inpainting state machine, loading and `generate`. |
| `native::NativeGenFill` | `native`, not wasm | Desktop/CLI runner (files in `<config>/ai-models` or `CAPISHOT_SD_DIR`). |

## Model and licences

Chosen with the owner on 2026-10-15:

| File(s) | What | Licence |
|---|---|---|
| `sd15-text-ds8-f16`, `sd15-vae-{enc,dec}-ds8-f16`, `ds8-inpaint-unet-f16.part{0..3}of4` | Lykon/dreamshaper-8-inpainting (SD 1.5 9-channel inpainting UNet, CLIP ViT-L/14 text encoder, VAE) | CreativeML OpenRAIL-M (use restrictions in Help › Terms of Use…) |
| `tcd-sd15-lora-f16` | h1t/TCD-SD15-LoRA, fused into the UNet at load time | MIT |
| `taesd-f16` | madebyollin/taesd | MIT |

Rejected: Anything-v4 inpainting (NovelAI-leak lineage), Mistoon Anime (Civitai: no hosting
outside Civitai), SD-Turbo (Stability Community licence, no inpainting UNet), DMD2 (CC BY-NC).
The plain `stable-diffusion-v1-5/stable-diffusion-inpainting` stays an internal A/B reference.

Conversion (`/root/capishot-ai-models/sd/scripts/convert_sd.py`, outside the repository):
f16 safetensors, diffusers tensor names, the UNet split into ≤ 450 MB parts, the VAE into
encoder and decoder, LoRAs renamed from kohya to UNet names with `alpha / rank` folded into
`lora_up`; metadata `arch`, `license`, `source`, `modified`.

| File | Bytes |
|---|---|
| text encoder (+ BPE merges) | 246 669 099 |
| VAE encoder | 68 339 472 |
| VAE decoder | 98 996 014 |
| TAESD | 4 902 382 |
| UNet, 4 parts | 1 719 154 392 |
| TCD LoRA | 134 576 208 |
| **Standard tier** | **2 272 637 567** |

## Verification

`examples/sd_compare.rs` against diffusers 0.41 / transformers 5.20 / torch 2.14 (CPU, f32;
`scripts/ref_sd.py`), 256 × 256, the same inputs (the reference re-implements `rng::Noise`
so both sides draw the same noise):

| Part | max abs diff | PSNR |
|---|---|---|
| CLIP text encoder (77 tokens) | 7.6e-6 | 153 dB |
| UNet (batch 2, 9-ch, t = 500) | 6.9e-6 | 133 dB |
| VAE encoder (latent mean) | 2.6e-5 | 123 dB |
| VAE decoder | 9.0e-6 | 125 dB |
| TAESD encoder / decoder (f16 file vs diffusers' f32 weights) | 3.6e-3 / 3.1e-3 | 73 dB |
| Full inpainting pipeline, LCM-LoRA fused, 4 steps, 2 variants | 1.2e-4 | 108 dB |
| Full inpainting pipeline, TCD-LoRA fused (γ 0.3), 4 steps, 2 variants | 1.7e-4 | 107 dB |

Tolerance: components ≤ 1e-4 absolute in f32; full pipelines ≥ 100 dB on the 0..1 image.
The tokenizer matches Hugging Face's `CLIPTokenizer` on 13 prompts (`tests/tokenizer_parity.rs`,
`SD_CLIP_MERGES=<merges.txt>`), and the schedulers match diffusers' coefficients to 2e-5.

## Cost (torch FlopCounter, 512 × 512)

UNet 803 GFLOP per step (batch 1); VAE encoder 1.12 TFLOP; VAE decoder 2.51 TFLOP; text
encoder 13 GFLOP. Attention is computed in query chunks so the f32 scores of one chunk stay
under 128 MB (a 64 × 64 latent's self-attention is 4096² per head).

## Browser check (2026-10-22)

Headless Chromium, SwiftShader WebGPU (software, no `shader-f16`, so `precision: "f32"`),
single-threaded web build + AI worker (`packaging/web-ai/build.sh`), files from the local models
folder through the AI Model Manager (`ai.models.download`, Cache Storage, sha256 while
streaming): select 100×90, prompt "una manzana roja" → "red apple", 1 variant, 256 px, 1 step.

| | |
|---|---|
| Download of the 9 files (2.27 GB, local server) | 24 s |
| Load (stream into GPU buffers, TCD fusion) + generate | 436 s (SwiftShader runs WebGPU on the CPU) |
| Peak RSS, all Chromium processes (f32 weights: 4.5 GB) | 9.4 GB |
| Longest main-thread frame gap during the job | 50 ms |
| `jobs.cancel` (Esc) | worker stopped in 24 ms, document unchanged |

Native CPU (burn-flex f32, 4 threads): 2 variants, 448×512, 4 TCD steps: 70–125 s; load 9–12 s.

## Ultra-light tier (2026-10-22)

SDXS-512-DreamShaper UNet (316 M, 1 step) + the CLIP text encoder, both 4-bit (block 32, f16
scales, round to nearest; first/last convs, time embedding, cross-attention k/v and the token
table at 8-bit), TAESD f16: 277 MB. The CPU keeps one int8 per weight and dequantizes each layer
when it runs; GPUs get dense weights at load. 4-channel UNet: blended latents (the original,
re-noised, outside the mask each step).

| | |
|---|---|
| Native CPU, 4 threads, 384×320, 2 variants, f16 file / q8 / q4 | 3.9 s / 7.3 s / 7.2 s; peak RSS 2.46 / 1.06 / 1.00 GB |
| q8 / q4 vs f16 (8-bit crop PSNR) | 41.5 dB / 16.7 dB (q4 draws a different but plausible result) |
| wgpu (lavapipe), 256 px, q4 | 7.5 s |
| Browser e2e, plain worker on the CPU (one thread, SIMD128), 384 px, 1 variant | 28 s incl. loading; Chromium peak RSS 2.23 GB (1.45 GB before the run); longest frame gap 41 ms |

Empty prompt on this tier: Manga LaMa (or MI-GAN) alone when downloaded, else a smooth
pre-fill re-drawn at strength 0.5.
