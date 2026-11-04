# Capishot embedding API (v1)

The web build doubles as an ES module that another page (Capistudio) loads to run Capishot on a
canvas of its own. Types: [`capishot-embed.d.ts`](capishot-embed.d.ts). Code:
`src/embed_api.rs` (wasm-bindgen exports) and `src/embed_api_core.rs` (pure logic, native tests).

```ts
import load, * as capishot from '/studio/capishot/<hash>/capishot-embed.js';
await load({ module_or_path: '/studio/capishot/<hash>/capishot-embed_bg.wasm' });
capishot.embedApiVersion();                       // 1
await capishot.init({ container, mode: 'embedded', locale: 'es' });
const doc = await capishot.openDocument(bytes, 'page.psd');
await capishot.setTool('brush', { size: 20, color: '#ff3366' });
await capishot.setMode('full');                   // Capishot's whole UI, same document, no reload
const psd = await capishot.saveDocument(doc.id, 'psd');
```

- `saveDocument(id, 'psd')` is incremental (#16): layers whose pixels and mask didn't change since
  the document's last PSD save keep their encoded channels, and a plain page (pixel and type layers,
  no effects) renders only the changed area of its merged image; the bytes are a full save's. A
  3000×4300 page with 18 layers, one layer edited: about 4× faster natively (920 → 250 ms median);
  `cargo run --release -p photocraft-io --example bench_psd_incremental` measures it.
- Loading the module runs nothing on a page without `<canvas id="photocraft_canvas">` (the app's own
  `index.html` has one and starts as before). `init` starts the app on the host's canvas.
- Hosted, the app doesn't install `window.photocraft`, the Photopea hash mode or the
  `beforeunload` prompt: the host owns its page.
- A page that starts in a hidden (background) tab boots there too (feature `boot.hiddenTab`):
  `init` resolves and the `ready` event, `window.photocraft` and its `{photocraft: "ready"}`
  message come without the tab ever being shown. The browser throttles a hidden tab's timers
  (about one frame a second, less after minutes hidden), so it boots and answers slower than a
  visible one; no fake `visibilitychange` is needed.
- Calls that need the app run at the start of the next frame, so they return promises.
  `execute`, `request`, `setTool`, `fit`, `zoom`, `undo` and `redo` use the same control channel as
  the desktop control server (`docs/control-protocol.md`): same results, same errors.
