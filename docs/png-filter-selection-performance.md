# PNG filter selection

Stop evaluating PNG filters when their partial cost cannot win. Filter choice and encoded bytes must remain identical.

Photo PNG encoding improves, with identical streams. The small designs show little change because they use the existing serial PNG encoder. The previous filter-kernel percentages are not end-to-end results.

Measured on 2026-10-19 against main `d95fdf3181c01ad41e95456091a394b105a9afb2`, independently of the other splits. Apple M4 Max, 16 CPUs, 128 GiB, macOS arm64, rustc 1.99.1; release O3, LTO off, 16 codegen units, debug off. Builds used nice 10 and warm APFS-cloned caches; measurements used default priority without concurrent builds or profiling. Five alternating paired samples follow a warmup; confirmation and fix runs use nine pairs. Time change compares the two medians; intervals resample paired ratios. Negative time change means faster. Small differences are not automatically established improvements or regressions.

The full-resolution CC0 Pixls photographs are P1000475.RW2 (24 MP foliage centre crop, no resampling) and IMG_4059.CR2 (12 MP city), each converted to U8/U16/F32 with the ProPhoto RGB profile. F32 derives from 16-bit development, not HDR capture. MIT psd-tools masks.psd supplies a 640 x 1394 web layout with 22 imported layers and three masks; mask-index.psd supplies a 1000 x 867 illustrated card with a mask. The sources and hashes are pinned by the existing corpus. These are the available authored layered files, not a large layered-photo corpus. No generated noise, repeating ramps, enlarged charts or duplicated photo layers are used.

The shared harness times in-memory operations. Input loading, image conversion, thumbnail rendering, hashes and equivalence checks are outside timers. Every save uses a fresh PcraftWriter and a real rendered thumbnail. Common reads use main-encoded files; own-output reads are separate. PSD exports use ZIP merged images at U8/U16, Raw merged images at F32, and normal RLE layer channels at U8/U16 / ZIP prediction at F32. Results do not measure filesystem throughput or browser performance.

| Document | Operation | Main ms | Branch ms | Time change | Paired 95% interval | Main / branch bytes |
|---|---|---:|---:|---:|---:|---:|
| panasonic-8 | png-encode | 358.725 | 346.020 | -3.5% | -7.0..-0.9% | 24,569,965 / 24,569,965 |
| panasonic-16 | png-encode | 338.106 | 296.860 | -12.2% | -12.3..-9.7% | 111,583,441 / 111,583,441 |
| canon-8 | png-encode | 178.675 | 169.289 | -5.3% | -35.0..-2.7% | 13,720,268 / 13,720,268 |
| canon-16 | png-encode | 162.107 | 145.356 | -10.3% | -12.2..+4.6% | 58,279,529 / 58,279,529 |
| layout-masks | png-encode | 4.037 | 3.986 | -1.3% | -3.3..+1.9% | 42,842 / 42,842 |
| layout-mask-index | png-encode | 11.343 | 11.404 | +0.5% | -0.0..+2.2% | 115,229 / 115,229 |

The real-image PNG streams match main byte for byte and decode to identical pixels. The committed previous-filter oracle also covers bytes-per-pixel layouts, early exits, zero-cost rows and ties.

Local raw evidence and the shared harness: `.local/perf-real-261019/` in the repository checkout (ignored). This comparison supersedes the earlier synthetic performance table. Small synthetic unit fixtures remain correctness checks; ignored synthetic timing tests were not run.

## Earlier split validation

These checks passed when the split was prepared; they were not all rerun for this benchmark review. Current affected checks are listed above where production code changed.

- `cargo test -p photocraft-codecs --locked`: passed (782 passed, 4 ignored).
- `cargo clippy -p photocraft-codecs --locked --all-targets -- -D warnings`: passed.
- `cargo xtask layers`: passed.
- `cargo check -p photocraft-codecs --locked --target wasm32-unknown-unknown`: passed.
- `cargo xtask test-corpus -p codecs`: passed (796 passed, 4 ignored).
