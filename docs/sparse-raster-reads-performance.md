# Sparse raster reads

Only default-pixel work changes: skip absent zero-default tiles and avoid temporary default-pixel allocations/copies. Read 6000×4000 single-channel pixels (24 MP) with one 9×9 painted patch, at three depths and two defaults.

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
| U8-0 | 48.484 | 0.430 | -99.1% |
| U8-0.25 | 48.934 | 4.466 | -90.9% |
| U16-0 | 48.517 | 0.441 | -99.1% |
| U16-0.25 | 48.475 | 4.461 | -90.8% |
| F32-0 | 49.066 | 0.479 | -99.0% |
| F32-0.25 | 48.511 | 4.503 | -90.7% |

These are read_region timings including output allocation. ZIP CRC-32 acceleration is already on main and is not duplicated here.

The committed oracle is the previous read_region implementation. Unit coverage compares exact float bits and checks read_region_into, RGBA/RGBA8 reads and individual samples across sparse tile boundaries, negative coordinates, signed zero, nonzero defaults and eight pixel formats.

Reproduce with the same release profile environment:

```sh
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 nice -n 10 cargo test --release -p photocraft-raster --lib --no-run
# Run the printed executable at normal priority:
<printed-executable> benchmark_sparse_reads --ignored --nocapture --test-threads=1
```

## Local validation

- `cargo test -p photocraft-raster --locked`: passed (27 passed, 1 ignored).
- `cargo clippy -p photocraft-raster --locked --all-targets -- -D warnings`: passed.
- `cargo xtask layers`: passed.
- `cargo check -p photocraft-raster --locked --target wasm32-unknown-unknown`: passed.
