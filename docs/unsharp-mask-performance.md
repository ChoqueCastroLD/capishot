# Unsharp Mask performance

Measured 2026-10-10–11. Base: `a28d786638ffbb84c5fb7d5d30ea1c3d594cd3bc` (fork `origin/main` at
dispatch), isolated branch `codex/unsharp-mask-perf-20261010`. Upstream was freshly fetched
and inspected at `03f4cc0de1a564930c34afbe6f8194f7905d34b1`, 23 commits ahead. The exact fallback
preserves PhotoCraft's starting output; the adaptive FFT path accepts measured rounding differences.
Neither establishes new Photoshop parity.

Eligible native, threshold-zero workloads now exceed the requested 15x target: 24 MP RGBA8
application is 37.22x faster at radius 300 and 102.17x at radius 1000. Existing 4K radius-1000
preview computation is 106.47x faster with reduction enabled and 137.08x with it disabled.
These are release CPU measurements; rounding differences and exact-fallback limits are detailed below.

The optimisation prioritises full application at large radii and the existing 4K preview workload.
FFT convolution retains the original coefficients but changes the addition order, permitting small
measured rounding differences. Positive thresholds and unstable cases keep the exact fallback.
The measurements below describe both the gain and its numerical limits.

## Upstream research

Searched upstream issues and PRs across open, closed and merged states for Unsharp Mask,
sharpening, SIMD, rustfft, FFT, blur and sampling; inspected relevant bodies, discussions,
diffs and merge commits, and the newer upstream history. No direct Unsharp Mask optimisation
or overlapping active Unsharp Mask PR was identified at the time of the search.

