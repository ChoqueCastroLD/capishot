# Code map

Where things live, to find them fast. See `docs/architecture.md` for why.

## Flow

Input → `crates/ui-egui` (tools and panels call engine commands) → `crates/engine` (commands, documents, history, jobs) → pixels (`raster`, `paint`, `algo`, `compose`, `gpu`) → the canvas (`ui-egui/src/canvas`).

## Crates

| Crate | What's there |
|---|---|
| `ui-egui` | The whole interface (largest: about 200k lines) |
| `engine` | `Session`, commands in `src/cmd/*_cmds*`, `jobs.rs` (background and stepped jobs), prefs, doc events |
| `algo`, `paint`, `raster`, `compose`, `gpu` | Filters; brushes; tiles and surfaces; CPU compositor; GPU helpers |
| `psd`, `io`, `format`, `codecs`, `raw`, `affinity`, `heif` | File formats |
| `text`, `typer` | Type layout; TypeR, bubble finding (`typer/src/bubble_find.rs`) |
| `ai`, `ai-lama`, `ai-ort`, `ai-web` | Upscalers; Manga LaMa and tiles (`ai-lama/src/tiles.rs`); ONNX Runtime; browser AI bridge |
| `host` | `capishot-host`: Capishot as a library for host apps (`src/view/*`: the widget) |
| `automation` | CLI session and MCP server |

## Apps

| App | What's there |
|---|---|
| `apps/photocraft` | Desktop: `main.rs`, `update_check.rs`, `app_dirs.rs` (config folder) |
| `apps/photocraft-web` | Browser: `web.rs` (start), `embed*.rs` (embed API), `usage_stats.rs` + `breadcrumb.rs` (statistics, crash signal), `recovery.rs`, `watchdog.rs` |
| `apps/photocraft-ai-worker` | The browser's AI Web Worker |
| `apps/photocraft-cli` | `capishot-cli` |

## `crates/ui-egui/src`

| To change | Look in |
|---|---|
| A frame, what runs when | `app/frame.rs`, `chrome_freeze.rs` (repaints the chrome during a drag) |
| Canvas drawing and pointer | `canvas.rs`, `canvas/tool_input.rs`, `canvas/press_watch.rs`, `canvas/gpu_canvas/` |
| A tool's behaviour | `tools/<tool>_ui*`, `tools/remove_mask_ui.rs`, `remove_ai_ui.rs`, `tools/bubble_text_ui.rs`, `tools/tool_options` in `state/tool_options.rs` |
| Tool letters and held keys | `input/tool_shortcuts.rs`, `input/hold_keys.rs`, `input/spring_tools.rs`, `input/shortcuts.rs` |
| Menus | `menus/items.rs` (tree), `menus/invoke.rs` (actions), `menu_catalog.rs` |
| Phone UI | `simple_ui.rs`, `touch_ui.rs`, `touch_locks.rs`, `mobile_sheet.rs`, `menus/mobile_menu.rs` |
| Panels and docking | `panels/`, `panels/dock.rs`, `panels/panel_docking.rs` (custom workspace), `dock/` |
| Dialogs | `dialogs/`, `prefs/prefs_ui/` |
| Themes | `theme.rs`, `theme/css.rs`, `theme/builtin/*.theme.css`, `theme/pattern.rs`, `profile_ui.rs` |
| Opening files, progress | `file_open/`, `jobs_ui.rs`, `ai_progress.rs` |
| Strings | `i18n/*.tsv` |
| Statistics | `telemetry.rs` (no file names or content, ever) |

## Building and testing

| What | Where |
|---|---|
| Web release | `packaging/web-release.sh` (default fat LTO; `--test-build <threads\|st>` for a fast test build) |
| Desktop release | `packaging/release-desktop.sh`, `packaging/windows/`, `packaging/linux/` |
| Browser tests | `packaging/web-threads/e2e/*.mjs` |
| Host contract | `examples/host-egui` (its own workspace) |
| Protocol and embed docs | `docs/control-protocol.md`, `apps/photocraft-web/embed/README.md`, `crates/host/README.md` |
