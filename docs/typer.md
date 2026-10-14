# TypeR in Capishot

TypeR is the Photoshop panel most manga typesetters use: paste a translated chapter, then fill bubbles
line by line with styled text. Capishot ports it natively. The full plan (feature inventory, specs,
work packages) is `capishot-typer-plan.md`, kept outside the repo; this page is the map of what is in
the tree.

Status (2026-10-14): **WP0 and WP3 done.** The script, cursor, page files, style library, TypeR
JSON exchange and settings work, as commands, and the TypeR panel (Window › TypeR) drives them.
Placement, bubble detection and line shaping are registered stubs for WP1, WP2-B and WP4: the
panel's Paste, Align, Apply and size buttons show why they can't run yet.

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
crates/typer (photocraft-typer, L2: doc, color, serde; no egui, no I/O, wasm-clean)
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
  payload.rs   scaled_style (S14), resolve_point_text (L6), selected_layers (P9), stored_selections (M2)
  bubble.rs    Rect, Row, BubbleHit, StoredSelection: data only, filled by WP1/WP4
crates/engine/src
  typer_state.rs        TyperSession { state, stored_selections } = Session::typer; to_json/from_json
  typer_cmds.rs         session, library and settings commands (working)
  typer_place_cmds.rs   placement and multi-bubble commands (stubs, WP1)
  typer_query_cmds.rs   detectBubble, selectionBounds (stubs, WP1), shapeText (stub, WP4)
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
| `typer.paste`, `typer.pasteMany`, `typer.apply`, `typer.applyLines`, `typer.align`, `typer.resize`, `typer.applyShape`, `typer.toggleCleaningLayers` | stub (WP1) | `enabled` reports "not implemented yet"; `journal: true` |
| `typer.multiBubble.toggle` / `.capture` / `.removeLast` / `.clear` | stub (WP1) | stored selections go in `TyperSession::stored_selections` |
| `typer.style.fromLayer` | stub (WP1/WP2) | use `psjson::text_props_from` |
| `typer.detectBubble`, `typer.selectionBounds` | stub (WP1) | queries, `journal: false` |
| `typer.shapeText` | stub (WP4) | |

Menu: `Type › TypeR` holds Next/Previous Line, Next/Previous Page, Next/Previous Style and Next
Size Preset, with no shortcuts. Everything else has no menu item until the panel exists.

## What the next work packages plug into

- **WP1 (placement, bubbles).** Fill `typer_place_cmds.rs` and `typer_query_cmds.rs`. Replace
  `enabled: wp1` with real checks. Put the pure mask code in `crates/typer/src/bubble.rs` and add
  `photocraft-algo` / `photocraft-text` to `crates/typer/Cargo.toml` (write it atomically).
  - Text and style for a paste: `state.current_line()` plus `state.current_style()`, mapped with
    `psjson::map_style(style, &MapOptions { dpi, text_scale, page_width, direction })`.
  - Markdown runs: `markdown::parse_runs` when `settings.interpret_markdown`.
  - Layer kind: `payload::resolve_point_text`. Stroke: `Mapped::stroke` → `layer.layerStyle.stroke`.
  - After success, call `state.next_line(true)`.
  - Multi-target fills: `payload::stored_selections` / `payload::selected_layers`.
  - Each placement must be one `Session::edit`. Return TypeR's errors (`noSelection`,
    `smallSelection`, …) as `BadParams` with a clear message.
- **WP2 (script/library, phase B).**
  - Persist `TyperSession::to_json()` with atomic writes through the platform services. Settings
    can also mirror into prefs `typer.*`.
  - Phase B: tabs (N6), `usedLineStyles`-based jump (N7), `.tpl` import (K6), FontScanR (F3),
    profiles (K5), the N4 page opening (`file.open` + `document.activate` on line change, using
    `State::current_image`) and auto-close with a prompt (N5).
  - Paths go through the automation policy: a params doc mentioning "path" needs a rule in
    `crates/automation/src/workspace.rs`.
