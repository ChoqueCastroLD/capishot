/**
 * Capishot embedding API, version 1 (`embedApiVersion() === 1`).
 *
 * The module is Capishot's web build (wasm-bindgen ES module). Loading it runs nothing on a page
 * without Capishot's own `<canvas id="photocraft_canvas">`: the host calls `init()`.
 *
 * ```ts
 * import load, * as capishot from '/studio/capishot/<hash>/capishot-embed.js';
 * await load({ module_or_path: '/studio/capishot/<hash>/capishot-embed_bg.wasm' });
 * if (capishot.embedApiVersion() !== 1) throw new Error('unsupported Capishot build');
 * await capishot.init({ container, mode: 'embedded', locale: 'es' });
 * ```
 *
 * Versioning: adding functions, optional params, events, fields or `features` strings keeps the
 * version; changing or removing any of them bumps it. Rules: `embed/README.md`.
 */

/** Loads and instantiates the wasm (wasm-bindgen's default export). Call once, before anything else. */
export default function load(input?: { module_or_path?: string | URL | Request | Response | BufferSource | WebAssembly.Module }): Promise<unknown>;

export type EmbedMode = 'embedded' | 'full';
export type SaveFormat = 'psd' | 'pcraft' | 'png' | 'jpg' | 'jpeg' | 'webp' | 'tif' | 'tiff';
export type DocumentId = number;
export type Json = string | number | boolean | null | Json[] | { [key: string]: Json };

export interface EmbedVersion {
  embedApi: number;
  /** Full git commit of the Capishot build. */
  capishotCommit: string;
  /** Build id (short commit, `-dirty` for uncommitted builds); also the publish directory name. */
  buildHash: string;
  /** Capishot app version, e.g. "0.6.0". */
  app: string;
  /** `.pcraft` format version. */
  format: number;
  /** What this build offers (`documents.open`, `modes.live`, …). Strings are only added. */
  features: string[];
}

export interface InitOptions {
  /** A canvas to run on (sized by CSS) … */
  canvas?: HTMLCanvasElement;
  /** … or an element to create one in (filling it). */
  container?: HTMLElement;
  /** Default `embedded`. */
  mode?: EmbedMode;
  /** UI language: `es`, `en`, `pt-br`, `auto`… */
  locale?: string;
  /** `pro` (default), `proMedium`, `studio`, `studioLight`, `classic`. */
  theme?: string;
  /** Force the WebGL2 backend instead of WebGPU. */
  webgl?: boolean;
  /** Force the CPU canvas renderer. */
  cpu?: boolean;
  /**
   * `false`: never show the "Missing fonts" notice; the host gets `fontsMissing` events instead
   * (they go out either way). Default `true` (feature `fonts.notice`).
   */
  fontsNotice?: boolean;
}

/** `execute('edit.removeBubbleText', …)` params. */
export interface RemoveBubbleTextParams {
  /** The click, document pixels. */
  x: number;
  y: number;
  /** 1..255: how far from the background (a colour difference) a pixel counts as text. Default `auto`. */
  tolerance?: number | 'auto';
  /** Model the background from every visible layer. Default false. */
  sampleAll?: boolean;
  /** The result on a new layer above. Default false. */
  newLayer?: boolean;
  /** 1..32 px: outline gaps up to about twice this wide stay shut. Default `auto` (2 px on a 1x page, scaled with the ink's stroke width) (feature `removeBubbleText.search`). */
  gapRadius?: number | 'auto';
  /** 1..512 px: how far from the click a light pixel to start from may be. Default `auto` (24 px at 1x, scaled) (feature `removeBubbleText.search`). */
  reach?: number | 'auto';
  /** 16..65536 px: half the side of the largest square searched around the click, or the whole page. Default `auto` (1024 px at 1x, scaled, at most 35 % of the page's short side when that is more) (feature `removeBubbleText.search`). */
  window?: number | 'page' | 'auto';
}

