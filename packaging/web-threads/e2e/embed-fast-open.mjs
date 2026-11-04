// Faster opening in a real browser (embed/README.md › Opening pages fast): a host page starts
// Capishot on its own canvas (the embedding module), makes a 2x manga page PSD (2480×3508, eight
// layers, three of them hidden) and measures, sampling the canvas every animation frame (drawn
// into a 48×64 bitmap; "usable" = at least 90 % of its cells look as in the final frame):
//   - a cold `openDocument(bytes)` (opening card over the merged preview), and a quiet one
//     (`{quiet: true}`, the merged preview alone): the first usable frame (≤ 500 ms for the
//     quiet one), the final frame, no blank or partial frame while the preview stands in, and the
//     frames that differ from the final while the new document's canvas composites (reported;
//     none for the quiet one: its preview stays until the canvas is drawn, `open.quiet.prewarm`);
//     the promise settles with the document's id once it is in place (`open.whenOpen`);
//   - `openDocument(…, {cacheKey, version})` again: from the decoded-document cache (`cached`),
//     at least 3× faster than the cold open; the same key opened hidden (a raw helper) shares the
//     page's pixels: `perf.memory`'s `pixelsMB` doesn't grow; a stale version without bytes is
//     refused (`open.cache`, `open.cache.sharedPixels`);
//   - `openDocument(url)`: the file streamed in while the page keeps drawing (`open.url`);
//   - a hidden helper PSD (`{hidden: true}`) beside a shown page: decoded off the frame, the
//     canvas never changes, no `documents` event, the active page stays (`open.hidden.background`).
// PAGE=<file.psd> also times a real page (never committed); DEBUG=1 prints the quiet open's
// frame timeline, SHOTS=<dir> saves screenshots of its preview and final frame. Either web build's
// `dist` directory.
//
//   CHROME=<chromium> [PAGE=<file.psd>] node embed-fast-open.mjs <dist dir>
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const [distArg] = process.argv.slice(2);
if (!distArg || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> [PAGE=<file.psd>] node embed-fast-open.mjs <dist dir>');
  process.exit(2);
}
const dist = path.resolve(distArg);
const files = fs.readdirSync(dist);
const version = fs.existsSync(path.join(dist, 'v')) ? fs.readdirSync(path.join(dist, 'v'))[0] : null;
const threads = !!version;
const js = threads ? `v/${version}/photocraft.js` : files.find((f) => /^photocraft-web-.*\.js$/.test(f));
const wasm = threads ? `v/${version}/photocraft_bg.wasm.gz` : files.find((f) => /^photocraft-web-.*_bg\.wasm$/.test(f));
if (!js || !wasm) {
  console.error(`no photocraft-web-*.js / *_bg.wasm (or v/<hash>/photocraft.js) in ${dist}`);
  process.exit(2);
}
const realPage = process.env.PAGE ? fs.readFileSync(process.env.PAGE) : null;

// The canvas sampler: every animation frame the canvas is drawn into a 48×64 bitmap; a sample is
// the share of its cells that differ from the workspace backdrop (the page's coverage) and a
// digest of them. `window.samples` holds `{t, cover, digest}` while `window.watching`.
const host = `<!doctype html><meta charset="utf-8"><body style="margin:0">
<div id="c" style="width:800px;height:600px"></div>
<script type="module">
import load, * as cs from './${js}';
${threads
    ? `const raw = await fetch('./${wasm}');
