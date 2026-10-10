# Capishot app icon

Capibara's capybara face (the Capibara Traductor app mark) on a rounded navy tile.

## Palette

| Colour | Hex | Used for |
|---|---|---|
| Navy (top) | `#163a55` | tile gradient |
| Ink navy (bottom) | `#0a1424` | tile gradient |
| Capibara cyan | glow around the face | from the Capibara mark |

## Geometry

A 512-unit tile, rounded square with `rx=112`, the face at 80% of the tile. `photocraft-1024.png`
and `photocraft.icns` pad the tile onto Apple's 824/1024 grid; `hicolor/` and `photocraft.ico`
use the tile edge to edge so the face reads at 16–48 px. File names keep PhotoCraft's so
packaging and `include_bytes!` paths stay as upstream has them.

## Files

Generated from the Capibara mark at 2048 px with Pillow: `photocraft.svg`,
`photocraft-small.svg` and `hicolor/scalable/` embed PNG renders.

License: see `LICENSE.txt`.