/** `execute('edit.removeBubbleText', …)` result. Errors start `noBubble (dark|edge|tooBig|tooSmall|notBubble|beyondWindow)`, `notPlain:` or `noText:`. */
export interface RemoveBubbleTextResult {
  ok: true;
  layer: number;
  /** `[x, y, w, h]`, document pixels. */
  bubbleBounds: [number, number, number, number];
  /** Pixels painted. */
  pixels: number;
  background: 'solid' | 'gradient';
  /** How the bubble was bounded: its outline, the image edge, or its lettering's block. */
  region: 'bubble' | 'edge' | 'textCluster';
  newLayer: boolean;
  /** The sizes the search used (feature `removeBubbleText.search`). */
  search: {
    /** The page's scale against a 1x page, from the ink's stroke width. */
    scale: number;
    gapRadius: number;
    reach: number;
    window: number | 'page';
  };
}

/** `request('fonts.preload', …)` params (feature `fonts.preload`). */
export interface FontsPreloadParams {
  /** Family names, matched as documents' are: case, spaces, punctuation and parentheses aside, PostScript names too. */
  families: string[];
  /** Give up on downloads still running after this long (those count as `failed`). Default 10000, at most 120000. */
  timeoutMs?: number;
  /** Tag echoed in this preload's `fontsPreloadProgress` events (at most 200 characters). */
  id?: string;
}

/** `request('fonts.status', …)` params (feature `fonts.status`). */
export interface FontsStatusParams {
  /** Family names (matched as in `fonts.preload`). Default: every family the host serves. */
  families?: string[];
}

/** `request('fonts.status', …)` result: each family in one of the lists. Asks for nothing. */
export interface FontsStatusResult {
  /** Usable now: installed, bundled, stored, or served and downloaded. */
  loaded: string[];
  /** Served and downloading (or the manifest is still being read). */
  loading: string[];
  /** Served, but the download failed. */
  failed: string[];
  /** Served and not asked for yet: `fonts.preload` fetches it. */
  available: string[];
  /** Not installed and not served. */
  missing: string[];
  /** The served fonts manifest: `loading`, `loaded` or `none` (no manifest). */
  manifest: 'loading' | 'loaded' | 'none';
}

/** `request('fonts.preload', …)` result: each asked family in one of the lists. */
export interface FontsPreloadResult {
  /** Installed, bundled, or served and downloaded. */
  ready: string[];
  /** Nobody has it: not installed, not in the served fonts manifest. */
  missing: string[];
  /** Served, but its download failed or didn't finish in time. */
  failed: string[];
}

export interface DocumentInfo {
  id: DocumentId;
  name: string;
  width: number;
  height: number;
  /** Bumps on every edit (undo included). */
  revision: number;
  dirty: boolean;
  active: boolean;
  /** A hidden helper document (feature `documents.hidden`): never shown or made active. */
  hidden: boolean;
}

/** `openDocument`'s third argument (features `documents.hidden`, `open.cache`). */
export interface OpenOptions {
  /**
   * A hidden helper document: reachable with `execute(…, {document})`, never shown, given a tab,
   * listed in `documents` events or made active; the active document, its view, the tool and the
   * focus stay as they are. A PSD/PSB decodes off the frame (on a worker in the threads build, a
   * step at a time between frames in the single-threaded one; feature `open.hidden.background`),
   * other files at once. Default `false`.
   */
  hidden?: boolean;
  /**
   * While a PSD/PSB decodes, its merged image (read first) shows alone at the view the page
   * will have, a thin progress bar along the canvas's top: no opening card over the page
   * (feature `open.quiet`). Once the layers land the preview stays until the canvas has drawn
   * the view offscreen, at most 1 s (feature `open.quiet.prewarm`). Default `false`.
   */
  quiet?: boolean;
  /**
   * Keep the decoded document in the decoded-document cache under this key, and open it from
   * there when it holds the key at `version`: at once, nothing decoded, its pixels shared with
   * the cache and every other document opened from it (copy-on-write; a hidden raw helper and
   * the shown page hold one copy). An entry at another version is stale: dropped, the file
   * decoded again. Least recently used entries go past the budget (`request('document.cache',
   * {budgetMb})`, default 512 MB). Feature `open.cache`, `open.cache.sharedPixels`.
   */
  cacheKey?: string;
  /** The caller's version stamp of the file under `cacheKey` (a revision, a hash…). Default `""`. */
  version?: string | number;
}