await load({ module_or_path: new Response(raw.body.pipeThrough(new DecompressionStream('gzip')), { headers: { 'Content-Type': 'application/wasm' } }) });
await cs.initThreadPool(Math.max(1, Math.min((navigator.hardwareConcurrency || 4) - 1, 8)));
cs.threadPoolReady();`
    : `await load({ module_or_path: './${wasm}' });`}
window.cs = cs;
window.events = [];
cs.onEvent((e) => window.events.push({ ...e, at: performance.now() }));
await cs.init({ container: document.getElementById('c'), mode: 'embedded', fontsNotice: false, locale: 'en' });
const probe = document.createElement('canvas');
probe.width = 48;
probe.height = 64;
const g = probe.getContext('2d', { willReadFrequently: true });
window.samples = [];
window.watching = false;
window.backdrop = null;
const sample = () => {
  const c = document.querySelector('#c canvas');
  g.drawImage(c, 0, 0, 48, 64);
  const d = g.getImageData(0, 0, 48, 64).data;
  return d;
};
window.takeBackdrop = () => { window.backdrop = sample(); };
const tick = (t) => {
  if (window.watching) {
    const d = sample();
    const b = window.backdrop;
    let cover = 0, digest = 0;
    for (let i = 0; i < d.length; i += 4) {
      const off = !b || Math.abs(d[i] - b[i]) + Math.abs(d[i + 1] - b[i + 1]) + Math.abs(d[i + 2] - b[i + 2]) > 24;
      if (off) cover++;
      digest = (digest * 31 + (d[i] >> 4) * 7 + (d[i + 1] >> 4) * 3 + (d[i + 2] >> 4)) >>> 0;
    }
    window.samples.push({ t: performance.now(), cover: cover / (d.length / 4), digest, d });
  }
  requestAnimationFrame(tick);
};
requestAnimationFrame(tick);
window.started = true;
</script>`;

// The page's PSD, uploaded by the browser once made (POST /upload), served back at /page.psd a
// chunk at a time (the stream arrives over about 300 ms).
let served = null;
const types = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.gz': 'application/octet-stream', '.html': 'text/html', '.json': 'application/json' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || '/').split('?')[0]);
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  if (url === '/host.html') {
    res.setHeader('Content-Type', 'text/html');
    return res.end(host);
  }
  if (url === '/upload' && req.method === 'POST') {
    const parts = [];
    req.on('data', (d) => parts.push(d));
    req.on('end', () => {
      served = Buffer.concat(parts);
      res.end('ok');
    });
    return;
  }
  if (url === '/page.psd' && served) {
    res.setHeader('Content-Type', 'application/octet-stream');
    res.setHeader('Content-Length', served.length);
    const n = 16;
    const step = Math.ceil(served.length / n);
    let i = 0;
    const next = () => {
      if (i >= n) return res.end();
      res.write(served.subarray(i * step, (i + 1) * step));
      i++;
      setTimeout(next, 20);
    };
    return next();
  }
  const file = path.join(dist, path.normalize(url).replace(/^(\.\.[/\\])+/, ''));
  if (!file.startsWith(path.resolve(dist)) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    res.statusCode = 404;
    return res.end();
  }
  res.setHeader('Content-Type', types[path.extname(file)] || 'application/octet-stream');
  fs.createReadStream(file).pipe(res);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const p = await (await b.newContext({ viewport: { width: 800, height: 600 } })).newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
if (process.env.DEBUG) {
  p.on('console', (m) => console.log('console:', m.text().slice(0, 300)));
  p.on('requestfailed', (r) => console.log('requestfailed:', r.url(), r.failure()?.errorText));
  p.on('response', (r) => console.log('response:', r.status(), r.url().slice(-60)));
}
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
const numbers = [];
const report = (what) => {
  console.log(`  ${what}`);
  numbers.push(what);
};
await p.goto(`${base}/host.html`);
await p.waitForFunction(() => window.started, null, { timeout: 300000 });
const build = threads ? 'threads' : 'st';

