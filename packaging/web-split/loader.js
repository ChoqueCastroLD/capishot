// --- Lazily loaded chunks of the module (packaging/web-split/split.py) ---
// The first-load module imports one placeholder per function moved to a chunk ("placeholder.cN",
// field = its slot in the shared function table). A chunk's element segment puts its functions
// into that table (over the placeholders), so once it is in, calls go straight to them.
//
// On the page every chunk's download starts (high priority) as soon as the module's instance is
// up, in parallel with the app's start; they are compiled and instantiated once the app has
// drawn its first frame (`capishotSplit.firstFrame`), or as soon as a command asks for them. A command that needs code from a
// chunk not in yet waits for it (`capishotSplit.ready` / `whenLoaded`, the app's code gate: a job
// card instead of a stall). Only a call the app didn't see coming reaches a placeholder before
// its chunk: then the page fetches it synchronously, the last resort (a worker, which may block,
// always loads chunks that way). That failing too is reported (`capishotBoot.chunkFailed`).
const __split = {
    chunks: __SPLIT_CHUNKS__, tableName: __SPLIT_TABLE__, hot: new Set(__SPLIT_HOT__),
    loaded: Object.create(null), modules: Object.create(null), pending: Object.create(null), downloads: Object.create(null),
    prefetching: false, calls: 0, syncLoads: 0,
};

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

function __splitAllLoaded() {
    return Object.keys(__split.chunks).every((c) => __split.loaded[c]);
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

// The last resort: the chunk, now.
function __splitLoad(c) {
    if (__split.loaded[c]) return;
    let module = __split.modules[c];
    try {
        if (!module) {
            __split.syncLoads++;
            module = new WebAssembly.Module(__splitFetchSync(c));
        }
        new WebAssembly.Instance(module, { primary: wasm });
    } catch (e) {
        if (typeof document !== 'undefined' && self.capishotBoot && self.capishotBoot.chunkFailed) self.capishotBoot.chunkFailed(e, true);
        throw e;
    }
    __splitInstalled(c, module);
}

// The page: chunk `c` fetched, compiled and instantiated, tried three times (1 s, then 3 s apart).
function __splitFetch(c) {
    if (__split.loaded[c]) return Promise.resolve();
    if (__split.pending[c]) return __split.pending[c];
    const once = async () => {
        // The download started at the instance's start, else a new one.
        const early = __split.downloads[c];
        delete __split.downloads[c];
        const module = await WebAssembly.compile(await (early || __splitDownload(c)));
        if (__split.loaded[c]) return;
        await WebAssembly.instantiate(module, { primary: wasm });
        if (!__split.loaded[c]) __splitInstalled(c, module);
    };
    const p = (async () => {
        let error;
        for (const wait of [0, 1000, 3000]) {
            if (wait) await new Promise((r) => setTimeout(r, wait));
            try {
                return await once();
            } catch (e) {
                error = e;
            }
        }
        throw error;
    })();
    __split.pending[c] = p;
    // A failed chunk can be asked for again (the app's Try Again).
    p.catch(() => {}).finally(() => { delete __split.pending[c]; });
    return p;
}

function __splitDownload(c) {
    return fetch(__splitUrl(c), { priority: 'high' }).then((res) => {
        if (!res.ok) throw new Error(`${__split.chunks[c]}: HTTP ${res.status}`);
        return res.arrayBuffer();
    });
}

// For the app's code gate (photocraft-web): whether command `id` can run now, and every chunk.
const __splitApi = {
    state: __split,
    ready(id) {
        return __splitAllLoaded() || __split.hot.has(id) || __split.sync;
    },
    // Every chunk in; `onProgress(fraction)` as each one arrives. Rejects with the first chunk
    // that didn't load (after its retries).
    // The app drew its first frame: compile and instantiate what has downloaded meanwhile.
    firstFrame() {
        if (__split.lazy || __split.started) return;
        __split.started = true;
        for (const c of Object.keys(__split.chunks)) {
            __splitFetch(c).catch((e) => console.warn(`Capishot: chunk ${c} not loaded yet:`, e));
        }
    },
    whenLoaded(onProgress) {
        const all = Object.keys(__split.chunks);
        const report = () => { if (onProgress) onProgress(all.filter((c) => __split.loaded[c]).length / all.length); };
        report();
        return Promise.all(all.map((c) => __splitFetch(c).then(report))).then(() => undefined, (e) => {
            if (self.capishotBoot && self.capishotBoot.chunkFailed) self.capishotBoot.chunkFailed(e, false);
            throw e;
        });
    },
};

function __splitInstanceReady() {
    if (typeof document === 'undefined' || __split.prefetching) return;
    __split.prefetching = true;
    self.capishotSplit = __splitApi;
    // Diagnostics: ?split=lazy loads chunks only when a command waits for them; ?split=sync never
    // waits, so every call into a chunk loads it on the spot.
    const mode = new URLSearchParams(location.search).get('split');
    __split.sync = mode === 'sync';
    __split.lazy = mode === 'lazy' || mode === 'sync';
    if (__split.lazy) return;
    for (const c of Object.keys(__split.chunks)) {
        const d = __splitDownload(c);
        d.catch(() => {});
        __split.downloads[c] = d;
    }
}
