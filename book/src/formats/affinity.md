# Affinity previews

PhotoCraft can open the **embedded PNG preview** in some current Affinity `.af` files. The
document contains one editable raster layer named “Affinity preview”, at the preview's actual
dimensions. The preview may be smaller than the source document. Native layers, text, vectors,
masks, effects, pages and document colour settings are not imported; the warning on open
explains this limitation.

This is preview import, not editable Affinity document support. Files without an indexed PNG
preview fail with an actionable error. Legacy `.afphoto`, `.afdesign` and `.afpub` containers
are recognized, but their thumbnail layouts are unverified and rejected. Export PSD or PNG
from Affinity for full-resolution artwork.

An opened preview starts without a save path, including when the source was renamed to another
extension. Save asks for a new copy. Writing `.af`, `.afphoto`, `.afdesign` or `.afpub` is
unsupported. Saving `.pcraft`, PSD or PNG saves the imported preview, not the native content.
Place, smart-object content decoding and auxiliary imports that cannot display the warning
reject the preview; export PSD or PNG in Affinity before using those paths.

The standalone `photocraft-affinity` crate inspects the container and reads only the declared
version-12 thumbnail offset. It never scans for PNGs or decompresses the object graph. The PNG
decoder validates the compressed preview with limits of 4096 pixels per dimension and 128 MiB
of decoded allocation; encoded PNGs are limited to 16 MiB.
Every PNG chunk CRC is checked. Compressed PNG metadata and animated PNG previews are rejected
before decoding; these have not been observed in the native oracle files.

See [the crate's format notes](https://github.com/storytold/photocraft/blob/main/crates/affinity/README.md)
for observed layouts, provenance, tests and the remaining native-import work.