export interface OpenedDocument extends DocumentInfo {
  /** Import notes ("adjustment layer flattened"…). */
  warnings: string[];
  /** Opened from the decoded-document cache (`OpenOptions.cacheKey`), nothing decoded. */
  cached: boolean;
}

export interface NewDocumentOptions {
  width: number;
  height: number;
  name?: string;
  /** `white` (default), `transparent`, `black`, `#rrggbb`. */
  background?: string;
  /** ppi, default 72. */
  resolution?: number;
  mode?: 'rgb' | 'gray' | 'cmyk' | 'lab';
  depth?: 8 | 16 | 32;
  /** A hidden helper document, as `OpenOptions.hidden` (feature `documents.hidden`). */
  hidden?: boolean;
}

export interface ToolOptions {
  /** Brush size in px. */
  size?: number;
  /** Foreground colour `#rrggbb`. */
  color?: string;
  /** 0..1 */
  opacity?: number;
  /** 0..1 */
  flow?: number;
  /** 0..1 */
  hardness?: number;
  /** Any `tools.setBrush` fields, passed through. */
  brush?: { [key: string]: Json };
}

export interface CommandInfo {
  id: string;
  label: string;
  menu: string[];
  shortcut: string | null;
  /** Human/agent-readable params description (JSON-ish). */
  params: string;
  enabled: boolean;
  /** `engine`: a document command; `ui`: a View/Window command of the shell. */
  kind: 'engine' | 'ui';
}

export interface EmbedState {
  mode: EmbedMode;
  tool: string;
  active: DocumentId | null;
  documents: DocumentInfo[];
}

