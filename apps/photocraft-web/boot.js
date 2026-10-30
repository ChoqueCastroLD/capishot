// Capishot's page loader around the WebAssembly start, shared by both web builds (the threads
// page at the site root and the single-threaded page in st/). Plain script, no modules, so it
// runs before anything else and works when the WebAssembly never does.
//
// - Progress: the module's download, then "preparing" (compiling and instantiating) and
//   "starting" (until the app removes #photocraft_loading). No time limits anywhere.
// - A broken load says why: the stage, the error and the bytes loaded, in a "Details" toggle.
// - The threads build failing at any stage goes to the single-threaded build; the
//   single-threaded build retries once past the caches, then shows the reason with Reload.
// - Not enough memory for the module gets its own message ("close other tabs").
// - A failed load is reported with one small beacon (`load_failed`, no WebAssembly needed) on
//   Capishot's own hosts, unless the browser sends Do Not Track or Global Privacy Control.
//
// Only failures of the load itself count: the module's files, the WebAssembly compile and
// instantiate, the worker pool and the app's start. An unrelated failing request (a blocked
// third-party fetch, a missing font) before the app has started is not a broken load.
(() => {
  const script = document.currentScript;
  const build = (script && script.dataset.build) || 'st';
  const RETRY = 'capishot.loadRetry';
  const FROM_THREADS = 'capishot.threadsFailure';
  const t0 = performance.now();
  const lang = (navigator.language || 'en').slice(0, 2);
  const TEXT = {
    en: {
      loading: 'Loading Capishot…', download: 'Downloading Capishot', preparing: 'Preparing Capishot…', starting: 'Starting Capishot…',
      failed: "Capishot didn't load completely.", memory: "This device doesn't have enough free memory for Capishot. Close other tabs or apps and try again.",
      reload: 'Reload', details: 'Details', stage: 'Stage', error: 'Error', loaded: 'Downloaded', build: 'Build', threadsFirst: 'Threads build first',
    },
    es: {
      loading: 'Cargando Capishot…', download: 'Descargando Capishot', preparing: 'Preparando Capishot…', starting: 'Iniciando Capishot…',
      failed: 'No se pudo cargar Capishot por completo.', memory: 'Tu dispositivo no tiene memoria suficiente para Capishot. Cierra otras pestañas o apps e inténtalo de nuevo.',
      reload: 'Recargar', details: 'Detalles', stage: 'Etapa', error: 'Error', loaded: 'Descargado', build: 'Versión', threadsFirst: 'Antes, la versión con hilos',
    },
    pt: {
      loading: 'Carregando o Capishot…', download: 'Baixando o Capishot', preparing: 'Preparando o Capishot…', starting: 'Iniciando o Capishot…',
      failed: 'Não foi possível carregar o Capishot por completo.', memory: 'Este dispositivo não tem memória livre suficiente para o Capishot. Feche outras abas ou apps e tente de novo.',
      reload: 'Recarregar', details: 'Detalhes', stage: 'Etapa', error: 'Erro', loaded: 'Baixado', build: 'Versão', threadsFirst: 'Antes, a versão com threads',
    },
  };
  const text = TEXT[lang] || TEXT.en;

  // Sizes deploy.sh writes into the page: the raw module, and what it costs to download with
  // Brotli and with gzip (bytes). Without them the progress shows the bytes received.
  // Read when the download starts: the meta comes after this script in <head>.
  const readSizes = () => {
    const m = document.querySelector('meta[name="capishot-wasm-bytes"]');
    const [raw, br, gz] = ((m && m.content) || '').split(/\s+/).map(Number);
    return raw > 0 ? { raw, br: br || 0, gz: gz || 0 } : null;
  };

  const state = { stage: 'html', bytes: 0, gzipStream: false, failed: false, started: false, wasmFetchError: null };
  const el = (id) => document.getElementById(id);
  const statusEl = () => el('photocraft_status');
  const setStatus = (s) => { const p = statusEl(); if (p && !state.failed) p.textContent = s; };
  const mb = (b) => (b / 1048576).toFixed(1);

  const stage = (s) => {
    if (state.failed || state.started) return;
    state.stage = s;
    if (s === 'compile' || s === 'instantiate') setStatus(text.preparing);
    else if (s === 'threads_pool' || s === 'start') setStatus(text.starting);
  };
  const progress = (bytes) => {
    if (state.failed) return;
    state.bytes = bytes;
    if (state.stage !== 'wasm_download') return;
    // The decoded bytes are the raw module (Brotli), or the gzip file the threads page inflates.
    const sizes = readSizes();
    if (sizes) {
      const whole = state.gzipStream ? sizes.gz : sizes.raw;
      const transfer = state.gzipStream ? sizes.gz : (sizes.br || sizes.gz || sizes.raw);
      const done = Math.min(1, bytes / whole) * transfer;
      setStatus(`${text.download} ${mb(done)}/${mb(transfer)} MB`);
    } else {
      setStatus(`${text.download} ${mb(bytes)} MB`);
    }
  };

  // The module's response with its bytes counted: the threads page passes its fetch through
  // here; the single-threaded page's loader (Trunk's) is reached through `fetch` below.
  const counted = (res, gzip) => {
    state.gzipStream = !!gzip;
    if (!res.body) return res;
    const reader = res.body.getReader();
    let n = 0;
    const body = new ReadableStream({
      async pull(c) {
        try {
          const { done, value } = await reader.read();
          if (done) { stage('compile'); c.close(); return; }
          n += value.byteLength;
          progress(n);
          c.enqueue(value);
        } catch (e) {
          state.wasmFetchError = e;
          c.error(e);
        }
      },
      cancel(reason) { return reader.cancel(reason); },
    });
    return new Response(body, { status: res.status, statusText: res.statusText, headers: res.headers });
  };
  const isModuleUrl = (u) => /_bg\.wasm(\.gz)?(\?|$)/.test(u);
  const ourAsset = (u) => {
    try {
      const url = new URL(u, location.href);
      return url.origin === location.origin && /(photocraft[^/]*\.(js|wasm|gz)|\/snippets\/|workerHelpers)/.test(url.pathname);
    } catch (e) { return false; }
  };
  const realFetch = self.fetch.bind(self);
  self.fetch = function (input, init) {
    const u = typeof input === 'string' ? input : (input && input.url) || String(input);
    if (build !== 'st' || !isModuleUrl(u) || state.started) return realFetch(input, init);
    stage('wasm_download');
    return realFetch(input, init).then((res) => {
      if (!res.ok) { const e = new Error(`HTTP ${res.status}`); e.http = res.status; throw e; }
      return counted(res, false);
    }, (e) => { state.wasmFetchError = e; throw e; });
  };

  // Error class for the report and the message.
  const classify = (e) => {
    const name = (e && e.name) || typeof e;
    const msg = String((e && e.message) || e || '');
    if (e && e.http) return 'http';
    if (/out of memory|could not allocate|allocation failed|Cannot allocate|Memory.*(grow|size)|array buffer allocation/i.test(msg)
      || (name === 'RangeError' && /memory|buffer|allocat/i.test(msg))) return 'oom';
    if (name === 'CompileError') return 'compile';
    if (name === 'LinkError') return 'link';
    if (name === 'RuntimeError') return 'runtime';
    if (name === 'SecurityError' || /SharedArrayBuffer|shared memory|crossOriginIsolated|cross-origin isolated|blocked/i.test(msg)) return 'blocked';
    if (/Failed to fetch|NetworkError|network error|Load failed|ERR_|aborted|didn't load|import/i.test(msg) || e === state.wasmFetchError) return 'network';
    return 'unknown';
  };
  const browserInfo = () => {
    const ua = navigator.userAgent;
    const os = /Android/i.test(ua) ? 'android' : /iPhone|iPad|iPod/.test(ua) ? 'ios' : /CrOS/.test(ua) ? 'chromeos'
      : /Windows/.test(ua) ? 'windows' : /Mac OS X|Macintosh/.test(ua) ? 'macos' : /Linux/.test(ua) ? 'linux' : 'other';
    let browser = 'other', major = 0;
    const m = (re) => { const r = ua.match(re); return r ? Number(r[1]) : 0; };
    if (navigator.brave) { browser = 'brave'; major = m(/Chrome\/(\d+)/); }
    else if (/SamsungBrowser\//.test(ua)) { browser = 'samsung'; major = m(/SamsungBrowser\/(\d+)/); }
    else if (/Edg\//.test(ua)) { browser = 'edge'; major = m(/Edg\/(\d+)/); }
    else if (/OPR\//.test(ua)) { browser = 'opera'; major = m(/OPR\/(\d+)/); }
    else if (/Firefox\//.test(ua)) { browser = 'firefox'; major = m(/Firefox\/(\d+)/); }
    else if (/Chrome\//.test(ua)) { browser = 'chrome'; major = m(/Chrome\/(\d+)/); }
    else if (/Safari\//.test(ua)) { browser = 'safari'; major = m(/Version\/(\d+)/); }
    const dm = navigator.deviceMemory;
    const mem = typeof dm !== 'number' ? 'unknown' : dm < 1 ? '<1' : dm < 2 ? '1' : dm < 4 ? '2' : dm < 8 ? '4' : '8+';
    const c = navigator.connection;
    const net = c && ['slow-2g', '2g', '3g', '4g'].includes(c.effectiveType) ? c.effectiveType : 'unknown';
    return { browser, browser_major: Math.min(major, 9999), os, mem, net };
  };
  const hex = (n) => Array.from(crypto.getRandomValues(new Uint8Array(n)), (b) => b.toString(16).padStart(2, '0')).join('');

  // `load_failed`: once per failure shown and once when the threads build falls back.
  const beacon = (r) => {
    try {
      if (!/(^|\.)capichan\.com$/.test(location.hostname)) return;
      if (navigator.doNotTrack === '1' || navigator.globalPrivacyControl === true) return;
      let iid = '';
      try { iid = localStorage.getItem('capishot.telemetry.iid') || ''; } catch (e) {}
      if (!/^[0-9a-f]{16,64}$/.test(iid)) iid = hex(16);
      const ev = Object.assign({ e: 'load_failed' }, r, browserInfo());
      const body = JSON.stringify({ v: 6, iid, sid: hex(16), seq: 1, events: [ev] });
      const url = '/capishot/api/t';
      if (!(navigator.sendBeacon && navigator.sendBeacon(url, body))) {
        realFetch(url, { method: 'POST', body, keepalive: true }).catch(() => {});
      }
    } catch (e) {}
  };
  const report = (e, outcome, fromThreads) => {
    const err = classify(e);
    const name = (e && e.name && /^[A-Za-z]{1,32}$/.test(e.name)) ? e.name : 'Error';
    const r = {
      outcome, stage: state.stage, err, name, build, coi: self.crossOriginIsolated === true,
      kb: Math.min(1000000, Math.round(state.bytes / 1024)), ms: Math.min(3600000, Math.round(performance.now() - t0)),
      retried: (() => { try { return sessionStorage.getItem(RETRY) === '1'; } catch (x) { return false; } })(),
    };
    if (e && e.http) r.http = Math.min(999, e.http);
    if (fromThreads) { r.mt_stage = fromThreads.stage; r.mt_err = fromThreads.err; }
    return r;
  };

  const reload = (bust) => {
    const u = new URL(location.href);
    if (bust) u.searchParams.set('r', Date.now().toString(36));
    location.replace(u.href);
  };

  const showFailure = (e, r, fromThreads) => {
    const box = el('photocraft_loading');
    if (!box) return;
    box.innerHTML = '';
    box.style.pointerEvents = 'auto';
    box.classList.add('error');
    const p = document.createElement('p');
    p.id = 'photocraft_load_failed';
    p.textContent = r.err === 'oom' ? text.memory : text.failed;
    const b = document.createElement('button');
    b.id = 'photocraft_reload';
    b.textContent = text.reload;
    b.style.cssText = 'font:15px system-ui,sans-serif;padding:10px 22px;border-radius:20px;border:0;background:#3b82f6;color:#fff;margin-top:8px';
    b.onclick = () => { try { sessionStorage.removeItem(RETRY); sessionStorage.removeItem(FROM_THREADS); } catch (x) {} reload(true); };
    const d = document.createElement('details');
    d.id = 'photocraft_load_details';
    d.style.cssText = 'margin-top:14px;max-width:420px;color:#9a9a9a;font:12px system-ui,sans-serif;text-align:left';
    const s = document.createElement('summary');
    s.textContent = text.details;
    s.style.cssText = 'cursor:pointer;text-align:center';
    const pre = document.createElement('pre');
    pre.style.cssText = 'white-space:pre-wrap;word-break:break-word;margin:8px 0 0;font:12px ui-monospace,monospace';
    const msg = String((e && e.message) || e || '').slice(0, 500);
    const lines = [
      `${text.stage}: ${r.stage}`,
      `${text.error}: ${r.name}${msg ? `: ${msg}` : ''} (${r.err})`,
      `${text.loaded}: ${mb(state.bytes)} MB, ${(r.ms / 1000).toFixed(1)} s`,
      `${text.build}: ${build}${r.coi ? ', isolated' : ''}`,
    ];
    if (fromThreads) lines.push(`${text.threadsFirst}: ${fromThreads.stage}, ${fromThreads.name}: ${fromThreads.message || ''} (${fromThreads.err})`);
    pre.textContent = lines.join('\n');
    d.append(s, pre);
    box.append(p, b, d);
  };

  // The load failed at the current stage.
  const fail = (e) => {
    if (state.failed || state.started || !el('photocraft_loading')) return;
    console.error(`Capishot: the load failed (${state.stage}):`, e);
    state.failed = true;
    let fromThreads = null;
    try { fromThreads = JSON.parse(sessionStorage.getItem(FROM_THREADS) || 'null'); } catch (x) {}
    const r = report(e, 'failed', fromThreads);
    const fallback = build === 'threads' && self.capishotFallback;
    if (fallback) {
      // The threads build failed: the single-threaded build, before any message.
      r.outcome = 'fell_back';
      beacon(r);
      try {
        sessionStorage.setItem(FROM_THREADS, JSON.stringify({ stage: r.stage, err: r.err, name: r.name, message: String((e && e.message) || e || '').slice(0, 300) }));
      } catch (x) {}
      const u = new URL(fallback, location.href);
      u.search = location.search;
      u.searchParams.set('r', Date.now().toString(36));
      location.replace(u.href + location.hash);
      return;
    }
    // One automatic retry past the caches (not for memory: the same device fails the same way).
    let retried = true;
    try { retried = sessionStorage.getItem(RETRY) === '1'; sessionStorage.setItem(RETRY, '1'); } catch (x) {}
    if (!retried && r.err !== 'oom') { reload(true); return; }
    beacon(r);
    showFailure(e, r, fromThreads);
  };

  // An error from the WebAssembly side of the load (compile, link, run, memory).
  const wasmish = (e) => {
    if (!e) return false;
    const name = e.name || '';
    const msg = String(e.message || e);
    return /CompileError|LinkError|RuntimeError/.test(name) || /WebAssembly|wasm/i.test(msg) || classify(e) === 'oom';
  };

  // Resource errors of the module's own files (capture: they don't bubble).
  addEventListener('error', (ev) => {
    if (state.started) return;
    const t = ev.target;
    if (t && t !== window && (t.src || t.href)) {
      const u = t.src || t.href;
      if (ourAsset(u)) {
        if (isModuleUrl(u)) stage('wasm_download');
        fail(new Error(`${u} didn't load`));
      }
      return;
    }
    // A module script's failed top-level await (Trunk's loader: the instantiation) is an `error`.
    const err = ev.error;
    if (wasmish(err) || (ev.filename && ourAsset(ev.filename) && err)) fail(err || new Error(ev.message));
  }, true);
  // The loader's own promise failing (Trunk's module script, the WebAssembly instantiation).
  addEventListener('unhandledrejection', (ev) => {
    if (state.started) return;
    const e = ev.reason;
    if (wasmish(e) || (state.wasmFetchError && (e === state.wasmFetchError || state.stage === 'wasm_download'))) fail(e);
  });

  // Trunk's loader (the single-threaded page) instantiated the module: the app is starting.
  addEventListener('TrunkApplicationStarted', () => stage('start'));
  // An inline module script (Trunk's loader) whose imports failed fires `error` on itself.
  document.addEventListener('error', (ev) => {
    const t = ev.target;
    if (!state.started && t && t.tagName === 'SCRIPT' && t.type === 'module' && !t.src) fail(new Error("the page's module script didn't load"));
  }, true);

  // The app started: it removed #photocraft_loading.
  const watch = setInterval(() => {
    if (el('photocraft_loading')) return;
    clearInterval(watch);
    state.started = true;
    try { sessionStorage.removeItem(RETRY); sessionStorage.removeItem(FROM_THREADS); } catch (x) {}
  }, 250);

  if (statusEl()) setStatus(text.loading);
  else document.addEventListener('DOMContentLoaded', () => { if (state.stage === 'html') setStatus(text.loading); });
  self.capishotBoot = { stage, progress, counted, fail, build, state };
  // The pre-boot.js name, still called by older pages' scripts.
  self.capishotLoadFailed = (why) => fail(why instanceof Error ? why : new Error(String(why)));
})();
