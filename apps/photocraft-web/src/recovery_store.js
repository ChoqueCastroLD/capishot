// Crash-recovery storage for Capishot in the browser (recovery.rs): checkpoints of unsaved
// documents in the origin-private file system, or IndexedDB where the browser can't write files
// there (no `createWritable`). One folder (or key prefix) per checkpoint key.
//
// A checkpoint is a generation `n`: its objects (`tiles/<hash>`, `blobs/<hash>`, shared between
// generations), `descriptor.<n>`, `sums.<n>` (each object it needs and its CRC-32) and `thumb`
// (a PNG). `head` names the current generation and the one before; it is written last, so a page
// killed mid-write leaves the previous checkpoint whole and current. The previous generation's
// files are kept until the next one is written, and a generation whose objects are missing or
// don't match their sums is passed over for the one before (or reported damaged).
//
// Sessions: each page holds a Web Lock named after its session while it lives, and marks the
// session closed cleanly on `pagehide`. Checkpoints of a session that holds no lock and didn't
// close cleanly were left by a crash (or a killed tab). Nothing here runs at import time: the
// worker pool of the threads build imports this module too.

const DIR = 'capishot-recovery';
const DB = 'capishot-recovery';
const STORE = 'files';
const CLEAN = 'capishot.recovery.clean';
const LOCK = 'capishot-session:';

let mode = null;
let dbp = null;

const fileName = (name) => name.replaceAll('/', '~');
const objectName = (file) => file.replaceAll('~', '/');

async function opfsRoot() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(DIR, { create: true });
}

async function detect() {
  try {
    if (navigator.storage && navigator.storage.getDirectory && self.FileSystemFileHandle && 'createWritable' in FileSystemFileHandle.prototype) {
      const dir = await opfsRoot();
      await put(dir, '.probe', new Uint8Array([1]));
      await dir.removeEntry('.probe');
      return 'opfs';
    }
  } catch (e) {
    // Private windows and some browsers refuse the file system: IndexedDB instead.
  }
  return self.indexedDB ? 'idb' : 'none';
}

function backend() {
  if (!mode) mode = detect();
  return mode;
}

function db() {
  if (!dbp) {
    dbp = new Promise((resolve, reject) => {
      const r = indexedDB.open(DB, 1);
      r.onupgradeneeded = () => r.result.createObjectStore(STORE);
      r.onsuccess = () => resolve(r.result);
      r.onerror = () => reject(r.error);
    });
  }
  return dbp;
}

// Runs `fn(store)` in one transaction; resolves with what `fn` returns (a request's result once done).
async function tx(write, fn) {
  const d = await db();
  return new Promise((resolve, reject) => {
    const t = d.transaction(STORE, write ? 'readwrite' : 'readonly');
    const out = fn(t.objectStore(STORE));
    t.oncomplete = () => resolve(out && 'result' in out ? out.result : out);
    t.onerror = () => reject(t.error);
    t.onabort = () => reject(t.error || new DOMException('The write was aborted', 'AbortError'));
  });
}

const range = (key) => IDBKeyRange.bound(key + '/', key + '/￿');

async function put(dir, name, data) {
  const h = await dir.getFileHandle(name, { create: true });
  const w = await h.createWritable();
  await w.write(data);
  await w.close();
}

async function read(dir, name) {
  const f = await (await dir.getFileHandle(name)).getFile();
  return new Uint8Array(await f.arrayBuffer());
}

export async function rcv_backend() {
  return backend();
}