export type EmbedEvent =
  | { type: 'ready'; version: EmbedVersion }
  | { type: 'documents'; documents: DocumentId[]; active: DocumentId | null }
  | { type: 'documentChanged'; id: DocumentId; revision: number; dirty: boolean }
  | { type: 'dirty'; id: DocumentId; dirty: boolean }
  | { type: 'selectionChanged'; id: DocumentId; bounds: [x: number, y: number, width: number, height: number] | null }
  | { type: 'toolChanged'; tool: string }
  | { type: 'modeChanged'; mode: EmbedMode }
  /** The active document's zoom factor changed (device pixels per document pixel, 1 = 100%). */
  | { type: 'zoom'; id: DocumentId; zoom: number }
  /**
   * The active document's view moved: any pan, zoom or rotation (hand tool, touch gestures, the
   * wheel, View commands, `zoom()`/`fit()`), or another document became active. At most one per
   * frame, only on change; not while a fit is still pending. `center` is the document point
   * (document pixels) at the canvas centre; `rotation` is in degrees, (−180, 180].
   */
  | { type: 'viewChanged'; id: DocumentId; zoom: number; center: [x: number, y: number]; rotation: number }
  /**
   * An opened document uses fonts nobody serves, or whose download failed (once per document,
   * after its served fonts arrived or failed). Goes out whether or not the notice shows
   * (feature `events.fontsMissing`).
   */
  | { type: 'fontsMissing'; families: string[] }
  /**
   * One family of a `fonts.preload` settled: `status` `loaded`, `failed` (download failed or
   * timed out) or `missing`; `done` of `total` families of that preload have settled, so the
   * last event has `done === total`. `id`: the preload's `id` param (feature
   * `events.fontsPreloadProgress`).
   */
  | { type: 'fontsPreloadProgress'; id?: string; family: string; status: 'loaded' | 'failed' | 'missing'; done: number; total: number }
  /**
   * A layer's pixels or layer mask changed (strokes, fills, Remove, clears, undo, redo…): `rect`
   * in document pixels, tile-aligned, the union of the changes since the last event. `pixelRev`
   * is the layer's new `pixelRev` (as in `document.inspect`). Feature `events.layerPixelsChanged`.
   */
  /**
   * Layers inside one group (`group` null: the top level) changed order; `layers` is that group's
   * children now, top to bottom. Adding, deleting or moving a layer to another group alone isn't
   * a reorder. Feature `events.layersReordered`.
   */
  | { type: 'layersReordered'; document: DocumentId; group: number | null; layers: number[] }
  | { type: 'layerPixelsChanged'; document: DocumentId; layer: number; rect: [x: number, y: number, width: number, height: number]; pixelRev: number }
  /** Another document became the visible (active) one (feature `events.documentShown`). */
  | { type: 'documentShown'; id: DocumentId; previous: DocumentId | null }
  /**
   * A new document is complete: no job works on it and its fonts have arrived or failed. Times
   * in ms from the `openDocument` / `open` call (else from the frame it appeared in) to the end of
   * its first frame and of its first complete frame (feature `events.documentReady`).
   */
  | { type: 'documentReady'; id: DocumentId; firstFrameMs: number; finalFrameMs: number }
  /**
   * The active document's view or the canvas size changed: `canvas` in device pixels, `rect` the
   * document area shown (document pixels; bounding box when rotated). At most one per animation
   * frame (feature `events.viewportChanged`).
   */
  | { type: 'viewportChanged'; document: DocumentId; zoom: number; center: [x: number, y: number]; rotation: number; canvas: [width: number, height: number]; rect: [x: number, y: number, width: number, height: number] }
  /** A layer's settings, type, shape or adjustment changed, or it was added or removed (feature `events.layerChanged`). */
  | { type: 'layerChanged'; document: DocumentId; layer: number; what: LayerChange[] }
  /** An opened document's fonts settled, its type drawn with them (feature `events.fontsReady`). */
  | { type: 'fontsReady'; document: DocumentId; ready: string[]; missing: string[]; failed: string[] }
  | { type: 'error'; message: string; source: 'init' | 'status'; fatal: boolean };

/** What a `layerChanged` event says changed. */
export type LayerChange =
  | 'added' | 'removed' | 'name' | 'visible' | 'opacity' | 'fillOpacity' | 'blend' | 'clipped'
  | 'locks' | 'label' | 'effects' | 'mask' | 'vectorMask' | 'parent' | 'content';

/** `getPixels` / `execute('layer.getPixels', …)` params (feature `layer.getPixels`). */
export interface GetPixelsParams {
  /** A layer id (or name). */
  layer: number | string;
  /** `[x, y, width, height]` in document pixels; default the layer's pixel bounds (a mask's: the canvas). At most 100 MP. */
  rect?: [x: number, y: number, width: number, height: number];
  /** `png` (default): a PNG file; `rgba`: raw 8-bit rows (straight alpha; 1 gray byte per pixel for a mask). */
  format?: 'png' | 'rgba';
  /** Read the layer mask instead of the pixels. */
  mask?: boolean;
  /** Any open document (not activated); default the active one. */
  document?: DocumentId;
}

/** `getPixels` result. `execute('layer.getPixels')` gives the same with `data` as base64. */
export interface LayerPixels {
  document: DocumentId;
  layer: number;
  rect: [x: number, y: number, width: number, height: number];
  width: number;
  height: number;
  format: 'png' | 'rgba';
  /** 4 (RGBA) or 1 (a mask). */
  channels: 1 | 4;
  data: Uint8Array;
  /** The layer's `pixelRev` these pixels are at (for `layer.dirtySince`). */
  pixelRev: number;
}