| Work | Status / merge commit | Relevance |
|---|---|---|
| [Gaussian Blur #867](https://github.com/storytold/photocraft/pull/867), [reported profile #464](https://github.com/storytold/photocraft/issues/464) | Merged `304322960edc53df0f7fda631e707903be73651a` | Reduced repeated halo copies and transposes for the existing three-box Gaussian approximation. Its description explicitly leaves Unsharp Mask/High Pass direct kernels unchanged. Reusing that approximation here would change sharpening and threshold decisions. |
| [Fixed-channel accumulators #1511](https://github.com/storytold/photocraft/pull/1511), [carry #1602](https://github.com/storytold/photocraft/pull/1602) | Original closed, carried/merged as `64496178d0dbac2a152a942d044ab57b747d6d54` | Already in the base's exact Gaussian horizontal convolution and resize path. These are portable Rust accumulators for compiler optimisation, not an explicit portable-SIMD subsystem. |
| [Mosaic row reads #1524](https://github.com/storytold/photocraft/pull/1524) | Merged `d651be1393b709d638dd9d0e4824a420f795d3ae` | The original AVX2/pulp implementation, features, CI and reports were removed after the maintainer noted no complete-filter SIMD gain. Only checked portable row reads landed. No allegedly merged portable-SIMD PR could be identified; explicit SIMD remains unsubstantiated. |
| [Motion Blur row convolution #902](https://github.com/storytold/photocraft/pull/902) | Merged `31790939856b59834a5500a71c4b156ece210f09` | Contiguous row traversal is useful here too. Unlike that change's merged taps, this change preserves every sample's original addition order. |
| [Adaptive Motion Blur FFT #1471](https://github.com/storytold/photocraft/pull/1471) | Merged `c7dbb37b3c61c18223e56c10d06efc71e48c7677` | Existing `rustfft` support, allocation caps and scalar fallbacks informed this implementation. The dependency is reused; the overlap-save helper is specialised to a discrete motion kernel and cannot be shared unchanged. Unsharp Mask's separate FFT path uses measured rounding limits and exact fallbacks. |
| [Integration batch #1514](https://github.com/storytold/photocraft/pull/1514) | Closed, explicitly “do not merge” | Integration test experiment; not evidence of a shipped algorithm. |
| [Preview scale #2207](https://github.com/storytold/photocraft/pull/2207), [issue #2063](https://github.com/storytold/photocraft/issues/2063) | Merged `37985c0c618d245f348051f460705a40abcfb2af` | Unsharp Mask was already correctly scaling radius on a proxy. Preview scale and quality are unchanged. |
| [Small-sigma issue #706](https://github.com/storytold/photocraft/issues/706) | Closed; fix `f15fb4ee` / #783 | Concerns `fxutil`'s approximate blur used by Camera Raw and other filters, not this exact `sharpen::unsharp` path. |
| [Smart-filter rendering #2739](https://github.com/storytold/photocraft/pull/2739) | Merged `d3ae5aad`, newer than this base | Cached filter-stack stages/background edits address a different layer of responsiveness. No duplicate cache or job system is added here. |
| [Image Size resampling #2451](https://github.com/storytold/photocraft/pull/2451) | Open when inspected | Overlaps the caller, not the kernel: Preserve Details enlargement still uses Lanczos plus this light Unsharp Mask. No resampling code changes here; a differential caller test protects it. |
| [Tilt-Shift #2880](https://github.com/storytold/photocraft/pull/2880), [Radial Blur #1499](https://github.com/storytold/photocraft/pull/1499), [Shape Blur #1566](https://github.com/storytold/photocraft/pull/1566) | Tilt-Shift open; Radial/Shape merged in base | Other filter optimisations; no shared blur changes or overlapping edited production files. |

The requested `craftrules` checkout was unavailable at both the worktree sibling and the main
checkout's sibling locations. Repository instructions and local
architecture/development/contributing/control/UI/progress/scorecard/performance guidance were used.

## Exact fallback and baseline profile

The unchanged 256×256 RGBA tile profile (20 repetitions, release, one tile on one calling thread):

| Radius | Kernel construction | Convolution | Crop + threshold/blend | Complete old tile kernel |
|---|---:|---:|---:|---:|
| 0.6 | 0.000335 ms | 0.819 ms | 0.398 ms | 1.263 ms |
| 3 | 0.000315 ms | 1.262 ms | 0.454 ms | 1.710 ms |
| 10 | 0.000545 ms | 2.912 ms | 0.426 ms | 3.384 ms |
| 30 | 0.001380 ms | 9.955 ms | 0.454 ms | 10.469 ms |

Kernel caching would save little. The new Unsharp Mask implementation streams one checked source
row into reusable scratch, premultiplies it, and accumulates each Gaussian tap over a contiguous
output row. The compiler can vectorise independent samples without explicit SIMD, unsafe code,
architecture gates or extra dependencies. The vertical pass retains its order, then the same
output allocation is unpremultiplied and sharpened against a contiguous original row.

In the exact fallback, Gaussian weights, truncation at ceil(3σ), f32 tap order on both axes, amount/100,
threshold/255, `abs(d) + 1e-6 >= threshold`, alpha cut-off `> 1e-7`, hidden colour at nonpositive
original alpha, unchanged original alpha, zero extension and canvas-edge repetition all remain.
Surface writes retain depth-specific rounding/clipping; floats retain HDR/negative values.
Selection mixing, output rectangles, undo/redo and the command API are unchanged. Smart Sharpen,
High Pass and the shared blur implementation are unchanged.

No amount-zero or small-radius identity shortcut is introduced: the original floating arithmetic
can change signed zero or premultiplied round trips even there. A high threshold is not an
identity on HDR input. The initial exact path introduces no box approximation, reduced-resolution
change, GPU backend, FFT, additional scheduler or cache. The existing native preview worker remains
in use. Large-radius FFT feasibility is assessed separately below.

Cancellation is checked per source/output row inside a tile in addition to existing tile/group
checks. Cancelled or rejected tile results are discarded. New scratch uses checked lengths,
fallible reservations and a 128-million-sample cap per buffer. Public Unsharp Mask dispatch
guards malformed/nonfinite input, zero/extreme tile overrides, source windows over 512 million
samples, more than 1,048,576 tiles, and geometry outside the documented 300k-pixel domain / ±1M
coordinates. The exact kernel accepts radii beyond the command's 1000 maximum for Preserve
Details resampling, up to 15446 (the original Gaussian i32 square's safe range); excessive
requests return `None` from the interruptible API, while its synchronous wrapper preserves the
input. The existing surface halo read still materialises a window; this change removes its
second padded copy, not the whole input read.

## Reproduction and measurement scope

All Cargo operations use `CARGO_TARGET_DIR=target/agent-unsharp-mask`. Machine: Ryzen 7 9700X,
8 physical cores / 16 logical processors, 61.6 GiB usable RAM, Windows x86_64, Rust 1.99.0.
Normal workspace release profile (thin LTO, one codegen unit), no target-cpu/native flags,
`RAYON_NUM_THREADS=8`. Timed calls are serial; no Cargo builds or other CPU-heavy task runs
concurrently. Synthetic fixtures only.

`sharpen/reference.rs` preserves the pre-change kernel independently of production helpers.
The benchmark also preserves the old surrounding tiled reads, automatic tile sizing, result
grouping, selection mixing, writes and pruning. Its unchanged before/candidate check measured
only 0.3–1.8% differences across radii, consistent with noise. Source construction, result
comparison and destruction are outside the filter timer. Both variants receive two warmups,
then alternating paired samples. Exact-path outputs are compared byte for byte; adaptive outputs
are compared sample by sample against the stated numerical error gates.

PowerShell setup and full-operation comparison:

```powershell
$env:CARGO_TARGET_DIR='target/agent-unsharp-mask'
$env:RAYON_NUM_THREADS='8'
$env:PHOTOCRAFT_UNSHARP_WIDTH='6000'
$env:PHOTOCRAFT_UNSHARP_HEIGHT='4000'
$env:PHOTOCRAFT_UNSHARP_REPS='5'
$env:PHOTOCRAFT_UNSHARP_DEPTH='8' # 16, or 32 (HDR pattern)
$env:PHOTOCRAFT_UNSHARP_CHANNELS='4' # 1 Gray, 2 Gray-alpha, 3 RGB, 4 RGBA, 5 CMYK-alpha
$env:PHOTOCRAFT_UNSHARP_CASES='0.6:150:0,3:150:8,10:300:16,30:150:0,100:300:8'
cargo test --release -p photocraft-algo unsharp_performance -- --ignored --nocapture --test-threads=1
```

For single-variant memory sampling, set `PHOTOCRAFT_UNSHARP_MODE=reference` or `candidate`;
otherwise omit it for paired comparisons. Small/no-op cases use the same benchmark with small
dimensions, including `3:0:0` and `3:500:255`. Kernel-stage profiling:
`cargo test --release -p photocraft-algo unsharp_baseline_profile -- --ignored --nocapture`.

The existing interactive preview workload is measured with
`cargo run --release -p photocraft-ui-egui --example unsharp_preview_bench` and the same dimension,
depth, repetition and case variables. It includes `preview_document` (proxy creation, parameter
scaling, engine command/history) plus CPU flattening, with two warmups and repeated samples.
The example uses the existing preview factor and also exercises Low Resolution Previews off.
Compare an unchanged baseline executable with a candidate built using the same profile/settings;
hashes cover every float of the flattened output. This is **preview compute time**, excluding
worker dispatch/settle, egui frame latency, display colour conversion, GPU upload/presentation.
It is not a measured GUI latency or native performance result on macOS/Linux/ARM/wasm.

## Adaptive large-radius path

The native dispatcher uses the existing `rustfft` dependency for radii 64–1000 on at least
65,536 output pixels, with zero/negative threshold and nonzero amount. It transforms the original
discrete f32 Gaussian coefficients in double precision: no box approximation, sigma change,
kernel resampling or reduction of preview resolution. The two axes remain separable, with an
f32 intermediate image. Transform convolution changes the addition order, so this path is not
bit-identical to the sequential f32 baseline.

Two independently scaled channels share each complex transform. Horizontal processing reads
each needed real source row once from the surface and extends canvas edges while packing the
FFT input. When the vertical halo falls outside a repeated canvas extent, it repeats horizontal
results lazily instead of decoding, copying and transforming duplicate rows. The vertical pass
gathers 32 adjacent columns per worker into contiguous memory, transforms each column, and writes
bounded groups of output stripes. Plans and coefficient spectra are shared within one apply.

Original alpha, hidden colour at nonpositive original alpha, unpremultiply cutoff, depth-specific
quantisation, HDR/negative values, selection mixing, output regions, engine commands/history and
preview scaling stay in place. Source alpha outside 0–1, NaNs/infinities, values over 1e6, outputs
near the alpha cutoff, nonfinite computed outputs, unsupported geometry or insufficient scratch
capacity decline FFT and retry the streamed exact path. The wasm build retains that exact path.
Scratch uses checked sizes and fallible reservations, FFT axes are capped at 32768 samples, and
group sizing applies a 1 GiB temporary budget to scratch/results and the intermediate image,
including a conservative FFT plan allowance. Document/source/output tiles, allocator retention
and worker stacks are separate; this budget is not a measured whole-process RSS limit.

**Every positive threshold retains the exact path.** In the unguarded experiment at radius 500,
threshold 8, a few pixels changed by 12/255 when different sums crossed the threshold. A tiny
blur difference can switch an entire sharpen contribution, so accepting generic rounding error
is insufficient there. Amount zero, smaller radii and tiny images also retain the exact path.
This conservative crossover does not promise a large speedup for every parameter combination.

Cancellation is checked between real rows, FFT channel pairs, gathered blocks and columns, and
before each write group. A cancelled or declined operation discards its output clone, including
any completed groups, before the exact fallback or caller proceeds. The existing preview worker
continues to coalesce requests and reject stale results; no new job system or GPU path is added.

Adaptive full-operation comparison (same dimensions/depth/case variables as above):

```powershell
cargo test --release -p photocraft-algo unsharp_fft_performance -- --ignored --nocapture --test-threads=1
cargo test --release -p photocraft-algo unsharp_fft_cancel_latency -- --ignored --nocapture --test-threads=1
```

It uses two warmups per variant, three alternating measured pairs, and checks the entire stored
output after every pair. Error gates are one 8-bit level, three 16-bit levels, and 3e-5 ×
(1 + |reference sample|) for floats. These are regression tolerances, not a proof for every possible
input; measured maxima and means are reported separately. The initial `unsharp_performance`
benchmark now explicitly bypasses FFT to keep the exact fallback measurements reproducible.

### Adaptive full-resolution results

Complete CPU application, two warmups per variant followed by three alternating measured pairs.
RGBA source has variable alpha; the float fixture also contains negative/HDR values. Amount 150,
threshold 0 throughout this table. All three entire stored outputs pass the quality gate per row.

| Image / depth | Radius | Before median ms [range] | After median ms [range] | Ratio | Max absolute sample error |
|---|---:|---:|---:|---:|---:|
| 24 MP / u8 | 64 | 1836.30 [1805.54–1859.18] | 528.08 [526.11–536.98] | 3.48x | 0.00392160 |
| 24 MP / u8 | 100 | 3487.37 [3418.93–3536.02] | 526.56 [516.39–532.87] | 6.62x | 0.00392160 |
| 24 MP / u8 | 300 | 19869.38 [19682.39–19935.54] | 533.82 [529.20–537.35] | 37.22x | 0.00392160 |
| 24 MP / u8 | 1000 | 79471.57 [79069.21–79683.28] | 777.86 [767.88–782.60] | 102.17x | 0.00392160 |
| 24 MP / u16 | 300 | 19315.11 [19282.10–19317.82] | 550.60 [539.82–552.65] | 35.08x | 0.00001532 |
| 24 MP / f32 HDR | 300 | 19352.44 [19304.59–19659.85] | 574.25 [559.51–580.96] | 33.70x | 0.00000876 |
| 36.15 MP / u8 | 300 | 27832.82 [27538.60–27941.65] | 849.79 [828.42–855.38] | 32.75x | 0.00392160 |

Of 96 million stored samples in the first measured pair, u8 changed-sample counts are 1473,
1620, 2624 and 5596 at the four radii respectively, each by one storage level. Radius 300
changes 848273 u16 samples (maximum approximately one storage level) and 62374913 float samples;
float mean absolute error is 0.000000675. Changed float bits are expected from the altered
addition order; their number alone does not describe error magnitude. Alpha samples are preserved.
The 36.15 MP fixture changes 4352 of 144609280 stored samples, by one 8-bit level.

The exact fallback remains necessary: these speedups describe eligible native large-radius
workloads, not all Unsharp Mask calls. At radius 64 the crossover is already beneficial here,
but the 15x target is reached at larger radii. No published performance budget or Photoshop
rendering-parity floor is changed by this local evidence.

1920×1080 RGBA8 proxy-sized full compute (same warmups/pairs; excludes proxy creation/flattening):

| Radius | Threshold | Before median ms [range] | After median ms [range] | Ratio | Changed samples / max error |
|---|---:|---:|---:|---:|---:|
| 300 | 0 | 5490.12 [5471.78–5742.20] | 63.99 [62.44–74.62] | 85.79x | 323 / one u8 level |
| 500 | 0 | 11811.43 [11805.71–11861.79] | 70.72 [70.07–71.84] | 167.01x | 365 / one u8 level |
| 500 | 8 | 11858.55 [11777.28–11923.29] | 9123.51 [8921.48–9188.15] | 1.30x | 0 / 0 (exact fallback) |

For the positive-threshold fixture, the exact streamed path's benefit grows with the halo-to-image
ratio but remains far below 15x. The prototype's threshold-crossing error is not shipped.

### Existing 4K preview computation

3840×2160, RGBA8 opaque synthetic image, radius 1000, amount 150, threshold 0. Same native
`preview_document` path as the filter dialog, including proxy creation, scaled parameters,
engine command/history and CPU flattening. Two warmups then three measured calls per variant;
unchanged baseline executable first, candidate second, both built with the same release settings.

| Preview mode | Proxy factor / applied radius | Before median ms [range] | After median ms [range] | Ratio |
|---|---:|---:|---:|---:|
| Low Resolution Previews on | 2 / 500 | 12035.35 [11968.12–12184.11] | 113.04 [112.39–117.77] | 106.47x |
| Low Resolution Previews off | 1 / 1000 | 55979.34 [55884.15–56261.68] | 408.38 [389.85–419.97] | 137.08x |

Every variant's full flattened-float hash is stable across its five calls; hashes differ between
old and adaptive implementations because rounding differences were accepted. Numerical quality
is measured separately by the full-operation comparisons and synthetic regression tests above.
The preview sequence is not paired/interleaved, so clock/load drift is not cancelled; the observed
gain is much larger than the measured run variation. These are computation timings, **not slider
event-to-screen latency**: existing worker scheduling, 150 ms settle, egui frames, display colour
conversion and GPU presentation are excluded. Reduced preview resolution and radius scaling
are unchanged. There is no new cache or preview scheduler.

The native 3840×2160 radius-1000 cancellation benchmark signals after 10 ms and measures
signal-to-return times of 3.827, 2.946, 4.311, 3.160 and 4.194 ms (five repetitions). The source
remains unchanged after cancellation. This measures cancellation inside the filter, separately
from the existing preview worker's dispatch and settle time.

## Initial exact-path results

Full tiled CPU operation, medians of five alternating pairs after two warmups per variant.
Ratios are before/after (1.20x means about 17% less time, not 20%).

| Image / storage / channels | Radius | Amount / threshold | Before ms | After ms | Ratio |
|---|---:|---:|---:|---:|---:|
| 24 MP / u8 / RGBA | 0.6 | 150 / 0 | 205.20 | 170.62 | 1.20x |
| 24 MP / u8 / RGBA | 3 | 150 / 8 | 265.43 | 222.36 | 1.19x |
| 24 MP / u8 / RGBA | 10 | 300 / 16 | 400.00 | 365.28 | 1.10x |
| 24 MP / u8 / RGBA | 30 | 150 / 0 | 915.24 | 872.05 | 1.05x |
| 24 MP / u8 / RGBA | 100 | 300 / 8 | 3263.71 | 3168.44 | 1.03x |
| 24 MP / u16 / RGBA | 0.6 | 150 / 0 | 230.17 | 188.21 | 1.22x |
| 24 MP / u16 / RGBA | 3 | 150 / 8 | 279.26 | 237.25 | 1.18x |
| 24 MP / u16 / RGBA | 30 | 150 / 0 | 930.46 | 908.80 | 1.02x |
| 24 MP / f32 HDR / RGBA | 0.6 | 150 / 0 | 257.55 | 207.48 | 1.24x |
| 24 MP / f32 HDR / RGBA | 3 | 150 / 8 | 297.46 | 261.37 | 1.14x |
| 24 MP / f32 HDR / RGBA | 30 | 150 / 0 | 979.41 | 950.21 | 1.03x |
| 36.15 MP / u8 / RGBA | 0.6 | 150 / 0 | 334.43 | 270.17 | 1.24x |
| 36.15 MP / u8 / RGBA | 3 | 150 / 8 | 425.81 | 361.69 | 1.18x |
| 36.15 MP / u8 / RGBA | 30 | 150 / 0 | 1373.99 | 1326.86 | 1.04x |
| 24 MP / u8 / Gray | 3 | 150 / 8 | 75.60 | 52.25 | 1.45x |
| 24 MP / u8 / Gray | 30 | 150 / 0 | 485.43 | 211.28 | 2.30x |
| 24 MP / u8 / RGB | 3 | 150 / 8 | 154.91 | 127.21 | 1.22x |
| 24 MP / u8 / RGB | 30 | 150 / 0 | 876.03 | 639.52 | 1.37x |
| 24 MP / u8 / CMYK-alpha | 3 | 150 / 8 | 329.46 | 285.11 | 1.16x |
| 24 MP / u8 / CMYK-alpha | 30 | 150 / 0 | 1224.70 | 1138.31 | 1.08x |

The 1.5 MP RGBA proxy-sized compute fixture measures 13.93 → 11.44 ms at radius 0.15,
14.88 → 12.45 at 0.75, 17.28 → 13.68 at 2.5, 21.44 → 19.01 at 7.5,
293.04 → 291.90 at 100 and 3914.64 → 3111.46 at 300. This excludes proxy creation and flattening.
The radius-100 runs vary substantially (before 268–355 ms, after 218–322 ms); their 1.00x
median is not evidence of improvement. Large-radius RGBA gains of 2–5% are also modest relative
to run variability. Channel layout materially affects the gain: the old four-channel fixed
accumulator was already efficient, while tap-major row traversal helps scalar channel paths more.

Small 64×48 RGBA8 images use 15 alternating pairs. Median before → after: radius 0.1
0.176 → 0.136 ms; 0.6 0.183 → 0.108; 3 0.243 → 0.181; amount-zero radius 3
0.215 → 0.156; threshold-255 radius 3 0.221 → 0.159; radius 30 1.540 → 1.121.
Submillisecond ratios are noisy (alternating allocation/scheduler states are visible in the raw
samples); no meaningful small-image regression was observed, and no identity shortcut is used.

All **220 entire-image paired comparisons** (32 scenarios) match byte for byte: maximum
sample error **0**, including depth-specific quantisation, variable alpha, HDR/negative floats,
no-op-looking parameters and image/tile edges. Regression tests additionally compare finite
sample bits and NaN classification across all eight colour modes, alpha on/off, tiny/empty
images, incomplete halos, signed zero/subnormal/nonfinite samples, threshold boundaries ±1 ULP,
soft selection/partial regions, varied tile sizes, radius 1000, Preserve Details enlargement,
malformed geometry/params and cancellation. NaN sign/payload identity is not promised.

Raw timings, individual sample ranges, build settings, preview hashes and memory measurements
are preserved in [unsharp-mask-performance.json](unsharp-mask-performance.json).

### Existing preview workload and memory (initial exact path)

24 MP RGBA8, five measured runs per executable after two warmups. The unchanged executable is
run again after the candidate to expose clock/load drift. These are compute medians, not GUI latency.

| Radius | Reduced before / repeat ms | Reduced after ms | Full before / repeat ms | Full after ms |
|---|---:|---:|---:|---:|
| 0.6 | 49.65 / 49.48 | 46.05 | 620.24 / 574.21 | 495.42 |
| 3 | 54.23 / 54.27 | 49.75 | 611.36 / 617.66 | 533.32 |
| 10 | 55.34 / 57.79 | 51.21 | 764.56 / 743.75 | 682.60 |
| 30 | 60.75 / 59.98 | 57.92 | 1257.02 / 1312.75 | 1205.47 |

Reduced previews use factor 4. Full-resolution variation is substantial: at radius 0.6 the
initial old executable measured 542 ms before this final sequence's 574–620 ms. Reduced compute
improves by roughly 3–9%; common-radius full compute improves more, but gains are not universal
15x improvements. At 24 MP f32, radius 3 reduced/full medians are 77.51/833.33 → 73.49/733.21 ms;
radius 30 82.14/1457.95 → 79.60/1390.11 ms. At 36.15 MP u8, radius 3 81.63/882.93 → 74.09/769.90;
radius 30 90.30/1727.70 → 83.94/1561.62 ms. Every flattened-preview float hash matches the old
executable (including the repeated baseline).

Separate-process Windows peak working sets (25 ms polling, three repetitions per variant,
24 MP RGBA8, two warmups plus three timed calls, includes source construction):

| Radius | Before median MiB [range] | After median MiB [range] |
|---|---:|---:|
| 3 | 374.5 [374.2–376.6] | 371.8 [371.3–372.1] |
| 30 | 409.8 [402.8–410.2] | 391.4 [390.3–392.3] |
| 100 | 762.1 [757.4–765.3] | 618.9 [609.5–625.4] |

Working set includes the document, output tiles and allocator retention, so it differs from
kernel scratch. The exact path replaces a full padded premultiplied window with one row and
reuses the result for sharpening, reducing kernel image-buffer allocations from four to three.

## Validation and limits

Final default-profile checks on this Windows host:

| Check | Result |
|---|---|
| `cargo test -p photocraft-algo` | 434 passed, 22 opt-in checks ignored; doctests passed |
| `cargo test --release -p photocraft-algo` | 433 passed, 22 ignored; one pre-existing Facet NaN-sign assertion failed (below); all 15 sharpening regressions passed |
| `cargo test -p photocraft-ui-egui` | 1824 passed across unit/integration suites, 10 ignored; doctests passed |
| Engine `filters::` tests | 18 passed, including parameter rejection, selections, presets and undo/redo |
| Engine ignored `panic_hunt` | Full adversarial command-registry check passed |
| Strict Clippy, algo + UI, all targets | Passed with `-D warnings` |
| Formatting / layering | Passed; 29 crates, no layering violations |
| `cargo xtask wasm` | All configured crates and codecs HEIF feature check passed |
| Additional native algorithm targets | All seven checks passed: ARM64 macOS/Windows/Linux, x86-64 macOS/Linux, i686 Linux and armv7 Linux |
| `cargo xtask perf --quick` | Passed; baseline and budget enforcement settings unchanged |
| `cargo xtask scorecard` | Passed; regenerated content unchanged (original line endings restored) |
| `cargo xtask test-corpus --changed` | Passed selector; skipped because no corpus-covered crate changed |

Cross-target checks establish compilation, not execution or a platform-specific speedup. The native
release benchmark comparisons check every stored sample outside timing; 30 adaptive pairs across
10 scenarios pass, alongside 220 exact pairs across 32 scenarios. The 4K preview hashes check
repeatability, while numerical tolerances are checked separately. Synthetic fixtures cover flat
fields, sharp edges, offset/partial regions, repeated/transparent edges, soft selections, all colour
models/depths, hidden colour, HDR, tiny alpha, nonfinite fallback, maximum radius and cancellation.

A release-only failure in the existing unrelated
`pixelate::tests::facet_reuse_matches_scalar_in_every_model` was reproduced on the **unchanged base**:
sample 56 compares positive and negative NaN bit patterns. The default-profile full suite passes.
Pixelate production/tests are untouched; this assertion limits the claim of a completely green
release suite. The investigation restored the original candidate files after testing the base.

No performance baseline, enforced budget, corpus floor or headline Photoshop parity number is
changed. No native macOS/Linux/ARM timing, GPU speedup, GUI event-to-screen timing or universal
error bound is claimed. Positive thresholds, unstable inputs, wasm, small radii/images and scratch
limits retain the exact fallback, so the large speedup is conditional.