- **WP3 (panel, shortcuts, i18n): done.** See [The panel](#the-panel) below.
- **WP4 (hyphenation, TextShapeR, harness).** Use `typer.shapeText` and `bubble::Row`. The
  real-page harness goes in `crates/typer/tests/real_pages.rs`, `#[ignore]`, gated by
  `CAPISHOT_TYPER_PAGES`.

## The panel

`crates/ui-egui/src/typer/` (Window › TypeR, its own right-dock group `Group::TypeR`, above
Navigator/History/Layers; not in the default workspace). It reads `Session::typer` to draw and
changes it only through `typer.*` commands; its view state is `UiState::typer` (`TyperUi`:
dialogs, collapsed folders, splitter).

| File | What |
|---|---|
| `mod.rs` | the dock body (preview, script, splitter, styles, footer), the panel commands `typer.ui.*`, the confirmation dialog, workspace persistence |
| `preview.rs` | Paste (multi-bubble count), Align, size −/+ with the step field, keep-size badge (Alt held), multi-bubble selection count and −, ↑/↓, line/page/style, text scale %, apply arrow, the line in its style's font (click: scroll the script to it) |
| `script_editor.rs` | `TextEdit` with a layouter (page markers, ignored lines, tag chips in the tag colour, ignore tags and Markdown markers dimmed), gutter with numbers / `P<n>` / current-line marker; a gutter click makes a line current (a marker or note: the next usable line); ⌘B / ⌘I and the B / I buttons toggle Markdown (`claim_format_keys` takes them before the menus); Import `.txt`/`.md` |
| `styles.rs` | Unsorted, then the folder tree (collapsible, bold when it holds the current style); rows with colour swatch (stroke outline), tag chip, the name in its font, size or preset chips, quick −/+ on hover, star for the default style; right-click: edit, apply style, copy / paste / duplicate, tags on/off, default style, delete; folder menu: add style, paste, edit, subfolder, duplicate, export, delete (with or without styles) |
| `style_editor.rs` | name, folder, tags, tag colour (TypeR's 8 + custom), layer kind (`textType`), font family (installed families listed) and style, size + unit, leading, tracking, H/V scale, baseline shift, kerning, colour, faux bold/italic, caps, underline, strike, ligatures, super/subscript, diacritics (Middle Eastern mode), alignment, hyphenate, hanging punctuation, anti-aliasing, stroke, up to three size presets with default and minimum page widths (auto size by page width), Copy layer style (`psjson::text_props_from` of the active text layer) |
| `folder_editor.rs` | name, parent (not itself or below), member styles, delete |
| `settings.rs` | General / Text / Behavior (a draft saved with one `typer.settings.set {values}`), Shortcuts (the keymap toggle and table), Data (import with "keep local styles?" on a full import, export all with or without settings, export one folder) |
| `keymap.rs` | TypeR's shortcut table and the optional TypeR keymap |
| `font_label.rs` | style names and the preview line rendered by the text engine in the style's font (cached textures), plain text when the family isn't available |

**Shortcuts.** Photoshop's keys always win; no `typer.*` command has a default. Inside the script
editor only, ⌘B / ⌘I toggle Markdown. The TypeR keymap (Edit › Keyboard Shortcuts checkbox, or
Settings › Shortcuts; off by default) sets these overrides, and turning it off removes the ones
still at these keys:

| Action | TypeR | TypeR keymap | Why |
|---|---|---|---|
| Paste (`typer.ui.paste`: Paste, Paste Many in multi-bubble mode, Apply Lines with ≥2 text layers and no selection) | Win+Ctrl | Ctrl+Alt+B | modifier-only chords aren't supported (and the OS/browser takes Super) |
| Apply (`typer.apply`) | Win+Shift | Ctrl+Alt+J | same |
| Align (`typer.align`) | Win+Alt | Ctrl+Alt+X | same |
| Paste text only (`typer.ui.insertText`) | Win+Shift+V | Ctrl+Alt+Shift+J | Ctrl+Alt+Shift+V is Paste Into |
| Next line | Ctrl+Enter | Ctrl+Alt+Enter | Ctrl+Enter commits type / loads a path as selection |
| Previous line | Ctrl+Tab | Ctrl+Alt+Shift+Enter | Ctrl+Tab is Next Document |
| Next page | Shift+X | Shift+X | free |
| Text size +/− (`typer.ui.increaseSize` / `decreaseSize`) | Ctrl+Shift+Plus/Minus | Ctrl+Alt+. / Ctrl+Alt+, | Ctrl+Shift+Plus zooms (`=` matches `+`) |
| Multi-bubble mode | Ctrl+Alt+M | Ctrl+Alt+M | free |
| Toggle cleaning layers | Ctrl+H | Ctrl+Alt+Shift+H | Ctrl+H is Extras |

(`Ctrl` is ⌘ on macOS, except `Ctrl+Alt+M`, which is the Control key everywhere, as in TypeR.)
Apply Style, Apply Lines, Remove Last / Clear Bubbles and Next Size Preset are listed under
Type › TypeR in Edit › Keyboard Shortcuts with no key. `typer/tests.rs` checks that the preset is
off by default, that no preset key is in Photoshop's table (`shortcut_parity/photoshop_defaults.tsv`)
or pressed by any Capishot default, and that it applies and reverts.

**Persistence.** `typer::persist` restores `TyperSession::to_json` (library, settings, script,
cursor, page files) on the first frame, into a session that hasn't used TypeR yet, and saves it
about once a second while it changes (only a hash is kept between checks), through
`Services::load_typer` / `save_typer`: the browser's localStorage (`capishot.typer.workspace`,
beside the preferences) on the web, `<config>/TypeR/workspace.json` (atomic write) on the
desktop. `edited` stamps come from `Services::clock` (the browser's `Date.now`) as the commands'
`now` param, since the engine has no clock in wasm.

**Not yet:** drag-to-reorder styles and folders, the page-file picker and page sync (N4, WP2-B),
the multi-bubble canvas overlay and the Align preview (need WP1's capture and detection), tabs
(N6), TextShapeR strip (WP4), UI order/visibility toggles.

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
