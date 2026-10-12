# Exact speed-ups: Content-Aware Scale, Content-Aware Fill, Gaussian Blur, Median

2026-10-11. Four algorithms and the steps around them do less work without changing a bit of their output:

- **Content-Aware Scale** keeps the seam search's cumulative-cost map between seams and redoes it only where a removal can change it (about 5 % of the map per seam). Pixels stay put behind per-row column maps; rows are cut into 256-cell chunks so a removal shifts one chunk.
- **Region reads and writes** (`Surface::read_region`, `read_region_into`, `write_region`) convert a row span per sample-type dispatch.
- **Content-Aware Fill** checks for cancellation everywhere it used to run for 60–240 ms without checking: the Auto sampling reduction and graph cut (`Graph::maxflow_until`), the mosaic copy, the selection read. The reduction and mosaic copy run by rows in parallel, each cell summed in the same order.
- **Gaussian Blur:**
  - Lock transparency's alpha restore copies the alpha bytes in parallel (`Surface::copy_alpha_from`), instead of decoding and re-encoding every changed tile serially.
  - The box passes balance across cores with 32 px strips.
  - The direct kernel (radius ≤ 4, Unsharp Mask) adds each tap along the whole row.
- **Median / Dust & Scratches:**
  - Radius 1 uses a 19-exchange median-of-9 network on `total_cmp` keys.
  - Larger radii skip histogram updates that cancel out and precompute their move offsets.

## Timings

Apple M2 Pro (6 performance + 4 efficiency cores, 16 GB), macOS, rustc 1.99.0, release profile. Base: `64ec252dbe1a780764e30d27fa88a6835f7f6ce2` (`main`); after: this branch rebased on it. The two were built in separate target directories. Each entry is the median of three alternating pairs: before/after, after/before, before/after. The machine wasn't idle: macOS storage indexing ran throughout (load average 9–29 at the start and end of the runs). The pairs see the same conditions, but absolute times are a little high. [Raw timings and output hashes](exact-speedups-performance.json).

### Algorithms

Timed by `crates/algo/examples/bench_exact_speedups.rs`: one operation per run on a photo-like image with soft alpha, fixture construction outside the timer. Every run wrote its complete output as little-endian f32 sample bits; the last column says whether the SHA-256 of the before and after outputs matched.

| Operation | Size | Function | Before (ms) | After (ms) | Speed-up | Same bits |
|---|---|---|---:|---:|---:|---|
| Content-Aware Scale to 90 % width | 6000×4000 RGBA f32 | `seam::carve` | 5,952 | 1,913 | 3.11× | yes |
| Gaussian Blur r 4 | 7360×4912 RGBA8 | `apply_in` | 367 | 139 | 2.65× | yes |
| Gaussian Blur r 40 | 7360×4912 RGBA8 | `apply_in` | 221 | 150 | 1.47× | yes |
| Gaussian Blur r 250 | 7360×4912 RGBA8 | `apply_in` | 256 | 162 | 1.58× | yes |
| Median r 1 | 6000×4000 RGBA8 | `apply_in` | 1,618 | 58.6 | 27.61× | yes |
| Median r 3 | 6000×4000 RGBA8 | `apply_in` | 943 | 490 | 1.93× | yes |
| Median r 10 | 6000×4000 RGBA8 | `apply_in` | 1,784 | 1,147 | 1.56× | yes |
| Median r 30 | 6000×4000 RGBA8 | `apply_in` | 5,602 | 3,450 | 1.62× | yes |
| Median r 100 | 6000×4000 RGBA8 | `apply_in` | 15,789 | 10,120 | 1.56× | yes |
| Content-Aware Fill Auto sampling area | 4382×4000 RGBA f32 | `auto_sampling` | 186 | 132 | 1.40× | yes |
| Content-Aware Fill, 200 px hole | 3872×2592 RGBA f32 | `content_aware::fill` | 1,938 | 1,890 | 1.03× | yes |

