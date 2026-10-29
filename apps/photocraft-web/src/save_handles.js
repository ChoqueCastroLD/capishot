// Save to device (save_handles.rs): Save As asks the browser for a file (File System Access API,
// `showSaveFilePicker`) where it has one, and the handle is kept by the name it got, so the next
// File › Save of that document writes the same file in place instead of downloading a new copy.
// Elsewhere (Firefox, Safari, browsers without the API) nothing changes: saves download.
// Nothing runs at import time (the worker pool imports this module too).
const handles = new Map();

export function sh_supported() {
  return typeof self.showSaveFilePicker === 'function' && self.isSecureContext !== false;
}

// Asks for a file named like `suggested`; resolves to the chosen file's name, `null` when the
// user cancelled, or throws when the browser refused (no user gesture, a sandboxed frame).
export async function sh_pick(suggested, description, mime, ext) {
  const opts = { suggestedName: suggested };
  if (ext) opts.types = [{ description, accept: { [mime]: ['.' + ext] } }];
  try {
    const handle = await self.showSaveFilePicker(opts);
    handles.set(handle.name, handle);
    return handle.name;
  } catch (e) {
    if (e && e.name === 'AbortError') return null;
    throw e;
  }
}

export function sh_has(name) {
  return handles.has(name);
}

export function sh_forget(name) {
  handles.delete(name);
}

// Writes `parts` (Uint8Arrays) to the kept file `name`, streamed one part at a time; the file is
// replaced only when the write completes (the browser writes a temporary file, then swaps it in).
export async function sh_write(name, parts) {
  const handle = handles.get(name);
  if (!handle) throw new Error('no file handle for ' + name);
  if (handle.queryPermission && (await handle.queryPermission({ mode: 'readwrite' })) !== 'granted') {
    const p = handle.requestPermission ? await handle.requestPermission({ mode: 'readwrite' }) : 'denied';
    if (p !== 'granted') throw new Error('permission to write ' + name + ' was not granted');
  }
  const w = await handle.createWritable();
  try {
    for (let i = 0; i < parts.length; i++) await w.write(parts[i]);
    await w.close();
  } catch (e) {
    try { await w.abort(); } catch (_) {}
    throw e;
  }
}