let CRC = null;
function crc32(data) {
  if (!CRC) {
    CRC = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      CRC[n] = c >>> 0;
    }
  }
  let c = 0xffffffff;
  for (let i = 0; i < data.length; i++) c = CRC[(c ^ data[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

const enc = (v) => new TextEncoder().encode(JSON.stringify(v));
const dec = (b) => { try { return JSON.parse(new TextDecoder().decode(b)); } catch (e) { return null; } };

// Test hook: `self.__capishotRecoveryStop = 'objects' | 'descriptor'` stops the next write after
// that step and never finishes it (a page killed mid-write).
const stopAt = (step) => self.__capishotRecoveryStop === step ? new Promise(() => {}) : null;

// One key's storage, the same calls over OPFS or IndexedDB.
async function bucket(key, create) {
  if ((await backend()) === 'opfs') {
    const dir = await (await opfsRoot()).getDirectoryHandle(key, { create });
    return {
      get: async (name) => { try { return await read(dir, fileName(name)); } catch (e) { return null; } },
      put: (name, data) => put(dir, fileName(name), data),
      names: async () => { const out = []; for await (const n of dir.keys()) out.push(objectName(n)); return out; },
      remove: (name) => dir.removeEntry(fileName(name)).catch(() => {}),
    };
  }
  if ((await backend()) !== 'idb') throw new DOMException('This browser has no storage for recovery', 'NotSupportedError');
  return {
    get: async (name) => (await tx(false, (s) => s.get(key + '/' + fileName(name)))) || null,
    put: (name, data) => tx(true, (s) => { s.put(data, key + '/' + fileName(name)); }),
    names: async () => (await tx(false, (s) => s.getAllKeys(range(key)))).map((k) => objectName(k.slice(key.length + 1))),
    remove: (name) => tx(true, (s) => { s.delete(key + '/' + fileName(name)); }),
  };
}

async function headOf(b) {
  const h = dec((await b.get('head')) || new Uint8Array());
  return h && Number.isInteger(h.cur) ? h : null;
}

// Stores one checkpoint: `names[i]` holds `datas[i]`, then `sums` and the descriptor of a new
// generation, then the thumbnail, then `head` pointing at it (the step that makes it current);
// then what neither it nor the previous generation needs goes. `live` names every object the
// descriptor needs (stored now or by an earlier write).
export async function rcv_write(key, names, datas, descriptor, thumb, live) {
  const b = await bucket(key, true);
  const head = await headOf(b);
  const gen = head ? head.cur + 1 : 1;
  const sums = {};
  for (let i = 0; i < names.length; i++) {
    await b.put(names[i], datas[i]);
    sums[names[i]] = crc32(datas[i]);
  }
  // Objects stored by an earlier generation keep the sums it recorded.
  const before = head ? dec((await b.get('sums.' + head.cur)) || new Uint8Array()) || {} : {};
  for (const n of live) if (!(n in sums)) sums[n] = n in before ? before[n] : null;
  await (stopAt('objects') || Promise.resolve());
  await b.put('sums.' + gen, enc(sums));
  await b.put('descriptor.' + gen, descriptor);
  await (stopAt('descriptor') || Promise.resolve());
  if (thumb) await b.put('thumb', thumb);
  await b.put('head', enc({ cur: gen, prev: head ? head.cur : null }));
  // Keep this generation and the previous one (the fallback if this one turns out damaged).
  const keep = new Set(['head', 'thumb', 'descriptor.' + gen, 'sums.' + gen, ...live]);
  if (head) {
    keep.add('descriptor.' + head.cur);
    keep.add('sums.' + head.cur);
    for (const n of Object.keys(before)) keep.add(n);
  }
  for (const n of await b.names()) if (!keep.has(n)) await b.remove(n);
}

// Whether generation `gen` of a key is whole: its descriptor and every object its sums name.
// `deep` reads the objects and checks their CRC-32 too.
async function check(b, gen, names, deep) {
  const descriptor = await b.get('descriptor.' + gen);
  const sums = dec((await b.get('sums.' + gen)) || new Uint8Array());
  if (!descriptor || !sums) return null;
  for (const [n, crc] of Object.entries(sums)) {
    if (!names.has(n)) return null;
    if (deep && crc !== null) {
      const data = await b.get(n);
      if (!data || crc32(data) !== crc) return null;
    }
  }
  return { descriptor, sums };
}

// The generation to recover a key from: the current one if whole, else the previous; or the
// key's own single `descriptor` (checkpoints written before generations).
async function choose(key, deep) {
  const b = await bucket(key, false);
  const names = new Set(await b.names());
  const head = await headOf(b);
  if (!head) {
    const descriptor = await b.get('descriptor');
    return descriptor ? { b, gen: 0, status: 'ok', descriptor, sums: null } : { b, gen: null, status: 'incomplete', descriptor: null };
  }
  const cur = await check(b, head.cur, names, deep);
  if (cur) return { b, gen: head.cur, status: 'ok', ...cur };
  const prev = head.prev !== null ? await check(b, head.prev, names, deep) : null;
  if (prev) return { b, gen: head.prev, status: 'previous', ...prev };
  return { b, gen: head.cur, status: 'damaged', descriptor: await b.get('descriptor.' + head.cur), sums: null };
}

async function keys() {
  if ((await backend()) === 'opfs') {
    const out = [];
    for await (const [key, h] of (await opfsRoot()).entries()) if (h.kind === 'directory') out.push(key);
    return out;
  }
  if ((await backend()) !== 'idb') return [];
  const out = new Set();
  for (const k of await tx(false, (s) => s.getAllKeys())) {
    const at = k.indexOf('/');
    if (at > 0) out.add(k.slice(0, at));
  }
  return [...out];
}

// Every stored checkpoint: [key, descriptor or null, thumbnail or null, status], status `ok`,
// `previous` (the newest write didn't finish or is damaged: the one before is used), `damaged`
// (nothing whole) or `incomplete` (a first write that never finished).
export async function rcv_list() {
  const out = [];
  for (const key of await keys()) {
    try {
      const c = await choose(key, false);
      out.push([key, c.descriptor, await c.b.get('thumb'), c.status]);
    } catch (e) {
      out.push([key, null, null, 'damaged']);
    }
  }
  return out;
}

// One checkpoint to load: { descriptor, names, datas, status } (the objects its generation needs,
// their CRC-32 checked; the previous generation if the current one fails).
export async function rcv_read(key) {
  const c = await choose(key, true);
  if (!c.descriptor || c.status === 'damaged') throw new DOMException('The recovery checkpoint is damaged', 'DataError');
  const names = [];
  const datas = [];
  const wanted = c.sums ? Object.keys(c.sums) : (await c.b.names()).filter((n) => n !== 'thumb' && n !== 'descriptor');
  for (const n of wanted) {
    const data = await c.b.get(n);
    if (data) { names.push(n); datas.push(data); }
  }
  return { descriptor: c.descriptor, names, datas, status: c.status };
}

export async function rcv_remove(key) {
  if ((await backend()) === 'opfs') {
    await (await opfsRoot()).removeEntry(key, { recursive: true }).catch((e) => {
      if (e.name !== 'NotFoundError') throw e;
    });
  } else if ((await backend()) === 'idb') {
    await tx(true, (s) => s.delete(range(key)));
  }
}

// Asks once that the browser not evict the checkpoints under storage pressure.
export async function rcv_persist() {
  try {
    if (navigator.storage && navigator.storage.persist) return (await navigator.storage.persisted()) || (await navigator.storage.persist());
  } catch (e) { /* not offered */ }
  return false;
}

function readClean() {
  try {
    const v = JSON.parse(localStorage.getItem(CLEAN) || '[]');
    return Array.isArray(v) ? v.filter((s) => typeof s === 'string') : [];
  } catch (e) {
    return [];
  }
}

function markClean(id, on) {
  try {
    const s = new Set(readClean());
    if (on) s.add(id); else s.delete(id);
    localStorage.setItem(CLEAN, JSON.stringify([...s]));
  } catch (e) { /* storage denied: the session counts as crashed */ }
}

// Starts this page's session: holds its lock while the page lives, and marks it closed cleanly
// when the page goes away normally (not after a crash: `rcv_mark_crashed`).
export function rcv_session_start(id) {
  if (navigator.locks && navigator.locks.request) navigator.locks.request(LOCK + id, () => new Promise(() => {})).catch(() => {});
  addEventListener('pagehide', () => { if (!self.__capishotCrashed) markClean(id, true); });
  addEventListener('pageshow', (e) => { if (e.persisted) markClean(id, false); });
}

// The app stopped (a panic): leaving the page now is not a clean close.
export function rcv_mark_crashed(id) {
  self.__capishotCrashed = true;
  markClean(id, false);
}

export function rcv_clean_sessions() {
  return readClean();
}

export function rcv_set_clean_sessions(list) {
  try { localStorage.setItem(CLEAN, JSON.stringify(list)); } catch (e) { /* ignore */ }
}

// Sessions whose page is alive (in any tab), or null when the browser can't tell.
export async function rcv_live_sessions() {
  if (!navigator.locks || !navigator.locks.query) return null;
  const q = await navigator.locks.query();
  return (q.held || []).map((l) => l.name).filter((n) => n && n.startsWith(LOCK)).map((n) => n.slice(LOCK.length));
}
