# capishot-host changelog

Every change to the stable API is listed here with the commit to pin. Breaking changes are under
**Breaking** and bump `API_VERSION`. Experimental items (`live`, `Host::session`,
`Host::session_mut`, `unstable`, `view::CapishotView::app`/`app_mut`/`with_services`/`control`)
may change in any commit; their changes are listed under **Experimental**. A change to the
`[patch]` pins (README.md) is listed too: copy the blocks again when you move the pin.

## API_VERSION 1, behaviour (no frame mixes states of a page)

- No API change. The canvas never shows part of what the view shows at an older state of the
  document than the rest: only a page shown for the first time fills from blank in pieces
  (request 19); an edit, a Move drag frame, a pan into a region an edit left for later, a kept
  view restored (`apply_view` after `prewarm_view`, request 17/18: what the prewarm had not
  drawn yet is drawn in that frame) and a page shown again after its canvas was let go are drawn
  whole in their frame.
- A Move drag of a Smart Object, a type layer or a plain group draws it from its pixels already
  on the GPU at the offset each frame (no copy per frame).
- A Move drag keeps the layers above the moving ones composited for the drag (over black and
  over white, at the view's resolution, within the canvas budget) and lays them over each frame,
  where they blend affinely over an opaque Background; the frame of a Move pointer event shows the
  layers at that event (no frame behind the pointer).

## API_VERSION 1, additions (opening big pages off the frame, Capikoma request 19)

- `Host::start_open_rgba(width, height, rgba: Vec<u8>, opts)`: `open_rgba` as a job (tightly
  packed rows, moved in), with `start_open`'s result.
- `Host::open_snapshot(&Snapshot, opts) -> Opened`: a document built by another `Host` (another
  thread) opened here without encoding; pixels shared until either side edits them.
- `Host::top_layers(document) -> Vec<TopLayer {id, name, kind, visible, children}>`: the top
  level only. `DocumentInfo::revision` (in `documents()`) already tells whether anything changed.
- The first frame of a document shown (or prewarmed) on the CPU compositor draws what the view
  shows in pieces for about 24 ms and the rest over the next frames, instead of the whole page in
  one frame; `document.prewarm` reports ready once the view's part is drawn.

## API_VERSION 1, additions (named view switches in one frame, Capikoma request 17)

- `Host::prewarm_view(document, name)` and `CapishotView::prewarm_view(document, name)` (returns
  the token of its `ViewEvent::Reply`: `{id, name, ready, frames, ms, reason?}`): draw a named view
  ahead, a slice per frame, so `Host::apply_view` (`document.views.apply`) then shows it in one
  frame. `ViewHooks::prewarm_view` has a default (does nothing): hosts with their own hooks keep
  compiling.
- Without asking, the GPU canvas keeps the canvas of a view it showed before (a GPU copy, within
  the canvas budget and `set_memory_budget`, at most 4 per document, dropped by any other edit):
  switching back to it takes one frame instead of redrawing the changed area.

## API_VERSION 1, additions (Move tool Auto-Select)

- `view::MoveTarget::{Layer, Group}` (`as_str`), `ViewOptions::{move_target, move_auto_select}`: the
  Move tool's Auto-Select from the host, applied when they change; `None` (default) leaves them as
  the editor has them. Build `ViewOptions` with `..Default::default()`.
- `CapishotView::control("ui.set", ..)` (experimental) takes `moveTarget` (`"layer"` or `"group"`)
  and `moveAutoSelect` (bool); `ui.inspect` reports them under `toolOptions`.

## API_VERSION 1, fixes (panes on one device)

- `CapishotView::set_camera` and `set_show_camera`'s `DocumentCamera::{At, Keep}` after `Host::show`
  (or `Host::close`, `Host::open`) moved another document's view: the view now catches up with the
  session as it is lent, before any queued request runs. A workaround that calls
  `embed::set_camera` before `Host::show` keeps working and can go.
- Several `CapishotView`s on one wgpu device no longer blank each other: each canvas has its own GPU
  resources, freed when the view drops. See README › Several editors on one device.
- **Experimental**: `PhotocraftApp::{gpu_canvas, sync_views_if_moved}`; `embed::camera` and
  `embed::set_camera` read the index as the session's document even before the frame's sync.

## API_VERSION 1, additions (tiles)

- `CapishotView::ui_tiles` and `CapishotEditor::ui_tiles`, `view::{Tile, TileOutput, TilesOutput,
  TileCamera, MAX_TILES}`: one canvas per rect, the active document's first plain tile live.
- `Tile::with_view`: one document as several named views, each composited on its own; the layers'
  visibility, the revision and the history stay as they are. Each named view holds a display texture
  (README › Tiles); `editor.setMemoryBudget` counts them.
- `ViewOptions::tile_camera` (`TileCamera::Shared` by default): build `ViewOptions` with
  `..Default::default()`.
- **Experimental**: `embed::{tiles_frame, tiles_rect, EmbedTile, TileDrawn, TileCamera, MAX_TILES}`,
  `photocraft_engine::doc_views_cmds::{view_document, view_signature}`.

## API_VERSION 1, additions (font aliases)

- `fonts::FontFile {name, bytes, aliases}` (`new`, `with_aliases`, `From<(String, Vec<u8>)>`,
  `From<(&str, Vec<u8>)>`), `fonts::register_file`, `fonts::{MAX_FONT_ALIASES,
  MAX_FONT_ALIAS_CHARS}`: a font file answers to other names, until it is unregistered.
- `Host::add_fonts` and `Host::add_document_fonts` take `Vec<impl Into<FontFile>>`: `(name, bytes)`
  calls compile as they are; an untyped empty `vec![]` needs a type.
- `RegisteredFont::aliases`. `fonts_ready`, `request_missing_fonts`, `Event::FontsReady` and
  `Opened::missing_fonts` count an alias as a family; so does a PostScript name given as an alias.

## API_VERSION 1, additions (full mode)

- `view::Mode::{Embedded, Full}`, `CapishotView::{set_mode, mode}`, `CapishotEditor::{set_mode,
  mode}`, `ViewOptions::{mode, full_theme}`, `view::FullTheme`: Capishot's whole interface in the
  widget's rect, on the same open documents. Feature `full` (default).
- `ViewEvent::{Window, FileDialog, WriteFile, OpenUrl, DocumentReady}`, `view::{WindowRequest,
  FileDialog, FileRequest}`. `ViewOptions` gained fields: build it with `..Default::default()`.
- `Keys::{deny, allow_only, filter, routing}` and `Shortcut` (with `Shortcut::opacity_digits`).
- `CameraChange::ZoomBy`; `CapishotView::raw_input_hook` (optional, pinch and touch).
- `Host::{open_rgba, put_rgba, add_rgba}`, `RgbaImage`, `PutRgba`, `AddRgba`: raw pixels, no PNG or base64.
- `Host::add_document_fonts`, `Event::FontsReady`.
- Features `full`, `camera-raw`, `exr`, `heif`, `all-catalogs`.
- `ViewOptions::rulers` applies when it changes, no longer every frame: the person's View ›
  Rulers stays.
- Native `ai-native`: a request with `allowDownload` now fetches missing weights from
  `AiConfig::models_url` (it failed with "weights not found" without trying).

## API_VERSION 1 (first release)

- `Host`: `execute`, `execute_with`, `execute_request`, `commands`; jobs (`start`, `job`, `jobs`,
  `cancel`, native `wait`); `poll`, `subscribe`, `unsubscribe`, `events`; documents (`documents`,
  `active_document`, `open`, `start_open`, `show`, `prewarm`, `close`, `save`, `save_psd`,
  `start_save`, `take_saved`, `inspect`); pixels (`get_pixels`, `put_pixels`); `batch`, `undo`,
  `redo`, `begin_group`, `end_group`, `group`, `in_group`; views (`views`, `define_view`,
  `apply_view`); tools (`tool`, `set_tool`); `set_view_hooks` / `ViewHooks`.
- Files: `OpenOptions`, `Opened`, `SaveFormat`, `SaveOptions`, `Saved`, `files::MAX_FILE_BYTES` (`u64`).
- Rendering: `Host::render`, `Host::snapshot`, `render::{RenderOptions, Rgba, Snapshot}`.
- Fonts: `Host::{add_fonts, fonts_ready, request_missing_fonts, pending_font_layers,
  refresh_fonts, layout_digest}`; `fonts::{register, unregister, check, families,
  family_status, use_system_fonts, FontProvider}`.
- AI: `Host::{install_ai, uninstall_ai, ai_availability, prepare_remove_ai, ai_model_status,
  download_ai_model}`; `ai::{AiConfig, AiDevice, AiAvailability, Readiness, RemoveAi, InpaintAi,
  RemoveBubbleText, UpscaleAi, AiReply}` and the command id constants. Features `ai-native`,
  `ai-web`.
- Widget (feature `view`, default): `view::{CapishotView, CapishotEditor, ViewOptions,
  ViewOutput, ViewEvent, CanvasTransform, Overlay, HostTheme, DocumentCamera}`.
- `Event` (serialises as the web embed's events), `Origin`, `JobEnd`, `Error`, ids
  (`DocumentId`, `LayerId`, `LayerUid`, `JobHandle`) and the option/result structs.
- **Experimental**: `live` (`Op`, `Envelope`, `Applied`; `Host::live_start`, `live_stop`,
  `is_live`, `live_apply`, `live_take_local`, `layer_uid`).
- Pins: egui `storytold/egui@6d518fc`, winit `storytold/winit@a517b38`, `egui-wgpu` from this
  repository's `vendor/egui-wgpu`.
