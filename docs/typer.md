# TypeR in Capishot

TypeR is the Photoshop panel most manga typesetters use: paste a translated chapter, then fill bubbles
line by line with styled text. Capishot ports it natively. The full plan (feature inventory, specs,
work packages) is `capishot-typer-plan.md`, kept outside the repo; this page is the map of what is in
the tree.

Status (2026-10-14): **WP0 and WP1 done.** The script, cursor, page files, style library, TypeR
JSON exchange and settings work, as commands, and so does the core typesetting loop: paste, apply,
align, resize, multi-bubble, bubble detection. The panel (WP3) and line shaping (WP4) are still to
come; `typer.shapeText` is a registered stub.

## Decisions

- **Photoshop shortcuts always win.** `typer.*` commands have no default shortcut. The optional
  "TypeR keymap" preset (WP3) is off by default. It never takes `Ctrl+Tab` or `Ctrl+H`.
- **UI languages: en, es, pt.** TypeR strings are translated into `es`, `pt-br` and `pt-pt`. The
  other catalogs fall back to English through `TYPER_LANGUAGES` in
  `crates/ui-egui/src/i18n/mod.rs`, which also scopes `tl!` strings under `src/typer/`.
- **Real-page tests:** a few local pages only, gated by `CAPISHOT_TYPER_PAGES`, never committed.
- **Licence:** TypeR is MIT (© 2017 Swirt). Each ported file carries a header naming its TypeR
  source. **Never port** `app_src/lib/jam/*.jsxinc` (GPL), `CSInterface.js` (Adobe) or TypeR's
  icons and logo. `psjson.rs` is written from the style JSON as data, not from jamText. See
  `NOTICE`, `ATTRIBUTION.md` and `crates/typer/LICENSE-TypeR.txt`.

## Architecture

```text
crates/typer (photocraft-typer, L2: doc, color, geom, algo, serde; no egui, no I/O, wasm-clean)
  id.rs        Id (string or number, TypeR's validId), num() (whole numbers written as 23, not 23.0)
  pages.rs     parse_page_marker (3.0 strict rule), image_page_number / image_for_page (N4), natural_cmp
  script.rs    Line, parse(text, ParseContext) (S1–S8), line numbering (S3), page_of, next_usable
  markdown.rs  parse_runs (S9), format_selection (Ctrl+B/I toggle, S11), escape
  library.rs   Style (TypeR JSON, whitelisted keys), Folder, Library: normalize, tree, tag_styles
               (folder isolation S5), CRUD (L1–L4), default_text_props / default_stroke
  presets.rs   size presets and auto size by page width (L5)
  settings.rs  Settings (§2.10, TypeR 3.0 defaults, TypeR key names), set/merge with coercions
  state.rs     State: script + cursor + current style + used line styles + page files + library +
               settings. Navigation (N1, N3), style stepping (L8), refresh() = TypeR's reducer phases
  exchange.rs  validate (3.0 validator), classify/import (folder / library / full merge by `edited`),
               export (K2) and export_folder (K3), with "schema": 1
  psjson.rs    TypeR textProps ↔ CharStyle/ParagraphStyle (§2.7): map_style (px→pt with the document
               dpi at apply time, text scale, auto size), stroke, text_props_from (reverse)
  payload.rs   scaled_style (S14), resolve_point_text (L6), selected_layers (P9), stored_selections (M2),
               stored_selection_line (M3)
  bubble.rs    Mask, wand (D1), fill_holes, opened_bounds (P7, TypeR 3.0 adaptive opening), rows (D3),
               Judge (is this region a bubble?), BubbleHit, StoredSelection
  place.rs     box_size, fit_height, fit_width, center_offset, px_to_pt (placement arithmetic)
crates/engine/src
  typer_state.rs        TyperSession { state, stored_selections } = Session::typer; to_json/from_json
  typer_cmds.rs         session, library and settings commands (working)
  typer_place_cmds.rs   placement, multi-bubble, cleaning toggle, style from layer (WP1)
    support.rs          page image cache, selection mask, style → type, measure/fit/centre by ink
  typer_query_cmds.rs   detectBubble, selectionBounds (WP1), shapeText (stub, WP4)
```

**State lives in the engine** (`Session::typer`), not in the UI, so the CLI, actions and MCP agents
drive the same loop. It is session state: it is not saved in `.pcraft` and is not part of undo.
Undo reverts only layer changes, and the cursor keeps its place, as in TypeR.
`TyperSession::to_json`/`from_json` give the workspace file WP2 persists (§3.7: library, settings,
script, cursor, page files). Nothing is written to disk yet.

