# PSD pixel buffer allocation

Only PSD pixel shuffling and sample-codec inlining change. Interleave writes into the final buffer rather than allocating an intermediate buffer per band. The kernel processes 24 million pixels with four full planes at 8-bit, 16-bit and float depth.

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
| U8 | 20.493 | 5.097 | -75.1% |
| U16 | 31.827 | 6.312 | -80.2% |
| F32 | 41.915 | 11.305 | -73.0% |

These are interleave-kernel timings, not whole PSD import/export timings. Direct RLE output is already on main and is not part of this PR.

The committed equivalence test uses the literal previous main-branch interleave implementation. It compares raw bytes across U8/U16/F32, empty and partial bands, missing/truncated planes, fill pixels and inversion. Existing deinterleave reference and big-endian sample-codec round trips cover the companion paths.

Reproduce with the same release profile environment:

```sh
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 nice -n 10 cargo test --release -p photocraft-io --lib --no-run
# Run the printed executable at normal priority:
<printed-executable> benchmark_interleave --ignored --nocapture --test-threads=1
```

## Local validation

- `cargo test -p photocraft-io --locked`: passed (617 passed, 6 ignored).
- `cargo clippy -p photocraft-io --locked --all-targets -- -D warnings`: passed.
- `cargo xtask layers`: passed.
- `cargo check -p photocraft-io --locked --target wasm32-unknown-unknown`: passed.
- `cargo xtask test-corpus -p io`: passed (646 passed, 6 ignored).