// The page: white paper, panels with screentone noise and line art, speech bubbles; a raw layer,
// a clean layer, two bubble/text layers, a shading layer and three hidden layers.
const made = await p.evaluate(async () => {
  const [W, H] = [2480, 3508];
  const layer = (draw) => {
    const c = new OffscreenCanvas(W, H);
    const g = c.getContext('2d');
    draw(g);
    return c.convertToBlob({ type: 'image/png' });
  };
  let seed = 7;
  const rnd = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
  const art = (g, tone) => {
    g.fillStyle = '#fff';
    g.fillRect(0, 0, W, H);
    g.lineWidth = 8;
    for (let r = 0; r < 4; r++) for (let c = 0; c < 2; c++) {
      const [x, y, w, h] = [80 + c * 1180, 80 + r * 840, 1140, 800];
      g.strokeStyle = '#000';
      g.strokeRect(x, y, w, h);
      for (let i = 0; i < 4000; i++) {
        g.fillStyle = `rgba(0,0,0,${tone * rnd()})`;
        g.fillRect(x + rnd() * w, y + rnd() * h, 6, 6);
      }
      g.lineWidth = 3;
      g.beginPath();
      for (let i = 0; i < 60; i++) { g.moveTo(x + rnd() * w, y + rnd() * h); g.quadraticCurveTo(x + rnd() * w, y + rnd() * h, x + rnd() * w, y + rnd() * h); }
      g.stroke();
      g.lineWidth = 8;
    }
  };
  const bubbles = (g, n) => {
    for (let i = 0; i < n; i++) {
      const [x, y] = [200 + rnd() * 2000, 200 + rnd() * 3000];
      g.fillStyle = '#fff';
      g.strokeStyle = '#000';
      g.lineWidth = 6;
      g.beginPath();
      g.ellipse(x, y, 220, 140, 0, 0, Math.PI * 2);
      g.fill();
      g.stroke();
      g.fillStyle = '#000';
      g.font = 'bold 44px sans-serif';
      g.fillText('LOREM IPSUM', x - 150, y - 10);
      g.fillText('DOLOR SIT!', x - 120, y + 45);
    }
  };
  const blobs = await Promise.all([
    layer((g) => art(g, 0.5)),
    layer((g) => art(g, 0.15)),
    layer((g) => bubbles(g, 8)),
    layer((g) => bubbles(g, 6)),
    layer((g) => { g.fillStyle = 'rgba(40,60,200,0.25)'; g.fillRect(80, 80, 1140, 800); g.fillRect(1260, 1760, 1140, 800); }),
    layer((g) => art(g, 0.8)),
    layer((g) => bubbles(g, 10)),
    layer((g) => art(g, 0.3)),
  ]);
  const doc = await window.cs.newDocument({ width: W, height: H, name: 'page' });
  const names = ['Raw', 'Clean', 'Text 1', 'Text 2', 'Shading', 'Raw alt', 'Text old', 'Clean alt'];
  for (let i = 0; i < blobs.length; i++) {
    await window.cs.execute('layer.new.layer', { name: names[i] });
    await window.cs.execute('layer.putPixels', { layer: names[i], src: blobs[i] });
    if (i >= 5) await window.cs.execute('layer.setProps', { layer: names[i], visible: false });
  }
  const bytes = await window.cs.saveDocument(doc.id, 'psd');
  await window.cs.closeDocument(doc.id, true);
  await fetch('/upload', { method: 'POST', body: bytes });
  window.pagePsd = bytes;
  return { size: bytes.length, id: doc.id };
});
check(made.size > 1e6, `made a 2480×3508 eight-layer PSD (${(made.size / 1e6).toFixed(1)} MB)`);

// A small shown page first, so the canvas has a workspace and a backdrop to compare against.
await p.evaluate(async () => {
  await window.cs.newDocument({ width: 600, height: 400, name: 'start' });
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  await window.cs.closeDocument((await window.cs.listDocuments())[0].id, true);
  await new Promise((r) => setTimeout(r, 300));
  window.takeBackdrop();
});