/** `execute('layer.dirtySince', {layer, rev, document?})` result (feature `layer.dirtySince`). */
export interface DirtySince {
  document: DocumentId;
  layer: number;
  pixelRev: number;
  /** Where the layer changed after `rev` (tile-aligned), `null` when it didn't. */
  rect: [x: number, y: number, width: number, height: number] | null;
  /** `rev` is older than the changes remembered (or not this document's): read the whole layer. */
  full: boolean;
}

/** `request('document.cache', …)`: change the decoded-document cache (feature `open.cache`). */
export interface DocumentCacheParams {
  /** Drop this key's entry. */
  remove?: string;
  /** Drop every entry. */
  clear?: boolean;
  /** Keep at most this much (default 512; 0 keeps nothing). */
  budgetMb?: number;
}

/** The decoded-document cache after `request('document.cache', …)`. */
export interface DocumentCacheInfo {
  budgetMb: number;
  /** What the entries hold; most of it is shared with open documents opened from them. */
  usedMb: number;
  /** Least recently used first. */
  entries: { key: string; version: string; mb: number }[];
}

/** `request('document.fontsReady', …)` (feature `document.fontsReady`). */
export interface DocumentFontsReadyParams {
  /** Default the active document. */
  document?: DocumentId;
  /** What is still downloading after this long counts as failed. Default 10000, at most 120000. */
  timeoutMs?: number;
}

export interface DocumentFontsReady {
  document: DocumentId;
  ready: string[];
  missing: string[];
  failed: string[];
}

/** The embedding API version (sync). */
export function embedApiVersion(): number;
/** Build stamp (sync). */
export function version(): EmbedVersion;
/** Starts Capishot; resolves once its first frame ran. Once per page until `destroy()`. */
export function init(options: InitOptions): Promise<EmbedVersion>;
/** Stops Capishot (a canvas `init` created is removed); pending calls reject. */
export function destroy(): void;
/** The single event listener (`null` removes it). */
export function onEvent(listener: ((event: EmbedEvent) => void) | null): void;

/** PSD/PSB/.pcraft/PNG/JPEG/WebP/… bytes → a new active document (a flat image becomes one layer). */
/**
 * Opens a file as a document and settles once it is in place (a PSD/PSB decodes in the
 * background, its merged image shown meanwhile; feature `open.whenOpen`). `data`: the file's
 * bytes, a URL (fetched as the page fetches, read from the response stream as it arrives; the
 * name defaults to the URL's last segment; feature `open.url`), or `null` with
 * `options.cacheKey` to open from the decoded-document cache only (rejects when it isn't there
 * at that version).
 */
export function openDocument(data: ArrayBuffer | ArrayBufferView | Blob | string | null, filename?: string | null, options?: OpenOptions | null): Promise<OpenedDocument>;
export function newDocument(options: NewDocumentOptions): Promise<DocumentInfo>;
/** `id` null = the active document. PSD keeps layers; a layered save marks the document clean.
 * PSD saves are incremental (unchanged layers aren't encoded again); the bytes are a full save's. */
export function saveDocument(id: DocumentId | null, format: SaveFormat): Promise<Uint8Array>;
export function closeDocument(id: DocumentId | null, force?: boolean): Promise<void>;
export function activateDocument(id: DocumentId): Promise<DocumentInfo>;

/** A shown or prewarmed document's camera: its own (fit the first time), the active document's
 * (`keep`), or `zoom` (device pixels per document pixel) with document point (`x`, `y`) centred. */
export type Viewport = 'keep' | 'own' | { zoom: number; x: number; y: number };

export interface ShowDocumentOptions {
  viewport?: Viewport;
  /** The document shown before becomes a hidden helper, textures kept (default true). */
  hidePrevious?: boolean;
  /** Swap anyway after this long if the canvas isn't drawn yet (default 1000). */
  timeoutMs?: number;
}

