# Viewport pages (GPU canvas for over-budget documents)

A document's display texture set (every tile, every mip level) is held to the canvas budget
(`gpu_canvas::canvas_budget`: from the detected RAM and VRAM, with small shares on phones). The
open documents share that budget. A document whose texture set fits keeps it, with everything
around it unchanged. There is no pixel-count cutoff.

A document whose texture set doesn't fit used to fall back to the CPU canvas. It is now drawn on
the GPU canvas from viewport pages (`ui-egui/src/canvas/viewport_pages.rs`,
`gpu_canvas/pages.rs`), ported from PhotoCraft #5332:

- Pages are 512 texels square at a power-of-two reduction chosen so a texel covers at most one
  screen pixel; a view asks for at most 256 pages and coarsens beyond that. Each page is rendered
  from its core plus a one-texel apron (`photocraft_compose::viewport`), so independently rendered
  pages filter their shared edges identically; the shader discards the apron.
- A coarse backdrop of the whole document (at most 4 pages, about 4 MB at 8 bits) stays resident
  and is drawn under the view's pages, so a pan or zoom shows it, soft, never a hole. It is asked
  for first; it is left out when it would take over half of what the budget allows.
- Two background workers render pages over all documents (one page a frame on the page's thread
  in the single-threaded web build), centre of the view first. At a reduction, a faithful document
  shows a quick nearest-sample preview of every page in view first, then the exact (area-averaged)
  page. Work for a page that left the view, changed zoom or was edited is cancelled, and a late
  result of older work is dropped.
- Two fingers on the glass keep the view's reduction: no finer pages are rendered until they lift.
- Uploads are bounded per frame (2 pages, 32 MB of texels), so staging stays bounded however
  many pages finish at once.
- Pages are held to half the canvas budget (the rest is headroom for other tabs and the
  compositor); the least recently wanted pages are evicted first and wanted pages never are. Other
  documents' texture sets give way to a page, and a new texture set can take a document's pages
  (they are rendered again when its tab shows).
- An edit dirties only the pages its damage (plus the layers' effect reach) touches; a dirty page
  stays on screen until its replacement is uploaded. A small edit re-renders only the texels it
  reaches and writes them into the page held. Invalidation costs the pages held, not the
  document's layers or tiles.
- When no page fits the budget, or a page fails to render or upload, the document is drawn on the
  CPU canvas as before. A failed page is retried when the budget or the Memory Usage preference
  changes. Closing the document, a GPU fault or dropping the GPU canvas cancels its work and frees
  its pages. Pattern Preview and size-changing previews keep the old path.

Switches for comparing: `PHOTOCRAFT_VIEWPORT_PAGES=0` natively and `?pages=0` on the web turn pages
off; `PHOTOCRAFT_CANVAS_BUDGET_MB=<n>` natively sets the canvas budget. `perf.memory` reports the
active document's counters as `viewportPages`.