- `setMode('embedded')` hides the title/menu bar, options bar, toolbar, dock, rail, status bar,
  document tabs and scroll bars, every frame (so Tab / ⇧Tab / F can't bring half of them back);
  zoom, pan, the tool cursor and floating windows the user opens still work. `setMode('full')`
  restores the chrome as it was. Both are live on the same session.
- AI commands run through `execute` (`edit.inpaintAi`, `edit.removeAi`, `paint.remove` with `ai`,
  `image.upscaleAi`) reply with `device`, `cpu` and `fellBack`: where the run really ended. A run that
  left DirectML, found no usable GPU (the CPU, or a software adapter) or lost the GPU mid-run sets
  `fellBack` and lists why in `notices` (`ai.directmlFallback`, `ai.cpuFallback`, `ai.gpuFallback`).
- Events (`onEvent`): `ready`, `documents`, `documentShown`, `documentReady`, `documentChanged`,
  `dirty`, `selectionChanged`, `toolChanged`, `modeChanged`, `zoom`, `viewChanged`,
  `viewportChanged`, `layerPixelsChanged`, `layerChanged`, `layersReordered`, `fontsMissing`,
  `fontsReady`, `fontsPreloadProgress`, `error`. Derived once per frame, after the
  frame's UI, from the session; each kind at most once per frame and only on change. Selection
  bounds are measured once per document revision.
- `viewChanged` `{id, zoom, center, rotation}`: the active document's view after any pan, zoom or
  rotation (hand tool, touch gestures, the wheel, View commands, `zoom()`/`fit()`), and when another
  document becomes active. `zoom` is device pixels per document pixel (1 = 100%), `center` the
  document point (document pixels) at the canvas centre, `rotation` degrees in (−180, 180]. A
  view whose fit is still pending reports nothing until it settles. The view is reported on the
  browser's next animation frame, with the latest values, so a gesture sends at most one
  `viewChanged` per frame however many app frames it ran. `zoom` `{id, zoom}` comes before it when
  the factor changed since the last report (feature `events.viewChanged`).

- `execute('layer.layerStyle.dropShadow', {source: 'fill+stroke', …})` casts the drop shadow from
  the layer and the outside of its strokes (an outlined lettering's 3D shadow); `'fill'` (default)
  from the layer's pixels as Photoshop does. Saved to PSD and `.pcraft` (feature
  `layerStyle.dropShadow.source`; `docs/control-protocol.md`).

- Live sync (`docs/control-protocol.md` › Live sync): every layer in
  `execute('document.inspect', {})` has `pixelRev`, moving whenever its pixels or layer mask change
  (`execute('layer.pixelRevision', {layer})` reads one), and the `layerPixelsChanged` event
  `{document, layer, rect, pixelRev}` says which layer changed where, once per frame (features
  `inspect.pixelRev`, `events.layerPixelsChanged`). `execute('document.inspect', {textStyle: true})`
  adds type layers' full run and paragraph styles (`text.style`) and every effect's settings
  (`effects.items[].fx`) (feature `inspect.textStyle`).
  Every layer also has `uid`, a stable id kept in the PSD (its layer id) and `.pcraft`: the same
  after save and reopen, a new one for a duplicate, never reused for another layer in the session;
  `id` is renumbered on every open (feature `inspect.uid`).
  - `execute(command, params, {document, history})`: `document` runs an engine command on that
    open document without activating it (feature `execute.document`); `history: 'none'` (or
    `'silent'`) applies it with no undo step, kept through the user's undo and redo (feature
    `execute.history`).
  - `execute('layer.putPixels', {layer, png, x, y, mode: 'replace' | 'over'})`,
    `execute('layer.addImage', {png, x, y, name, above})`, `execute('layer.layerMask.set', {layer,
    png, x, y})` (`png`: base64 or a `data:` URL), `execute('selection.get')` /
    `execute('selection.set', {id})`, `execute('layer.selectMany', {layers, active})`.
  - Events `toolChanged {tool}` and `layersReordered {document, group, layers}` (feature
    `events.layersReordered`).

  ```js
  const toB64 = async (blob) => btoa(String.fromCharCode(...new Uint8Array(await blob.arrayBuffer())));
  const sel = await capishot.execute('selection.get', {}, {document: doc});
  await capishot.execute('layer.putPixels', {layer: 28, png: await toB64(png), x: 120, y: 340, mode: 'over'},
                         {document: doc, history: 'none'});
  await capishot.execute('selection.set', {id: sel.id}, {document: doc});
  ```

