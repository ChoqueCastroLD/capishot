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
- Events (`onEvent`): `ready`, `documents`, `documentChanged`, `dirty`, `selectionChanged`,
  `toolChanged`, `modeChanged`, `zoom`, `viewChanged`, `error`. Derived once per frame, after the
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

- Type layers through `execute` (`docs/control-protocol.md`): `type.frame {layer}` gives a layer's
  frame without its layout (`shape` point/box/path/inPath with its geometry, `transform`, `bounds`),
  and `type.edit {layer, path | inPath, vertical?, inset?}` gives type in a path a new path (document
  pixels: `type.frame`'s `documentPath`, a vertex moved), keeping the layer id, in one undo step
  (features `type.frame`, `type.edit.path`).

- `execute('type.createInBubble', {x, y, style?, styles?})` puts type at a document point as the
  Bubble Type tool does: in the speech bubble or narration box there, else floating text
  (`style` in the result: `bubble`, `narration` or `floating`).

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
  - `request('fonts.preload', {families, timeoutMs?})` fetches families ahead and resolves once
    each has arrived or failed: `{ready, missing, failed}` (feature `fonts.preload`).
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
