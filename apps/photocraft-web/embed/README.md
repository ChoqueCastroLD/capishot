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

- Type layers through `execute` (`docs/control-protocol.md`): `type.frame {layer}` gives a layer's
  frame without its layout (`shape` point/box/path/inPath with its geometry, `transform`, `bounds`),
  and `type.edit {layer, path | inPath, vertical?, inset?}` gives type in a path a new path (document
  pixels: `type.frame`'s `documentPath`, a vertex moved), keeping the layer id, in one undo step
  (features `type.frame`, `type.edit.path`).

- `execute('type.createInBubble', {x, y, inset?})` puts type in the speech bubble at a document
  point (the Bubble Type tool): `{layer, bounds, path}`, or an error starting `noBubble`.

## Versioning rules

`EMBED_API_VERSION` (in `embed_api_core.rs`) is the contract hosts check. Adding a function, an
optional parameter, an event, a result field or a `features` string keeps it; renaming, removing
or changing the shape of any of them bumps it, and the host then needs a new adapter. Keep
`capishot-embed.d.ts` in step with every change.

## Building and publishing

```sh
scripts/build-embed.sh        # trunk release build of HEAD → dist/embed/<hash>/ (or $CAPISHOT_EMBED_BUILDS/<hash>/)
```

The directory holds `capishot-embed.js`, `capishot-embed_bg.wasm`, `capishot-embed.d.ts` and
`manifest.json` (embed API version, commit, build hash, sha256 and size of every file). The commit
and build hash are compiled in (`CAPISHOT_COMMIT`, `CAPISHOT_BUILD_HASH`) and reported by
`version()`, so a host can refuse a build that isn't the one it pinned.