**Library format.** Styles keep TypeR's JSON: field names and `textProps` (Photoshop
ActionDescriptor stringIDs). They are limited to TypeR's `serializeStyle` whitelist plus a
`capishot` side object. A TypeR 2.5 or 3.0 export imports and re-exports unchanged, modulo key order
(tested). Sizes stay in the style's `typeUnit` and are converted with the document dpi only when a
style is applied (`psjson::map_style`).

**Refresh rule.** Every `State` mutator re-derives the lines, then:
1. puts the cursor on a usable line;
2. on a line change, selects the line's tag style, else the default style;
3. falls back to the first style when the current one is unknown;
4. re-matches tags if the selected style's folder changed. TypeR leaves that re-match stale until
   the next action.

## Commands

Results of the navigation commands carry `summary()`: `currentLine, line, number, text, style,
styleName, page, image, usableLines, lineCount, textScale` (plus `moved`).

| Command | Status | Notes |
|---|---|---|
| `typer.state` | works | `{lines:true}` adds every parsed line |
| `typer.script.set` | works | `\r\n` normalized, 8 MB cap |
| `typer.parseScript` | works | query; `{text}` parses without changing the script |
| `typer.line.set` / `.next` / `.previous` | works | `next {add:true}` = after a paste: records `usedLineStyles`, stops at the last line of a synced page |
| `typer.page.next` / `.previous` | works | strict `Page N` markers |
| `typer.images.set` / `.clear` | works | file names, natural sort, page → file by last digit run, else position |
| `typer.style.list` / `.select` / `.next` / `.previous` | works | `select {index}` = numpad, within the current folder |
| `typer.style.nextSizePreset` / `.setSizePreset` / `.setSize` | works | L5; `now` param stamps `edited` (wasm has no clock in the engine) |
| `typer.style.resolve` | works | the Capishot CharStyle/ParagraphStyle a style applies at the active document's dpi and width |
| `typer.style.save` / `.delete` / `.duplicate` / `.togglePrefixes` / `.reorder` | works | save merges into the existing style and validates |
| `typer.folder.save` / `.delete` / `.duplicate` / `.reorder` | works | refuses parent cycles; `delete {permanent}` = Shift+Delete |
| `typer.library.import` / `.export` / `.exportFolder` | works | JSON text or object in, JSON out (no file paths: the UI owns files) |
| `typer.settings.get` / `.set` | works | TypeR key names; `set` is all-or-nothing |
| `typer.paste` | works | P1+P7, one step "TypeR Paste". Without `text`/`bounds`/`style` it is TypeR's Paste key: `pasteMany` in multi-bubble mode with stored bubbles, `applyLines` with 2+ type layers selected and no selection |
| `typer.pasteMany` | works | M2, one step "TypeR Multiple Paste"; stored bubbles (raw box, or opened with `openStoredSelections`) or explicit `entries` |
| `typer.apply` / `typer.applyStyle` | works | P2/P3/P4 (`mode` textAndStyle/textOnly/styleOnly), selected layers, keep size; one step "TypeR Change" |
| `typer.applyLines` | works | P9: click order, all targets validated first, nothing changes on error |
| `typer.align` | works | P5+D1+P7, multi-layer, `resizeBox`; deselects; one step "TypeR Align" |
| `typer.resize` | works | P6: every run's size and fixed leading ± delta (px or pt), box scaled, centre kept |
| `typer.applyShape` | works | D4 apply: lines joined, box shrunk to the text, centre kept |
| `typer.toggleCleaningLayers` | works | P10, per document; no default shortcut (Ctrl+H is Photoshop's) |
| `typer.multiBubble.toggle` / `.capture` / `.removeLast` / `.clear` | works | session state (`journal: false`); capture stores raw and opened bounds with the line and style, then advances; removeLast restores the line (M3) |
| `typer.style.fromLayer` | works | L4 "Copy layer style": new style, or `id` to update one |
| `typer.detectBubble`, `typer.selectionBounds` | works | queries: bbox, opened, centre, rows (D3), `found` + `reason` |
| `typer.shapeText` | stub (WP4) | |

Menu: `Type › TypeR` holds Next/Previous Line, Next/Previous Page, Next/Previous Style and Next
Size Preset, with no shortcuts. Everything else has no menu item until the panel exists.

## What the next work packages plug into

- **WP1 (placement, bubbles): done.** How it works:
  - **Bubbles** are found on the page *without its type layers*, composited once and cached until
    a non-type layer's pixels change (`support::page_image`; placing text never invalidates it).
    The wand is TypeR's (tolerance 20, contiguous, anti-aliased, seeded 5 px left of the text),
    holes are filled (the text inside), then TypeR 3.0's adaptive opening removes the tail
    (`r = min(max(4, round(0.1·min side)), floor(min side/2 − 1))`, halved until the opened box is
    ≥ 200 px²), all as pure mask functions: the selection and history are never touched.
  - **Not a bubble** (`bubble::Judge`, Capishot's addition: TypeR centres in any region): a wand
    region is refused when it fills < 35 % of its box, is < 16 px across, has a dark seed
    (luma < 128), touches two page edges, holds < 60 % of the text's ink, or is > 60× the text
    (counted as at least 4 em square). The user's own selection is always taken as is.
  - **Text** is centred by its rendered ink, moved by whole pixels. Box text is measured in an
    oversized box (no clipped lines), then its height becomes `extent + size + 2`; paste boxes
    are `0.9 × opened width − 2 × padding` (≥ 10). Styles map through `psjson::map_style` at the
    document's dpi with the text scale; Markdown becomes faux bold/italic runs; the stroke is a
    layer-style Stroke (outside), replacing only an existing stroke.
  - **History:** one `Session::edit` per action; on error nothing is committed. The cursor
    advances only after success and only when the text came from the script.
  - Real pages (local, `CAPISHOT_TYPER_PAGES`): `typer_place_cmds::tests::typer_real_pages`.
- **WP2 (script/library, phase B).**
  - Persist `TyperSession::to_json()` with atomic writes through the platform services. Settings
    can also mirror into prefs `typer.*`.
  - Phase B: tabs (N6), `usedLineStyles`-based jump (N7), `.tpl` import (K6), FontScanR (F3),
    profiles (K5), the N4 page opening (`file.open` + `document.activate` on line change, using
    `State::current_image`) and auto-close with a prompt (N5).
  - Paths go through the automation policy: a params doc mentioning "path" needs a rule in
    `crates/automation/src/workspace.rs`.
- **WP3 (panel, shortcuts, i18n).** Build `crates/ui-egui/src/typer/` against these commands.
  - The script editor's highlighting comes from `typer.parseScript` (`ignorePrefix`,
    `stylePrefix`, `page`, `index`) and `markdown::format_selection`. Tag chips use
    `Style::prefix_color()`.
  - The modifier-only chords and the optional TypeR keymap are off by default.
  - Translate new strings into es, pt-br and pt-pt only (`TYPER_LANGUAGES`).
- **WP4 (hyphenation, TextShapeR, harness).** Use `typer.shapeText`, `bubble::rows` /
  `typer.detectBubble {layer}` (rows of the hole-filled bubble) and `typer.applyShape` (already
  works: lines in, box shrunk, centre kept). The engine-side real-page check is
  `typer_place_cmds::tests::typer_real_pages` (`#[ignore]`, `CAPISHOT_TYPER_PAGES`); the full
  harness (§3.8 #4 c/d) goes in `crates/typer/tests/real_pages.rs`. Local page 15 has no speech
  bubbles (narration and SFX over art), so it measures the "no bubble" rejection, not centring:
  a chapter page with bubbles plus a labels file is needed for the centring floors.

## Tests

`cargo test -p photocraft-typer` covers the ports of TypeR's acceptance scripts:
- `testWorkspaceNavigation` (marker accept/reject list, reducer)
- `testLineNumbering`
- `testFolderTagIsolation`
- `testMarkdownFormatting`
- `testLibraryRoundTrip` (serializer, validator rejections, cycle repair, descendants)
- `testStyleSizePresets`
- `testTextLayerPayload`
- `testPageImageMapping`
- the style half of `testNumpadStyleShortcuts`

It also covers 2.5 behaviours: first-match tags, `//` and `//:`, ignore tags, the end-of-page lock,
the merge by `edited`, 2.5 and 3.0 export round trips, and the psjson mapping and its reverse.

Engine tests (`typer_cmds/tests.rs`) cover the CLI-style loop, page files, CRUD, import/export
through commands, settings, stub refusals and bad params for every `typer.*` command. `panic_hunt`
covers the new commands too.

Placement tests (`typer_place_cmds/tests.rs`) run on synthetic manga pages (white ellipse bubbles
with a black outline, some with tails, on screentone): paste/align land within 1 px of the bubble
body's centre (the raw box with the tail is > 15 px off), rectangle selections centre exactly,
tiny ones are `smallSelection`, box width/height rules, point text, apply keeps the centre,
resize adds the delta to fixed leading, multi paste is one step, applyLines rolls back on a
non-type target, keep size, text scale at 300 dpi, the cleaning toggle, style from layer, the page
cache, and bad params. `typer_place_perf` (ignored) times a 2000×3000 page.
