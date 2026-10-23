// Crash-recovery storage for Capishot in the browser (recovery.rs): checkpoints of unsaved
// documents in the origin-private file system, or IndexedDB where the browser can't write files
// there (no `createWritable`). One folder (or key prefix) per checkpoint key: its objects
// (`tiles/<hash>`, `blobs/<hash>`), `thumb` (a PNG) and `descriptor`, written last.
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

// Stores one checkpoint: `names[i]` holds `datas[i]`, then the thumbnail and the descriptor;
// then every other object of the key goes (the descriptor names `live`).
export async function rcv_write(key, names, datas, descriptor, thumb, live) {
  const keep = new Set(live.map(fileName));
  keep.add('descriptor');
  keep.add('thumb');
  if ((await backend()) === 'opfs') {
    const dir = await (await opfsRoot()).getDirectoryHandle(key, { create: true });
    for (let i = 0; i < names.length; i++) await put(dir, fileName(names[i]), datas[i]);
    if (thumb) await put(dir, 'thumb', thumb);
    await put(dir, 'descriptor', descriptor);
    const stale = [];
    for await (const name of dir.keys()) if (!keep.has(name)) stale.push(name);
    for (const name of stale) await dir.removeEntry(name).catch(() => {});
    return;
  }
  if ((await backend()) !== 'idb') throw new DOMException('This browser has no storage for recovery', 'NotSupportedError');
  // One transaction: the checkpoint lands whole or not at all.
  await tx(true, (s) => {
    for (let i = 0; i < names.length; i++) s.put(datas[i], key + '/' + fileName(names[i]));
    if (thumb) s.put(thumb, key + '/thumb');
    s.put(descriptor, key + '/descriptor');
  });
  const keys = await tx(false, (s) => s.getAllKeys(range(key)));
  const stale = keys.filter((k) => !keep.has(k.slice(key.length + 1)));
  if (stale.length) await tx(true, (s) => stale.forEach((k) => s.delete(k)));
}

// Every stored checkpoint: [key, descriptor, thumbnail or null]; keys whose descriptor is missing
// (a first write that never finished) as [key, null, null].
export async function rcv_list() {
  const out = [];
  const b = await backend();
  if (b === 'opfs') {
    const root = await opfsRoot();
    for await (const [key, h] of root.entries()) {
      if (h.kind !== 'directory') continue;
      let descriptor = null;
      let thumb = null;
      try { descriptor = await read(h, 'descriptor'); } catch (e) { /* unfinished */ }
      try { thumb = await read(h, 'thumb'); } catch (e) { /* none */ }
      out.push([key, descriptor, thumb]);
    }
  } else if (b === 'idb') {
    const keys = await tx(false, (s) => s.getAllKeys());
    const byKey = new Map();
    for (const k of keys) {
      const at = k.indexOf('/');
      if (at > 0 && !byKey.has(k.slice(0, at))) byKey.set(k.slice(0, at), true);
    }
    for (const key of byKey.keys()) {
      const descriptor = await tx(false, (s) => s.get(key + '/descriptor'));
      const thumb = await tx(false, (s) => s.get(key + '/thumb'));
      out.push([key, descriptor || null, thumb || null]);
    }
  }
  return out;
}

// One checkpoint to load: { descriptor, names, datas } (the objects by name).
export async function rcv_read(key) {
  const names = [];
  const datas = [];
  let descriptor = null;
  if ((await backend()) === 'opfs') {
    const dir = await (await opfsRoot()).getDirectoryHandle(key);
    for await (const [name, h] of dir.entries()) {
      if (h.kind !== 'file' || name === 'thumb') continue;
      const data = new Uint8Array(await (await h.getFile()).arrayBuffer());
      if (name === 'descriptor') descriptor = data;
      else { names.push(objectName(name)); datas.push(data); }
    }
  } else {
    const keys = await tx(false, (s) => s.getAllKeys(range(key)));
    const values = await tx(false, (s) => s.getAll(range(key)));
    keys.forEach((k, i) => {
      const name = k.slice(key.length + 1);
      if (name === 'descriptor') descriptor = values[i];
      else if (name !== 'thumb') { names.push(objectName(name)); datas.push(values[i]); }
    });
  }
  if (!descriptor) throw new DOMException('The recovery checkpoint is incomplete', 'NotFoundError');
  return { descriptor, names, datas };
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
