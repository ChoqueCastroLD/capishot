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
}

export interface OpenedDocument extends DocumentInfo {
  /** Import notes ("adjustment layer flattened"…). */
  warnings: string[];
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
  | { type: 'error'; message: string; source: 'init' | 'status'; fatal: boolean };

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
export function openDocument(data: ArrayBuffer | ArrayBufferView | Blob, filename: string): Promise<OpenedDocument>;
export function newDocument(options: NewDocumentOptions): Promise<DocumentInfo>;
/** `id` null = the active document. PSD keeps layers; a layered save marks the document clean. */
export function saveDocument(id: DocumentId | null, format: SaveFormat): Promise<Uint8Array>;
export function closeDocument(id: DocumentId | null, force?: boolean): Promise<void>;
export function activateDocument(id: DocumentId): Promise<DocumentInfo>;
export function listDocuments(): Promise<DocumentInfo[]>;
export function getState(): Promise<EmbedState>;

/** Runs a command by id (`engine.execute` of the control protocol); params: object or JSON string. */
export function execute(command: string, params?: { [key: string]: Json } | string | null): Promise<Json>;
/** Any control-protocol method (`ui.inspect`, `ui.pointer`, …). */
export function request(method: string, params?: { [key: string]: Json } | null): Promise<Json>;
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
