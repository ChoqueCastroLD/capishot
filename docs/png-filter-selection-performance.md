# PNG filter selection

Only adaptive PNG filter selection changes: stop evaluating filters when their partial cost cannot win, and stop after a zero-cost winner. The kernel processes 4000 rows of 6000 RGBA8 pixels (24 MP) for zero, identical and varied rows; deflate is outside the timer.

Measured on 2026-10-19 against `d95fdf3181c01ad41e95456091a394b105a9afb2` on an Apple M4 Max
(16 CPUs, 128 GiB), macOS arm64, rustc 1.99.1. Each branch starts directly at that
base; none of the other performance splits is applied. Release uses optimization
level 3, LTO off, 16 codegen units, debug info off and incremental compilation off.
Builds ran at nice 10 with APFS-cloned warm caches; measurements ran at normal
priority with no concurrent builds. Timings are medians of five samples after one
warm-up, except the compositor's ten Criterion samples. These synthetic workloads
are reproducible checks of this change, not a representative image corpus.

| Workload | Main ms | This change ms | Time change |
|---|---:|---:|---:|
| zero | 448.138 | 5.734 | -98.7% |
| identical | 447.020 | 59.819 | -86.6% |
| varied | 450.469 | 277.254 | -38.5% |

These are serial filter-kernel timings, not end-to-end PNG encoding timings.

The committed oracle is the previous main-branch filter function. The unit test compares exact output bytes, including the selected filter/tie order, across six bytes-per-pixel layouts, 32 patterns and absent/identical/shifted previous rows. Existing encoder round trips and deterministic worker-count tests remain in place.

Reproduce with the same release profile environment:

```sh
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 nice -n 10 cargo test --release -p photocraft-codecs --lib --no-run
# Run the printed executable at normal priority:
<printed-executable> benchmark_filter_selection --ignored --nocapture --test-threads=1
```

## Local validation

- `cargo test -p photocraft-codecs --locked`: passed (782 passed, 4 ignored).
- `cargo clippy -p photocraft-codecs --locked --all-targets -- -D warnings`: passed.
- `cargo xtask layers`: passed.
- `cargo check -p photocraft-codecs --locked --target wasm32-unknown-unknown`: passed.
- `cargo xtask test-corpus -p codecs`: passed (796 passed, 4 ignored).