- Smooth page switching for hosts that keep one visible document per page
  (`docs/control-protocol.md` › Live sync):
  - Hidden helper documents (feature `documents.hidden`): `openDocument(bytes, name, {hidden:
    true})`, `newDocument({…, hidden: true})` (`window.photocraft.open(bytes, name, {hidden:
    true})` → `{warnings, id}`). Reachable with `execute(…, {document})`, never shown, given a tab
    or tile, made active (`activateDocument` refuses one) or listed in the `documents` event; the
    active document, its view, the tool and the focus stay as they are. `DocumentInfo.hidden`
    says which. `closeDocument(id)` closes one; Close All leaves them.
  - `execute('layer.copyToDocument', {fromDocument, toDocument, layers?, at?, center?, offset?})`
    copies layers between documents by id, hidden ones too, activating nothing (feature
    `layer.copyToDocument.ids`).
  - `batch(commands, {document?, history?: 'one' | 'step' | 'none', render?: 'end', inspect?})`
    (also `window.photocraft.batch`) runs `[{command, params}]` as one transaction: one undo step
    (`one`, default), one each (`step`) or none (`none`); all or nothing (a failing command undoes
    the batch); drawn once, at the end (a hidden document not at all) → `{results, inspect?}`
    (feature `batch`). `execute('layer.setPropsMany', {layers: [{layer, visible, opacity, …}]})`
    is the same batch of `layer.setProps` (feature `layer.setPropsMany`).
  - `execute('layer.putPixels', {layer, src})` replaces the whole layer with an image: `src` a
    `Blob`, an `ArrayBuffer`, a typed array, a URL, base64 or a `data:` URL; one step, one repaint,
    no temporary document (feature `layer.putPixels.src`).
  - No call with `{document}` moves anything of the person's active document (pixels, selection,
    layers, history, view, tool, focus).

  ```js
  const helper = await capishot.openDocument(nextPageBytes, 'p13.psd', {hidden: true});
  await capishot.execute('layer.putPixels', {layer: 4, src: 'https://cdn.example/p13/clean.jpg'}, {document: helper.id});
  await capishot.batch([
    {command: 'layer.setProps', params: {layer: 9, visible: false}},
    {command: 'layer.setProps', params: {layer: 10, opacity: 0.5}},
  ], {document: helper.id, history: 'none', inspect: true});
  await capishot.execute('layer.copyToDocument', {fromDocument: helper.id, toDocument: page.id, layers: [12]});
  ```

  - Views and one-frame page switches (`docs/control-protocol.md` › Live sync): `execute(
    'document.views.define', {name, visible, hidden})` / `execute('document.views.apply', {name})`
    switch which layers show, no history step, nothing re-rasterized (feature `document.views`).
    `showDocument(id, {viewport: 'keep' | 'own' | {zoom, x, y}, hidePrevious, timeoutMs})` draws
    the document's canvas offscreen, then swaps it in one frame (the document shown before becomes
    a warm hidden helper; feature `editor.showDocument`); `prewarmDocument(id, {viewport, full})`
    draws it ahead so the swap is instant (feature `document.prewarm`). `request('editor.setLayout',
    {single: true})` keeps one visible document (no tiles, tabs or open cards; feature
    `editor.setLayout`), `request('editor.setMemoryBudget', {mb})` evicts the least recently shown
    documents' canvas textures beyond `mb` (feature `editor.setMemoryBudget`) and
    `request('document.memory')` reports each document's use (feature `document.memory`).
    `window.photocraft.request(…)` takes the same methods.

  ```js
  await capishot.request('editor.setLayout', {single: true});
  const next = await capishot.openDocument(nextBytes, 'p13.psd', {hidden: true});
  await capishot.prewarmDocument(next.id, {viewport: 'keep'});       // while p12 is on screen
  await capishot.showDocument(next.id, {viewport: 'keep'});          // {frames: 0, …}: one frame
  await capishot.execute('document.views.apply', {name: 'raw'}, {document: next.id});
  ```

  `batch` sends its commands as JSON: a `src` inside it must be base64 or a `data:` URL; a `Blob`
  or URL goes through `execute('layer.putPixels', …)`.