export interface PrewarmOptions {
  viewport?: Viewport;
  /** The whole document at full size, not just what the view will show (default false). */
  full?: boolean;
  /** Reply after this long whatever is drawn (default 10000). */
  timeoutMs?: number;
}

export interface SwitchResult {
  id: DocumentId;
  /** Frames waited for the canvas (0: it was warm, swapped in the frame of the call). */
  frames: number;
  ms: number;
  /** The wait ran out before the canvas (or the fonts) were ready. */
  timedOut: boolean;
  /** GPU bytes of the document's canvas textures. */
  gpuBytes: number;
  /** showDocument: shown (false: `superseded` by a later showDocument). */
  shown?: boolean;
  superseded?: boolean;
  index?: number;
}

/**
 * Shows document `id` (a hidden helper too) double-buffered (`editor.showDocument`, feature
 * `editor.showDocument`): its canvas is drawn for the view first while the shown document stays on
 * screen, then swapped in one frame; no blank or partial frame, no history step, no animation.
 */
export function showDocument(id: DocumentId, options?: ShowDocumentOptions | null): Promise<SwitchResult>;
/** Draws document `id`'s canvas ahead, fonts in (`document.prewarm`, feature `document.prewarm`). */
export function prewarmDocument(id: DocumentId, options?: PrewarmOptions | null): Promise<SwitchResult>;

export interface DocumentMemory {
  documents: { id: DocumentId; hidden: boolean; active: boolean; width: number; height: number; gpuMB: number; warm: boolean; pixelMB: number; framesSinceShown: number | null }[];
  gpuMB: number;
  compositorMB: number;
  budgetMB: number | null;
  gpu: boolean;
}
export function listDocuments(): Promise<DocumentInfo[]>;
export function getState(): Promise<EmbedState>;

/** `execute`'s third argument (features `execute.document`, `execute.history`). */
export interface ExecuteOptions {
  /** Run an engine command on this open document without making it active (no focus change). */
  document?: DocumentId;
  /**
   * `none` / `silent`: no undo step; the change is carried into every history state, so the
   * user's undo and redo keep it (per layer; document-wide changes aren't carried). Default `step`.
   */
  history?: 'step' | 'none' | 'silent';
}

/** One command of a `batch`. */
export interface BatchCommand {
  command: string;
  params?: { [key: string]: Json };
}

/** `batch`'s options (feature `batch`). */
export interface BatchOptions {
  /** Run on this open document (hidden ones too) without making it active. */
  document?: DocumentId;
  /** `one` (default): one undo step; `step`: one per command; `none` / `silent`: no undo step, kept through the user's undo and redo. */
  history?: 'one' | 'step' | 'none' | 'silent';
  /** `end` (the only way): drawn once, after the batch; a hidden document isn't drawn. */
  render?: 'end';
  /** `true` or `document.inspect`'s params: the document after the batch, as `inspect`. */
  inspect?: boolean | { [key: string]: Json };
}

export interface BatchResult {
  /** Each command's result, in order. */
  results: Json[];
  inspect?: Json;
}

/**
 * Runs `commands` in order as one transaction (`document.batch`): all or nothing (a failing
 * command undoes the batch and rejects with its error). Params are JSON: `layer.putPixels`' `src`
 * here is base64 or a `data:` URL (a Blob or URL goes through `execute`).
 */
export function batch(commands: BatchCommand[], options?: BatchOptions | null): Promise<BatchResult>;

/**
 * Runs a command by id (`engine.execute` of the control protocol); params: object or JSON string.
 * `layer.putPixels {layer, src}` takes a `Blob`, an `ArrayBuffer`, a typed array or a URL as
 * `src` too (feature `layer.putPixels.src`); `layer.copyToDocument {fromDocument, toDocument}`
 * activates nothing (feature `layer.copyToDocument.ids`); `layer.setPropsMany {layers: [{layer,
 * …}]}` is one batch of `layer.setProps` (feature `layer.setPropsMany`).
 * `edit.removeAi` and `paint.remove` with `ai: true` take `maxSide`: 0 (default) fills a hole larger
 * than 512 px at full resolution in overlapping, feathered tiles; 64..2048 downscales to it as
 * before (feature `removeAi.fullRes`).
 */
