# photocraft-affinity

An independent, bounded Rust reader for native Affinity documents: `.af` from Affinity 3 and
`.afdesign`, `.afphoto` and `.afpub` from Affinity 1 and 2 (container versions 8 to 12). It reads
the archive, the tagged object stream in `doc.dat` and the part of the document model listed below,
and hands `photocraft-io` a document in pixels with every transform composed. It does not write
Affinity files.

## What is imported

`photocraft-io` maps the model to an 8-bit RGB document at the Affinity document's resolution:

| Affinity | PhotoCraft |
|---|---|
| Pages, Publisher spreads | one artboard per spread, spreads side by side 64 px apart |
| Artboards (`ShpN` with `ABEn`), with name, solid background, any transform | artboards (nested ones as groups with a vector mask) |
| Layers (`Scop`), groups (`Grup`), pass-through or isolated | groups (pass-through or Normal) |
| Curves (`PCrv`): cubic subpaths, closed flags, live corners (`CnrD`) | shape layers (multi-subpath curves fill even-odd) |
| Rectangles with corner radii (relative or absolute), ellipses, polygons (smooth too), stars, square stars, pies, triangles, trapezoids | shape layers |
| Compound shapes (`Comp`, add and subtract) | one even-odd shape |
| A shape or curve with children clips them | group with the outline as vector mask, the shape at its bottom |
| Vector masks (`AdCh` curves and shapes) | vector masks |
| Pixel masks (`MRst`, attached or as a layer in a group) | layer masks |
| Solid fills: RGBA, HSLA, CMYK, grey, Lab (D50) | shape fill (converted to RGB) |
| Linear, elliptical and radial gradients with Affinity's midpoint bias | gradient fills |
| Strokes: weight, scale with object, dash pattern, alignment, caps, joins, miter limit | shape strokes |
| Fill layers (`FRst`) | filled rectangles |
| Artistic and frame text: characters, font family/weight/style, size, tracking, fixed leading, colour, paragraph alignment, first baseline | type layers (rendered with the fonts installed) |
| Placed images (`ImgN`): the original JPEG/PNG embedded in the file | embedded smart objects |
| Pixel layers (`Rstr`): RGBA 8/16-bit and CMYK 8-bit tiles, cropped to their content | pixel layers |
| Embedded documents and symbols (`EmbN`) | the picture Affinity cached of them |
| Opacity, visibility, lock, names, blend modes | the same (Add as Linear Dodge) |

Everything else is reported in the import warnings, one line per kind with a count, never
dropped silently: layer effects, adjustment layers and live filters, brush and pressure strokes,
transparency gradients, fill opacity, bitmap fills, master pages, conical gradients, special shapes
(cloud, heart, cog, callouts, arrows…, imported as their bounding ellipse), corner types other than
round, stars with rounded points, several fills or strokes on one object (one of each is used),
strokes behind the fill, outlined or scaled text, text fields such as page numbers, frames with
columns or a curved outline, grey/Lab/32-bit pixels, CMYK and Lab colour (converted to RGB without
the document's profile), facing pages and the Average, Negation, Reflect, Glow and Erase blend
modes (as Normal).

Opened Affinity documents start without a save path: Save asks for a new copy and never writes
back over the Affinity file. When the native document can't be read at all, `photocraft-io` opens
the embedded PNG preview instead, as one pixel layer named “Affinity preview”, with a warning
that names the reason. That preview can't be placed or used by imports that can't show the
warning (smart-object decoding, stacks, batch, displacement maps, variables).

## Embedded preview

`preview(bytes)` reads only the thumbnail record the header's offset 24 points to (the same in
container versions 8 to 12); it never scans for PNG signatures. All integers are little-endian.

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

The PNG is limited to 16 MiB and 4096 pixels a side. IHDR must come first and be unique, IDAT
chunks contiguous, an optional PLTE unique and before IDAT, every chunk CRC must match and IEND must
end the file; compressed metadata (`zTXt`, `iTXt`, `iCCP`), APNG chunks and unknown critical chunks
are rejected. The I/O adapter decodes it with a 128 MiB allocation limit.

## Provenance

The container and object-stream layout was learned from [VMDevCpp/afread](https://github.com/VMDevCpp/afread)
(MIT, at `04b672334a43e3e37ded6b5ffc57af231d589774`, written for container versions 7–11) and
re-described in our own words before this Rust code was written; no code was translated. Its
meaning was then worked out from public documents only, without running Affinity: the semantics of
shapes, paints, text and pixel data were fitted to the thumbnail every Affinity document embeds
(Affinity's own render of it) on 176 public documents saved by Affinity 1.x, 2.x and 3.0/3.1 on
Windows, macOS and iPad. No GPL/AGPL code (such as Inkscape's Affinity extension) was read, and no
Affinity application code, asset or document is part of this repository. The same reader is
shared, as an independent copy, with VectorCraft's `vectorcraft-affinity`.

## Validation

* Unit tests on synthetic containers built by the `synth` feature (stored, zlib and zstd entries,
  checksums, budgets, cycles, hostile streams, every truncation) and property tests of random
  mutations, here and in `photocraft-io`'s `tests/affinity.rs`.
* `cargo xtask corpus --affinity` fetches 21 public CC0/MIT documents, including the four
  Affinity 3 `.af` files of [samuel-etver/vector-art](https://github.com/samuel-etver/vector-art)
  (CC0), at pinned commits, each checked against `xtask/affinity-corpus.sha256`. `tests/real_files.rs`
  parses them; `photocraft-io`'s `tests/affinity_corpus.rs` imports and flattens each one and
  compares it with the thumbnail Affinity saved in it (mean difference 0–4.6 of 255, with a
  ceiling per file).
* `fuzz/` has `cargo-fuzz` targets for the whole reader (`preview`), the object stream (`stream`)
  and the I/O import (`import_preview`):

  ```sh
  cd crates/affinity/fuzz
  cargo +nightly fuzz run stream -- -max_total_time=600
  cargo +nightly fuzz run import_preview --features io -- -max_total_time=600
  ```

What has not been verified: files from Affinity builds or platforms outside the public set, files
written by other applications, rotated or skewed images against Affinity's render, mask polarity
against a render with and without the mask, and anything listed as a warning above. The `synth`
builders write only what this reader accepts; Affinity has never opened their output, so they are
test fixtures, not an exporter.

## Safety limits

Every size is checked before it is allocated: 256 MiB per archive entry and 1 GiB per import by
default (`Limits`), a 64 MiB zstd window, 4096 saved revisions, array lengths no longer than the
bytes left, 16 777 216 decoded values per document stream (fields and array elements together, counted before
anything is allocated: a value takes about 40 bytes in memory however few it took in the file; the largest of
333 public documents uses 3.3 million), 384 levels of object nesting, 128 levels of layers, 500 000 layers, four million curve
nodes per curve, 64 megapixels per pixel layer (cropped to its content first) and 1024 gradient
stops; `photocraft-io` adds 300 000 pixels a side per document and 64 megapixels per image on the
canvas. Every archive entry's CRC-32 and size must match. Malformed input returns an `Error`; the
crate has no panics outside tests.

## Privacy

Affinity documents can hold the folder they were saved in, user names, original image paths and
XMP metadata. This reader never imports those fields.