- Opening pages fast (`docs/control-protocol.md` › Live sync, "Opening pages fast"):
  - `openDocument` (and `window.photocraft.open`) settles once the document is in place, with its
    id, also when a PSD/PSB decodes in a job (feature `open.whenOpen`; before, a PSD on the web
    settled early without an id).
  - `{quiet: true}`: while a PSD/PSB decodes, its merged image (read first) shows alone at the
    view the page will have, with a thin progress bar along the canvas top: no opening card and no
    tab strip over or above the page (feature `open.quiet`). Once the layers land, the preview
    stays until the page's canvas has drawn its view offscreen (as `editor.showDocument` does), so
    no frame shows a half-composited page (feature `open.quiet.prewarm`, at most 1 s).
  - Every other shown open lands the same way (feature `open.prewarm`, at most 1 s): the opening
    card (and the preview under it) stays until the canvas has drawn the page, and a page reopened
    from the cache keeps the page shown before on screen until it is drawn (the card when there
    was none).
  - `openDocument(url)`: fetched as the page fetches, read from the response stream as it arrives
    into one buffer (the page keeps drawing); the name defaults to the URL's last segment
    (feature `open.url`).
  - `{hidden: true}` decodes a PSD/PSB off the frame: on a worker in the threads build, a step at
    a time between frames in the single-threaded one (feature `open.hidden.background`).
  - `{cacheKey, version}`: the decoded document is kept in a memory LRU (512 MB by default) and
    the same key at the same version opens again at once, nothing decoded (`data` may then be
    `null`); another version decodes again. Every document opened from one entry shares its
    pixels with the cache and the others (copy-on-write tiles): a hidden raw helper and the
    shown page hold one copy until one of them is edited. `cached` in the result says which
    (features `open.cache`, `open.cache.sharedPixels`). `request('document.cache', {remove?,
    clear?, budgetMb?})` → `{budgetMb, usedMb, entries}`; `request('perf.memory')` reports
    `pixelsMB` (every tile once) and `docCacheMB`. The cache lives in memory only (not
    IndexedDB: what it saves is the decode, and the browser's HTTP cache already keeps the
    file's bytes).

  ```js
  const page = await capishot.openDocument('https://cdn.example/w/12/p13.psd', null,
                                           {quiet: true, cacheKey: 'w12/p13', version: rev});
  const raw = await capishot.openDocument(null, 'p13-raw.psd',
                                          {hidden: true, cacheKey: 'w12/p13', version: rev}); // at once, shared pixels
  // Later, back on page 13: from the cache, or decoded again if the file changed.
  await capishot.openDocument(await fetchIfChanged(), 'p13.psd', {cacheKey: 'w12/p13', version: rev});
  ```

  Measured by `packaging/web-threads/e2e/embed-fast-open.mjs` (headless Chromium, software GL
  (SwiftShader), a 2480×3508 page with eight full-page layers, 40 MB PSD; single-threaded /
  threads build): a quiet cold open shows the page (its merged preview) after 78 / 80 ms and the
  final page after 710 / 308 ms; with the opening card the first page-like frame is the final
  one, 755 / 743 ms. From the cache: settled in 158 / 13 ms, the page drawn after 199 / 110 ms
  (the new document's first composite; switching to a page already open takes 46 / 40 ms). A
  hidden raw copy of the cached page adds no pixel memory (185.1 → 185.1 MB `pixelsMB`). A hidden
  PSD helper opens with the longest frame 93 / 36 ms and the canvas unchanged. The URL open,
  streamed in 16 chunks over about 320 ms, keeps frames under 173 / 38 ms. Still open: on its
  first frames a new document's canvas can be partly composited (one to five frames here, the
  rest filled in on the next), after the preview and with a cached reopen alike.
- Pixels without touching anything (features `layer.getPixels`, `layer.dirtySince`):
  `getPixels({layer, rect?, format?: 'png' | 'rgba', mask?}, {document?})` reads a layer's pixels
  (or its mask) from its tiles, on any open document, with no document switch, no history step and
  nothing redrawn → `{document, layer, rect, width, height, format, channels, data: Uint8Array,
  pixelRev}` (`window.photocraft.getPixels` is the same; `execute('layer.getPixels', …)` gives
  `data` as base64, as the CLI does). `execute('layer.dirtySince', {layer, rev}, {document})` →
  `{pixelRev, rect | null, full}`: where the layer changed after `rev` (`full`: too old, read it
  all). Mirroring a stroke from page A into page B:

  ```js
  capishot.onEvent(async (e) => {
    if (e.type !== 'layerPixelsChanged' || e.document !== a) return;
    // `execute` gives the PNG as base64, which putPixels takes as it is.
    const px = await capishot.execute('layer.getPixels', {layer: e.layer, rect: e.rect}, {document: a});
    await capishot.execute('layer.putPixels', {layer: mirror[e.layer], png: px.data, x: px.rect[0], y: px.rect[1]},
                           {document: b, history: 'none'});
  });
  ```
- More push events, so a host needn't poll snapshots (features `events.documentShown`,
  `events.documentReady`, `events.viewportChanged`, `events.layerChanged`, `events.fontsReady`):
  `documentShown {id, previous}`, `documentReady {id, firstFrameMs, finalFrameMs}` (a new document
  is complete: decoded, fonts settled; ms from the open call to its first frame and to the first
  complete frame), `viewportChanged {document, zoom, center, rotation, canvas, rect}` (the canvas
  size in device pixels and the document area it shows; also on resizes), `layerChanged {document,
  layer, what}` (`what`: `added`, `removed`, `name`, `visible`, `opacity`, `fillOpacity`, `blend`,
  `clipped`, `locks`, `label`, `effects`, `mask`, `vectorMask`, `parent`, `content`) and
  `fontsReady {document, ready, missing, failed}`.