export function execute(command: string, params?: { [key: string]: Json } | string | null, options?: ExecuteOptions | null): Promise<Json>;
/** Any control-protocol method (`ui.inspect`, `ui.pointer`, …). */
export function request(method: 'fonts.preload', params: FontsPreloadParams): Promise<FontsPreloadResult>;
export function request(method: 'fonts.status', params?: FontsStatusParams | null): Promise<FontsStatusResult>;
/** Resolves once the document's fonts have arrived or failed and its type is drawn with them. */
export function request(method: 'document.fontsReady', params?: DocumentFontsReadyParams | null): Promise<DocumentFontsReady>;
export function request(method: 'document.cache', params?: DocumentCacheParams | null): Promise<DocumentCacheInfo>;
export function request(method: 'editor.showDocument', params: ShowDocumentOptions & { id: DocumentId }): Promise<SwitchResult>;
export function request(method: 'document.prewarm', params: PrewarmOptions & { id: DocumentId }): Promise<SwitchResult>;
/** One visible document: no tiles, tabs, floating windows or open cards (feature `editor.setLayout`). */
export function request(method: 'editor.setLayout', params: { single: boolean }): Promise<{ single: boolean }>;
/** Canvas textures (GPU canvas, CPU canvas tiles) all documents may hold; least recently shown evicted first, parsed data kept (feature `editor.setMemoryBudget`). */
export function request(method: 'editor.setMemoryBudget', params: { mb: number | null }): Promise<{ mb: number | null; evicted: DocumentId[] }>;
export function request(method: 'document.memory', params?: {} | null): Promise<DocumentMemory>;
export function request(method: string, params?: { [key: string]: Json } | null): Promise<Json>;
/** A layer's pixels or mask, read from its tiles: no document switch, no history step, nothing redrawn. */
export function getPixels(params: GetPixelsParams, options?: { document?: DocumentId } | null): Promise<LayerPixels>;
export function listCommands(): Promise<CommandInfo[]>;
/** Tool ids, e.g. `brush`, `eraser`, `cloneStamp` (sync). */
export function listTools(): string[];

/** `embedded`: only the canvas; `full`: Capishot's whole UI. Live, same documents. */
export function setMode(mode: EmbedMode): Promise<EmbedMode>;
export function setTool(tool: string, options?: ToolOptions | null): Promise<string>;
export function fit(): Promise<Json>;
/** A factor (1 = 100%) or `in`, `out`, `fit`, `actual`. */
export function zoom(level: number | 'in' | 'out' | 'fit' | 'actual'): Promise<Json>;
/** Resolves `true` when a step was undone/redone. */
export function undo(): Promise<Json>;
export function redo(): Promise<Json>;

/** Where an AI command (`edit.inpaintAi`, `edit.removeAi`, `paint.remove` with `ai`,
 * `image.upscaleAi`) really ran, in its `execute` reply. */
export interface AiRunInfo {
  /** The runtime's name (`"DirectML"`, `"Vulkan: …"`, `"GPU, DiscreteGpu(0)"`, `"CPU"`). */
  device: string;
  /** It ran on the CPU (asked for, or no usable GPU). */
  cpu: boolean;
  /** It left where it was headed (DirectML, the GPU); `notices` say why. */
  fellBack: boolean;
  notices?: AiNotice[];
}

export interface AiNotice {
  class: 'ai.gpuFallback' | 'ai.directmlFallback' | 'ai.cpuFallback' | string;
  /** The message shown to the user. */
  text: string;
  /** The underlying error. */
  cause: string;
}
