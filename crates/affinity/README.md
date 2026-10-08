# photocraft-affinity

Standalone Rust inspection of Affinity containers and extraction of an indexed
PNG preview. It does **not** decode the native document graph or write Affinity files. The
`photocraft-io` adapter opens the preview as one raster layer and reports the fidelity limitation.

`inspect(bytes)` recognizes document-class containers with versions 7–12. `preview(bytes)`
accepts only the version-12 thumbnail record observed in newly generated Affinity 3.3.0.4850
documents. Versions 7–11, files without an indexed thumbnail, other encodings, and unknown
record layouts return an actionable error. Not every current `.af` file contains a preview.
Recognizing a filename extension or the container magic does not establish editable support.

The preview must be decoded by a bounded PNG decoder: this crate checks the PNG signature,
IHDR dimensions, every chunk CRC, chunk boundaries and exact IEND termination; it does not validate
compressed pixels or the native colour settings. Limits are 16 MiB of encoded PNG and 4096
pixels in either dimension. The I/O adapter additionally limits decoded allocation to 128 MiB.
Offsets and lengths are checked before indexing. No compressed object graph is decompressed.
The only runtime dependency is the already-used, permissively licensed `crc32fast` checksum crate.
Compressed PNG text/profile chunks (`zTXt`, `iTXt`, `iCCP`) and APNG records are unsupported
and rejected before decoding. The observed native previews contain only IHDR, pHYs, IDAT and
IEND; relaxing this policy needs a total metadata allocation bound and native-file oracles.
IHDR must be first and unique, IDAT chunks must be present and contiguous, and an optional
PLTE must be unique and precede IDAT. Unknown critical chunks are rejected. This protects
against trailing format errors that the image decoder may otherwise treat as partial output.

## Observed record layout

All container integers below are little-endian. The header's offset 24 points to the record;
signature scanning is deliberately unsupported because a resource or obsolete revision may
contain another PNG.

| Record offset | Size | Observed value |
|---|---:|---|
| 0 | 8 | `ffffffff` followed by `Thmb` |
| 8 | 4 | Record version 1 |
| 12 | 4 | PNG byte length + 13 |
| 16 | 4 | PNG start offset 29 |
| 20 | 4 | Zero |
| 24 | 4 | PNG byte length |
| 28 | 1 | PNG encoding 1 |
| 29 | declared length | Exactly one PNG |

This is a narrow, observed layout rather than a vendor specification. Expanding it needs
independent, synthetic native-file oracles and deliberate version dispatch.

## Provenance and verification

The public MIT-licensed [afread reader at `04b6723`](https://github.com/VMDevCpp/afread/tree/04b672334a43e3e37ded6b5ffc57af231d589774) (Vladimir Mamonov, 2020–2021) was consulted for
the legacy header layout, `Prsn` document class and versions 7–11. This crate's Rust code was
written independently; it does not incorporate C++, GPL/AGPL readers or proprietary source.
The version-12 thumbnail layout was observed through files authored from scratch with the
public Affinity scripting SDK. No proprietary application bundle or private user files were read.

Unit tests use generated record envelopes, every truncation, bad offsets, dimensions and
mutations. Integration tests exercise the bounded PNG decoder, alpha, copy export and save
protection, including 8-bit RGBA and 16-bit Gray/alpha previews. Warningless auxiliary imports
(Place, smart-object decoding, stack/batch, displacement maps, variables, notes and video)
reject an Affinity derivative preview so it cannot be silently treated as native content.
A local ignored `native_affinity_oracle` in `crates/io/tests/affinity.rs` compares
generated Affinity files against their PNG exports. Binary oracles belong in the shared
[photocraft-corpus](https://github.com/storytold/photocraft-corpus), with committed generators,
scrubbed personal metadata and a pinned hash manifest; they are never committed here.

```sh
cargo test -p photocraft-affinity
cargo test -p photocraft-io --test affinity
AFFINITY_ORACLE_DIR=/path/to/generated/files cargo test -p photocraft-io --test affinity native_affinity_oracle -- --ignored
cd crates/affinity/fuzz
cargo +nightly fuzz run preview -- -max_total_time=60 -max_len=20000 -timeout=5
cargo +nightly fuzz run import_preview --features io -- -max_total_time=60 -max_len=20000 -timeout=5
```

Native editable import remains a separate project:

- Validate the allocation table and revision chain and choose the current `doc.dat`, including
  incremental saves, deletions, object checksums and resource bounds.
- Decode zstd/deflate and predictors with independent output, ratio and work limits.
- Build a typed tagged intermediate model, resolving object references with cycle, count and
  depth limits before mapping data into the document model.
- Preserve RGB/Gray/CMYK/Lab, 8/16/32f depth and ICC profiles; map raster tiles, layer order,
  groups, transforms, masks and blends; issue explicit warnings for unsupported effects,
  text, vectors, pages and other records rather than silently flattening or dropping them.
- Verify native-file oracles across application/platform versions and malformed inputs.
  Native writing and round trips need a later design for carrying unknown records safely.

Preview import must never be presented as completing those tasks or preserving the original artwork.
