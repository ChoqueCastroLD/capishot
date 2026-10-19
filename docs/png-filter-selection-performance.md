# PNG filter selection

The final inner loop stops as soon as candidate cost reaches `best`. Costs cannot decrease, and only a strict improvement replaces the winning filter, preserving tie order. The previous-filter oracle now also checks an immediate tie against Sub.

Measured on 2026-10-19 against frozen main `d95fdf3181c01ad41e95456091a394b105a9afb2`, independently for each branch. Apple M4 Max, 16 CPUs, 128 GiB, macOS arm64, rustc 1.99.1. Release O3, LTO off, 16 codegen units, debug off; builds/tests at nice 10 with warm caches. Timed operations run at normal priority without concurrent builds or profiling. Persistent processes alternate execution order after a warmup. Time change compares the two medians; paired bootstrap intervals from these small samples describe variation, not guarantees.

CC0 Pixls P1000475.RW2 supplies a 6000 x 4000 centre crop (24 MP, no resampling); IMG_4059.CR2 supplies the native 4000 x 3000 city photograph. U8/U16/F32 documents retain ProPhoto RGB; F32 derives from 16-bit development and is not native HDR. No generated noise, repeated ramps, enlarged charts or duplicated photo layers are timed. Source pins and attribution remain in the existing corpus.

Nine alternating samples compare main, the previous split and the final cutoff implementation. The four photo cases use the parallel encoder; the two MIT psd-tools designs use the unchanged serial encoder. The illustrated card has a separate nine-pair confirmation, shown in the table. Input conversion, hashes and decoded-pixel checks are outside timers. Every PNG is byte-identical to main and decodes to identical pixels.

| Document | Operation | Main ms | Branch ms | Change | Paired 95% interval |
|---|---|---:|---:|---:|---:|
| panasonic-8 | png-encode | 358.588 | 337.725 | -5.8% | -6.6..-5.2% |
| panasonic-16 | png-encode | 334.253 | 301.130 | -9.9% | -10.8..-9.3% |
| canon-8 | png-encode | 179.462 | 168.651 | -6.0% | -8.4..-4.0% |
| canon-16 | png-encode | 168.310 | 150.325 | -10.7% | -17.5..-9.4% |
| layout-masks | png-encode | 4.035 | 4.020 | -0.4% | -1.4..+0.8% |
| layout-mask-index | png-encode (confirm) | 10.808 | 10.674 | -1.2% | -2.5..+0.7% |

| Document | Previous split ms | Final ms | Cutoff-only change |
|---|---:|---:|---:|
| panasonic-8 | 339.785 | 337.725 | -0.6% |
| panasonic-16 | 300.287 | 301.130 | +0.3% |
| canon-8 | 172.739 | 168.651 | -2.4% |
| canon-16 | 150.183 | 150.325 | +0.1% |
| layout-masks | 4.019 | 4.020 | +0.0% |
| layout-mask-index | 10.674 | 10.674 | -0.0% |

The cutoff-only photo intervals cross zero: these complete-image measurements do not establish an additional speedup over the previous split. The serial card initially measured +2.1% against the previous split; its independent confirmation is neutral (-0.0%, interval -1.1..+1.2%). The updated table reports the final implementation against main without relying on the earlier kernel-only promise.

Current affected checks: seven PNG unit tests passed, one ignored manual benchmark; strict all-target codec Clippy, affected codec WASM check and formatting passed. No synthetic timing test was run.

Harness, raw observations, exact inputs and binary hashes: `.local/perf-followup-261019/` (ignored local evidence). Previous flat-photo and authored-design results remain in `.local/perf-real-261019/`. Nothing was pushed and no PR was changed.

## Earlier split validation

These checks passed when the split was prepared; they were not all rerun for this benchmark review. Current affected checks are listed above where production code changed.

- `cargo test -p photocraft-codecs --locked`: passed (782 passed, 4 ignored).
- `cargo clippy -p photocraft-codecs --locked --all-targets -- -D warnings`: passed.
- `cargo xtask layers`: passed.
- `cargo check -p photocraft-codecs --locked --target wasm32-unknown-unknown`: passed.
- `cargo xtask test-corpus -p codecs`: passed (796 passed, 4 ignored).