// Opens `src` (bytes in window.pagePsd, a URL, or null for the cache) and watches the canvas until
// the final frame: {settledMs, firstMs, finalMs, blankAfterFirst, longestFrame, result, ready}.
const timedOpen = (src, name, options) =>
  p.evaluate(
    async ([src, name, options]) => {
      window.samples.length = 0;
      window.events.length = 0;
      const data = src === 'bytes' ? window.pagePsd : src === 'real' ? window.realPsd : src;
      window.watching = true;
      const t0 = performance.now();
      let result, error;
      try {
        result = await window.cs.openDocument(data, name, options);
      } catch (e) {
        error = String(e);
      }
      const settledMs = performance.now() - t0;
      // Until documentReady (complete: no job, fonts settled) and a few frames more.
      const until = performance.now() + 20000;
      while (performance.now() < until && !window.events.some((e) => e.type === 'documentReady' && e.at >= t0)) await new Promise((r) => setTimeout(r, 20));
      for (let i = 0; i < 6; i++) await new Promise((r) => requestAnimationFrame(r));
      window.watching = false;
      const s = window.samples.filter((x) => x.t >= t0);
      const final = s[s.length - 1];
      // How much of a frame looks like the final one: the share of its cells within a small
      // tolerance (a reduced preview of the page matches; the workspace, a card over the page
      // or a page half drawn doesn't).
      const like = (x) => {
        let n = 0;
        for (let i = 0; i < x.d.length; i += 4) if (Math.abs(x.d[i] - final.d[i]) + Math.abs(x.d[i + 1] - final.d[i + 1]) + Math.abs(x.d[i + 2] - final.d[i + 2]) <= 60) n++;
        return n / (x.d.length / 4);
      };
      // Usable: the page shows as it will (≥ 90 % of the cells); the final frame: identical.
      const first = s.find((x) => like(x) >= 0.9);
      const firstFinal = s.find((x) => x.digest === final.digest);
      const after = first ? s.filter((x) => x.t >= first.t) : [];
      const gaps = s.slice(1).map((x, i) => x.t - s[i].t);
      // The document's own first frame (documentReady comes at the end of its first complete
      // frame): before it, the preview; from it, the canvas compositing the new document.
      const ready = window.events.find((e) => e.type === 'documentReady' && e.at >= t0);
      const docAt = ready ? ready.at - (ready.finalFrameMs - ready.firstFrameMs) - 4 : Infinity;
      const blankAfterFirst = after.filter((x) => x.t < docAt && like(x) < 0.9).length;
      const unlikeAfterFirst = after.filter((x) => like(x) < 0.9).length;
      const partialAtSwap = s.filter((x) => x.t >= docAt && x.t < firstFinal.t && like(x) < 0.9).length;
      const timeline = s.map((x) => `${(x.t - t0).toFixed(0)}:${like(x).toFixed(2)}/${x.cover.toFixed(2)}`).join(' ');
      for (const x of window.samples) delete x.d;
      return {
        settledMs,
        firstMs: first ? first.t - t0 : null,
        finalMs: firstFinal ? firstFinal.t - t0 : null,
        blankAfterFirst,
        partialAtSwap,
        unlikeAfterFirst,
        timeline,
        longestFrame: Math.max(0, ...gaps),
        cover: final.cover,
        result,
        error,
        ready: ready || null,
        documentsEvents: window.events.filter((e) => e.type === 'documents').length,
      };
    },
    [src, name, options],
  );
const fmt = (r) =>
  `settled ${r.settledMs.toFixed(0)} ms, first usable frame ${r.firstMs?.toFixed(0)} ms, final frame ${r.finalMs?.toFixed(0)} ms, longest frame ${r.longestFrame.toFixed(0)} ms, ` +
  `frames not like the final while the new document's canvas composites: ${r.partialAtSwap}, documentReady ${JSON.stringify(r.ready && { first: r.ready.firstFrameMs, final: r.ready.finalFrameMs })}`;
const closeAll = () => p.evaluate(async () => { for (const d of await window.cs.listDocuments()) await window.cs.closeDocument(d.id, true); });
const memory = () => p.evaluate(() => window.cs.request('perf.memory', {}).then((m) => m.pixelsMB));

