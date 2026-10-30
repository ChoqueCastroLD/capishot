// --- Lazily loaded chunks of the module (packaging/web-split/split.py) ---
// The first-load module imports one placeholder per function moved to a chunk ("placeholder.cN",
// field = its slot in the shared function table). A placeholder loads its chunk, whose element
// segment puts the real functions into that table, then calls the real one. On the page the chunks
// are fetched and instantiated in the background once the app runs, so placeholders rarely have to
// load anything; a worker (the threads build's pool) loads a chunk synchronously when it first
// needs it. A chunk's element segment also overwrites the slots that held its placeholders, so once
// it is in, calls stop going through JavaScript (split.py checks no placeholder sits elsewhere).
const __split = { chunks: __SPLIT_CHUNKS__, tableName: __SPLIT_TABLE__, loaded: Object.create(null), modules: Object.create(null), prefetching: false, calls: 0, syncLoads: 0 };

function __splitAddImports(imports) {
    for (const c of Object.keys(__split.chunks)) {
        const made = Object.create(null);
        imports['placeholder.' + c] = new Proxy({}, {
            get(_, key) {
                if (typeof key !== 'string') return undefined;
                if (made[key]) return made[key];
                const i = Number(key);
                const f = function (...args) {
                    __split.calls++;
                    __splitLoad(c);
                    return __splitTable().get(i)(...args);
                };
                made[key] = f;
                return f;
            },
        });
    }
}

// The table the chunks fill (the split adds it to the module's exports).
function __splitTable() {
    return wasm[__split.tableName];
}

function __splitUrl(c) {
    return new URL(__split.chunks[c], import.meta.url).href;
}

// The chunk's bytes, synchronously: on the page with the x-user-defined text trick (a page's
// synchronous request can't ask for an ArrayBuffer), in a worker as an ArrayBuffer. A failed
// request is tried again (twice): the call that needs the chunk can't go on without it.
function __splitFetchSync(c) {
    const page = typeof document !== 'undefined';
    let error;
    for (let attempt = 0; attempt < 3; attempt++) {
        try {
            const x = new XMLHttpRequest();
            x.open('GET', __splitUrl(c), false);
            if (page) x.overrideMimeType('text/plain; charset=x-user-defined');
            else x.responseType = 'arraybuffer';
            x.send();
            if (x.status !== 200) throw new Error(`${__split.chunks[c]}: HTTP ${x.status}`);
            if (!page) return new Uint8Array(x.response);
            const s = x.responseText;
            const b = new Uint8Array(s.length);
            for (let i = 0; i < s.length; i++) b[i] = s.charCodeAt(i) & 0xff;
            return b;
        } catch (e) {
            error = e;
        }
    }
    throw error;
}

function __splitInstalled(c, module) {
    __split.modules[c] = module;
    __split.loaded[c] = true;
}

function __splitLoad(c) {
    if (__split.loaded[c]) return;
    let module = __split.modules[c];
    if (!module) {
        __split.syncLoads++;
        module = new WebAssembly.Module(__splitFetchSync(c));
    }
    new WebAssembly.Instance(module, { primary: wasm });
    __splitInstalled(c, module);
}

// The page: every chunk in the background once the app runs (it removes #photocraft_loading).
async function __splitPrefetch() {
    for (const c of Object.keys(__split.chunks)) {
        if (__split.loaded[c]) continue;
        try {
            const res = await fetch(__splitUrl(c));
            if (!res.ok) throw new Error(`HTTP ${res.status}`);
            const module = await WebAssembly.compileStreaming(res).catch(async () => WebAssembly.compile(await (await fetch(__splitUrl(c))).arrayBuffer()));
            if (__split.loaded[c]) continue;
            await WebAssembly.instantiate(module, { primary: wasm });
            if (!__split.loaded[c]) __splitInstalled(c, module);
        } catch (e) {
            // A placeholder loads it on first use instead.
            console.warn(`Capishot: chunk ${c} not preloaded:`, e);
        }
    }
}

function __splitInstanceReady() {
    if (typeof document === 'undefined' || __split.prefetching) return;
    __split.prefetching = true;
    // Diagnostics: `capishotSplit` (counters); ?split=lazy loads chunks only when first called.
    self.capishotSplit = __split;
    if (/[?&]split=lazy\b/.test(location.search)) return;
    const go = () => {
        if (document.getElementById('photocraft_loading')) { setTimeout(go, 500); return; }
        __splitPrefetch();
    };
    setTimeout(go, 500);
}