- `window.photocraft` gets every event too (feature `window.events`): `photocraft-<type>` window
  events (`detail`: the event) and, with `?embed=`, `{photocraft: '<type>', …}` messages, for all
  of the above plus `documents`, `documentChanged`, `selectionChanged`, `toolChanged`, `zoom`,
  `viewChanged`, `layerPixelsChanged`, `layersReordered` and `error` (`photocraft-dirty` stays the
  app-wide boolean).
- Fonts without a fallback flash (feature `fonts.noFallbackFlash`): type whose served font is
  still downloading keeps the pixels it shows (a new layer none) until the font arrives, never
  drawing a fallback first. `request('document.fontsReady', {document?, timeoutMs?})` resolves
  once the document's fonts have arrived or failed and its type is drawn with them (`{document,
  ready, missing, failed}`; feature `document.fontsReady`); the `fontsReady` event says the same
  for every opened document, and `fontsPreloadProgress` gives a preload's progress.

- `execute('type.layoutDigest', {layer?, fontHashes?})` fingerprints how type lays out: the same
  digest from the browser and from a `photocraft-cli serve` session means every glyph sits at the
  same place (layout is deterministic given the same font files; `docs/control-protocol.md` ›
  Same type layout in the browser and the CLI lists what can still differ). Feature
  `type.layoutDigest`.

- Type layers through `execute` (`docs/control-protocol.md`): `type.frame {layer}` gives a layer's
  frame without its layout (`shape` point/box/path/inPath with its geometry, `transform`, `bounds`),
  and `type.edit {layer, path | inPath, vertical?, inset?}` gives type in a path a new path (document
  pixels: `type.frame`'s `documentPath`, a vertex moved), keeping the layer id, in one undo step
  (features `type.frame`, `type.edit.path`).

- `execute('type.createInBubble', {x, y, style?, styles?})` puts type at a document point as the
  Bubble Type tool does: in the speech bubble or narration box there, else floating text
  (`style` in the result: `bubble`, `narration` or `floating`).

- Remove with AI at full resolution: `execute('edit.removeAi', {strokes, size})` (and
  `paint.remove` with `ai: true`) fills a hole larger than the 512 px working side in overlapping
  512 px tiles at full resolution, feathered at their seams, instead of downscaling it (screentone
  and window grids stay sharp). `maxSide: 0` (the default) is that; `maxSide: 64..2048` (multiple
  of 8) downscales larger crops to it as before. Holes up to 512 px are filled as before.
  `edit.inpaintAi` keeps downscaling unless it gets `maxSide: 0` (feature `removeAi.fullRes`).

  ```js
  await capishot.execute('edit.removeAi', {strokes: [[[300, 500], [1100, 500]]], size: 300});
  await capishot.execute('edit.removeAi', {maxSide: 512}); // the selection, downscaled as before
  ```

- `execute('edit.removeBubbleText', {x, y, gapRadius?, reach?, window?})` (feature
  `removeBubbleText.search`): the search for the bubble is sized for the page's resolution (from
  the ink's stroke width around the click, not resolution metadata), so a 2x or 3x page finds its
  bubbles on the first try; a click with no bubble answers at once with `noBubble (<reason>)`
  (`beyondWindow` when the light area reaches past the search window) instead of searching the
  page. `gapRadius` (px, outline gaps up to about twice it stay shut), `reach` (px, how far from
  the click a light pixel to start from may be) and `window` (px, half the side of the largest
  square searched around the click, or `'page'`) override the automatic sizes; the result's
  `search` says which were used (`RemoveBubbleTextParams`, `RemoveBubbleTextResult`).

  ```js
  const r = await capishot.execute('edit.removeBubbleText', {x: 1830, y: 2410});
  // r.search: {scale: 2, gapRadius: 4, reach: 48, window: 1048}
  await capishot.execute('edit.removeBubbleText', {x: 1830, y: 2410, gapRadius: 6, window: 'page'});
  ```

- Fonts the host serves (`fonts/manifest.txt` next to the page, craft-fonts' format
  `family | style | file | …`; file paths may have spaces and other characters, they are
  URL-encoded for the request). A family the manifest lists is never reported missing: a document
  that uses one (opened, or reached by `layer.copyToDocument`, paste, place, `type.create`, a font
  change) fetches it and draws with a fallback until it arrives, then re-renders. Until the
  manifest itself is read nothing is called missing. Names match loosely: "Hand Of Sean Demo",
  "Hand Of Sean (Demo)" and "HandOfSeanDemo" are one family, and a PSD's PostScript names
  (`PatrickHand-Regular`) and the names in the font file (name IDs 1, 4, 6, 16, 17) find it too.
  The "Missing fonts" notice lists only families nobody serves, and those whose download failed
  ("couldn't download it").
  - `request('fonts.preload', {families, timeoutMs?, id?})` fetches families ahead and resolves once
    each has arrived or failed: `{ready, missing, failed}` (feature `fonts.preload`). A family
    already loaded or on its way is not fetched again. As each family settles, a
    `fontsPreloadProgress` event `{id?, family, status: 'loaded' | 'failed' | 'missing', done,
    total}` goes out (`id`: the call's `id`; `done === total` on the last one); with
    `window.photocraft` it is the `photocraft-fontsPreloadProgress` window event (`detail`), over
    `postMessage` `{photocraft: "fontsPreloadProgress", …}` (feature `events.fontsPreloadProgress`).
  - `request('fonts.status', {families?})` says where families stand without fetching anything:
    `{loaded, loading, failed, available, missing, manifest}` (`available`: served, not asked for
    yet; without `families`, every family the host serves; `manifest`: `loading`, `loaded` or
    `none`). Preload only what isn't `loaded` or `loading` (feature `fonts.status`).

    ```js
    const st = await capishot.request('fonts.status', {families: workFonts});
    capishot.onEvent((e) => e.type === 'fontsPreloadProgress' && e.id === 'work-12' && bar(e.done / e.total));
    await capishot.request('fonts.preload', {families: st.available, id: 'work-12', timeoutMs: 60000});
    ```
  - `init({fontsNotice: false})` (or `request('ui.set', {fontsNotice: false})`) hides the notice;
    the `fontsMissing` event `{families}` goes out either way (features `fonts.notice`,
    `events.fontsMissing`). A page using `window.photocraft` gets it as the
    `photocraft-fontsMissing` window event (`detail.families`), and over `postMessage` as
    `{photocraft: "fontsMissing", families}`.

## Versioning rules

`EMBED_API_VERSION` (in `embed_api_core.rs`) is the contract hosts check. Adding a function, an
optional parameter, an event, a result field or a `features` string keeps it; renaming, removing
or changing the shape of any of them bumps it, and the host then needs a new adapter. Keep
`capishot-embed.d.ts` in step with every change.

## Building and publishing

```sh
scripts/build-embed.sh        # trunk release build of HEAD → dist/embed/<hash>/ (or $CAPISHOT_EMBED_BUILDS/<hash>/)
```

The directory holds `capishot-embed.js`, `capishot-embed_bg.wasm`, `capishot-embed.d.ts`, the
module's lazily loaded chunks (`photocraft-web-<hash>_bg.c<N>.wasm`, fetched next to the glue in the
background once the app runs; serve them with the rest) and `manifest.json` (embed API version,
commit, build hash, sha256 and size of every file). The commit
and build hash are compiled in (`CAPISHOT_COMMIT`, `CAPISHOT_BUILD_HASH`) and reported by
`version()`, so a host can refuse a build that isn't the one it pinned.