### In the app (headless benchmarks)

Rows of `cargo xtask perf`'s `perf_scenarios` (CPU canvas) and `interactive_bench` (GPU refresh), built from the same two trees. The Gaussian Blur rows filter a layer with transparency locked, so they include the alpha restore.

| Scenario | Size | Metric | Before (ms) | After (ms) | Speed-up |
|---|---|---|---:|---:|---:|
| P28 Content-Aware Scale to 90 % width | 24 MP photo | p50 | 6,641 | 2,610 | 2.54× |
| P49 Cancel a Content-Aware Fill job (until the worker exits) | 24 MP photo | max of 8 cancels | 216 | 17.3 | 12.47× |
| P21 Content-Aware Fill (200 px hole) | 10 MP photo | p50 of 5 | 263 | 248 | 1.06× |
| P18 Open a 20 MP layered PSD | 20 MP PSD | p50 | 474 | 416 | 1.14× |
| Gaussian Blur r 4 + refresh, transparency locked | 36 MP photo layer | p50 of 5 | 782 | 217 | 3.60× |
| P41 Gaussian Blur r 40 + refresh, transparency locked | 36 MP photo layer | p50 of 5 | 602 | 182 | 3.31× |
| Gaussian Blur r 250 + refresh, transparency locked | 36 MP photo layer | p50 of 5 | 659 | 219 | 3.01× |

The Content-Aware Fill row barely moves: this branch only changes how it cancels. The patch search, most of its time, is untouched (#2805 speeds it up separately). P49's budget is 200 ms for the slowest of 8 cancels; the branch's slowest single cancel in these runs was 94 ms.

## Reproduce

```sh
git worktree add --detach ../photocraft-base 64ec252dbe
cp crates/algo/examples/bench_exact_speedups.rs ../photocraft-base/crates/algo/examples/
(cd ../photocraft-base && CARGO_TARGET_DIR=target cargo build --release -p photocraft-algo --example bench_exact_speedups)
cargo build --release -p photocraft-algo --example bench_exact_speedups

../photocraft-base/target/release/examples/bench_exact_speedups blur 40 before.bin
target/release/examples/bench_exact_speedups blur 40 after.bin
cmp before.bin after.bin
```

Cases: `scale`, `blur <radius>`, `median <radius>`, `auto`, `fill`. Repeat each pair three times, reversing the order for the middle pair. The app rows come from `perf_scenarios --cpu --only "<row>"` and `interactive_bench --only "<row>"`, built the same way.

## Correctness

The tests pin exact output bits computed with the previous code, and each new test was checked by breaking the code it covers:

- **Content-Aware Scale:** the reference-implementation oracle (400 random cases full of ties) also runs at chunk sizes 1, 2, 3 and 7, alongside a wide carve against the reference and the pinned hashes.
- **Content-Aware Fill:**
  - fill with every adaptation at 1/3/4 channels;
  - Auto sampling, including reduced windows;
  - the command at 8/16/32-bit with auto, rectangular, custom-area and channel sampling;
  - cancelling during the graph cut.
- **Gaussian Blur:**
  - 60 box-blur cases over 5 formats, with a soft selection and an extent beyond the canvas;
  - 32 direct-kernel cases;
  - the alpha copy against decode-and-encode;
  - a test fixing the band edges to the formula they were always cut by.
- **Median:** the direct-selection oracle (ties, signed zeros, values outside 0..1, NaN, with and without a threshold) covers radius 1.

The full-image hashes above caught one change the pinned cases missed: the first version of the strip change also moved the box blur's band edges, which changes the bits of blurs wider than one band. Bands are cut where they always were now, and every hash above matches.

Passed on the rebased branch: `cargo xtask ci` (fmt, clippy with warnings denied, 6,272 workspace tests, layers, wasm). One app test (`gpu_startup::…backend_recovery`) failed once in the parallel run and passes alone; it is unrelated to this change.