// 1. Cold open from bytes, kept in the cache.
const cold = await timedOpen('bytes', 'page.psd', { cacheKey: 'p13', version: 'a1' });
check(!cold.error && cold.result && typeof cold.result.id === 'number' && cold.result.width === 2480 && cold.result.active, `cold open settles with the document in place: ${JSON.stringify(cold.result || cold.error).slice(0, 160)}`);
check(cold.result && cold.result.cached === false, 'decoded, not from the cache');
report(`${build} cold open (2480×3508, 8 layers; opening card over the preview): ${fmt(cold)}`);
check(cold.blankAfterFirst === 0, `no blank or partial frame once the page shows, before the document lands (${cold.blankAfterFirst})`);

// 2. From the cache: at once, sharing the pixels.
const info0 = await p.evaluate(() => window.cs.request('document.cache', {}));
const memBefore = await memory();
const warm = await timedOpen(null, 'page.psd', { cacheKey: 'p13', version: 'a1' });
check(!warm.error && warm.result?.cached === true && warm.result.id !== cold.result?.id, `reopened from the cache with a fresh id: ${JSON.stringify(warm.result || warm.error).slice(0, 120)}`);
report(`${build} cached reopen: ${fmt(warm)}`);
if (process.env.DEBUG) console.log(JSON.stringify((await p.evaluate(() => window.cs.request('ui.inspect', {}))).perf?.slowFrames?.slice(-3)));
// Nothing is decoded: what is left is the new document's first composite (a page already open
// switches in a frame or two: measured below).
check(warm.firstMs != null && warm.firstMs * 3 <= cold.finalMs, `cached reopen at least 3× faster than the cold open (${warm.firstMs?.toFixed(0)} vs ${cold.finalMs?.toFixed(0)} ms)`);
const raw = await p.evaluate(() => window.cs.openDocument(null, 'raw.psd', { cacheKey: 'p13', version: 'a1', hidden: true }).then((r) => r, (e) => ({ err: String(e) })));
const memAfter = await memory();
check(raw.hidden === true && raw.cached === true && !raw.active, `the raw helper from the same entry is hidden: ${JSON.stringify(raw).slice(0, 120)}`);
report(`${build} page + cached copy + hidden raw copy: pixels in memory ${memBefore} → ${memAfter} MB (each copy alone holds ${info0.entries?.[0]?.mb} MB)`);
check(memAfter - memBefore < 1, `copies share the decoded pixels (pixels +${(memAfter - memBefore).toFixed(1)} MB)`);
const info = await p.evaluate(() => window.cs.request('document.cache', {}));
check(info.entries?.length === 1 && info.entries[0].key === 'p13' && info.entries[0].version === 'a1', `document.cache lists the entry: ${JSON.stringify(info)}`);
// For scale: switching back to the cold page, already open (the canvas draws an open page).
const sw = await p.evaluate(async (id) => {
  const t0 = performance.now();
  await window.cs.activateDocument(id);
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  return performance.now() - t0;
}, cold.result?.id);
report(`${build} switch to the open page, two frames later: ${sw.toFixed(0)} ms`);
const stale = await p.evaluate(() => window.cs.openDocument(null, 'x.psd', { cacheKey: 'p13', version: 'b2' }).then(() => 'opened', (e) => String(e)));
check(/isn't in the document cache/.test(stale), `a stale version without bytes is refused: ${stale}`);
await closeAll();

// 3. From a URL, streamed in.
const url = await timedOpen(`${base}/page.psd`, null, {});
check(!url.error && url.result?.name === 'page.psd' && url.result.width === 2480, `opened from a URL, named after it: ${JSON.stringify(url.result || url.error).slice(0, 120)}`);
report(`${build} URL open (16 chunks over ~320 ms): ${fmt(url)}`);
check(url.longestFrame < 250, `the page keeps drawing while it downloads and opens (longest frame ${url.longestFrame.toFixed(0)} ms)`);
await closeAll();

// 4. A quiet cold open: the merged image alone while the layers decode.
if (process.env.SHOTS) {
  p.evaluate(() => window.cs.openDocument(window.pagePsd, 'shot.psd', { quiet: true }));
  await p.waitForTimeout(250);
  await p.screenshot({ path: `${process.env.SHOTS}/quiet-preview.png` });
  await p.waitForTimeout(3000);
  await p.screenshot({ path: `${process.env.SHOTS}/quiet-final.png` });
  await closeAll();
}
const quiet = await timedOpen('bytes', 'quiet.psd', { quiet: true });
check(!quiet.error && quiet.result?.width === 2480, `quiet open: ${quiet.error || 'opened'}`);
report(`${build} quiet cold open (merged preview alone): ${fmt(quiet)}`);
if (process.env.DEBUG) console.log(quiet.timeline);
check(quiet.firstMs != null && quiet.firstMs <= 500, `first usable frame ≤ 500 ms (${quiet.firstMs?.toFixed(0)} ms)`);
check(quiet.blankAfterFirst === 0, `the preview stays, no blank or partial frame, until the document lands (${quiet.blankAfterFirst})`);
check(quiet.unlikeAfterFirst === 0, `no half-composited frame once the page shows, the landing included (open.quiet.prewarm; ${quiet.unlikeAfterFirst})`);
await closeAll();

// 5. A hidden helper PSD beside a shown page: decoded off the frame, nothing shown.
await p.evaluate(async () => {
  await window.cs.newDocument({ width: 1200, height: 1800, name: 'shown', background: 'white' });
  for (let i = 0; i < 4; i++) await new Promise((r) => requestAnimationFrame(r));
});
const hidden = await p.evaluate(async () => {
  const active = (await window.cs.listDocuments()).find((d) => d.active).id;
  window.samples.length = 0;
  window.events.length = 0;
  window.watching = true;
  const t0 = performance.now();
  const r = await window.cs.openDocument(window.pagePsd, 'raw.psd', { hidden: true });
  const ms = performance.now() - t0;
  for (let i = 0; i < 4; i++) await new Promise((res) => requestAnimationFrame(res));
  window.watching = false;
  const s = window.samples;
  const gaps = s.slice(1).map((x, i) => x.t - s[i].t);
  const digests = new Set(s.map((x) => x.digest));
  return { r, ms, longest: Math.max(0, ...gaps), frames: s.length, digests: digests.size, active, nowActive: (await window.cs.listDocuments()).find((d) => d.active).id, documents: window.events.filter((e) => e.type === 'documents' || e.type === 'documentShown').length };
});
check(hidden.r.hidden === true && hidden.r.active === false && hidden.nowActive === hidden.active, `hidden helper opened, the shown page stays active: ${JSON.stringify(hidden.r).slice(0, 100)}`);
check(hidden.digests === 1 && hidden.documents === 0, `the canvas never changed (${hidden.digests} distinct frames) and no documents event (${hidden.documents})`);
report(`${build} hidden PSD helper: settled ${hidden.ms.toFixed(0)} ms over ${hidden.frames} frames, longest frame ${hidden.longest.toFixed(0)} ms`);
await closeAll();

// 6. A real page, when given.
if (realPage) {
  await p.evaluate(async (b64) => { window.realPsd = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)); }, realPage.toString('base64'));
  const real = await timedOpen('real', path.basename(process.env.PAGE), { quiet: true });
  check(!real.error, `real page opened: ${real.error || ''}`);
  report(`${build} real page ${path.basename(process.env.PAGE)} (${(realPage.length / 1e6).toFixed(1)} MB): ${fmt(real)}`);
  check(real.blankAfterFirst === 0, `real page: the preview stays until the document lands (${real.blankAfterFirst})`);
  await closeAll();
}

await b.close();
server.close();
console.log(failed ? `${failed} FAILED` : 'all passed');
process.exit(failed ? 1 : 0);
