// One-frame page switches in a real browser (embed/README.md › Smooth page switching): a host
// page keeps one visible document (`editor.setLayout {single}`) and switches between pages kept
// as hidden helper documents, and between named views of one page:
// - `document.views.apply`: the next frame shows the new view, nothing in between, no history step;
// - `prewarmDocument` + `showDocument {viewport: 'keep'}`: time from the call to the first frame
//   showing the new page (target ≤ 100 ms), zoom and centre kept;
// - `showDocument` of a cold page (never drawn): double-buffered, the old page stays until the new
//   one is drawn;
// - `editor.setMemoryBudget` evicts the hidden pages' GPU textures (`document.memory`), and a
//   switch to an evicted page still shows no blank frame.
// Every animation frame the page reads the app's canvas back (a 5×5 grid of points inside the
// page) and classifies it: one page's colour, or "other" (blank, pasteboard, checkerboard or a
// mix: a partial frame). Serves the build itself. Either web build's `dist` directory.
//
//   CHROME=<chromium> node embed-doc-switch.mjs <dist dir>
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import zlib from 'node:zlib';

const [distArg] = process.argv.slice(2);
if (!distArg || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> node embed-doc-switch.mjs <dist dir>');
  process.exit(2);
}
// The single-threaded build (Trunk's dist/web) or the threads build (dist/web-threads: the module
// gzipped in v/<hash>/, started after its Web Worker pool).
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

// A solid-colour PNG, written here (no image files in the repository).
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const c = Buffer.alloc(4);
  c.writeUInt32BE(crc(td));
  return Buffer.concat([len, td, c]);
};
const solidPng = (w, h, [r, g, b]) => {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: w }, () => [r, g, b]).flat())]);
  const raw = Buffer.concat(Array.from({ length: h }, () => row));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
};
const W = 1200;
const H = 1800;
const COLORS = { red: [220, 30, 30], blue: [30, 40, 220], green: [30, 190, 60], yellow: [235, 220, 20] };
const pngs = Object.fromEntries(Object.entries(COLORS).map(([k, c]) => [k, solidPng(W, H, c).toString('base64')]));

const host = `<!doctype html><meta charset="utf-8"><body style="margin:0">
<div id="c" style="width:800px;height:600px"></div>
<script type="module">
// Canvas readback between frames: keep the WebGL drawing buffer (nothing else changes).
const getContext = HTMLCanvasElement.prototype.getContext;
HTMLCanvasElement.prototype.getContext = function (kind, attrs) {
  if (kind === 'webgl' || kind === 'webgl2') attrs = { ...(attrs || {}), preserveDrawingBuffer: true };
  return getContext.call(this, kind, attrs);
};
import load, * as cs from './${js}';
${threads
    ? `const raw = await fetch('./${wasm}');
await load({ module_or_path: new Response(raw.body.pipeThrough(new DecompressionStream('gzip')), { headers: { 'Content-Type': 'application/wasm' } }) });
await cs.initThreadPool(Math.max(1, Math.min((navigator.hardwareConcurrency || 4) - 1, 8)));
cs.threadPoolReady();`
    : `await load({ module_or_path: './${wasm}' });`}
window.cs = cs;
window.events = [];
cs.onEvent((e) => window.events.push(e));
await cs.init({ container: document.getElementById('c'), mode: 'embedded' });
// Every animation frame: its time and what the canvas shows at 25 points inside the page.
const probe = document.createElement('canvas');
probe.width = 80; probe.height = 60;
const pc = probe.getContext('2d', { willReadFrequently: true });
const kind = ([r, g, b]) => r > 150 && g > 150 && b < 100 ? 'yellow' : r > 150 && g < 100 && b < 100 ? 'red'
  : b > 150 && r < 100 && g < 120 ? 'blue' : g > 130 && r < 100 && b < 120 ? 'green' : 'other';
window.samples = [];
window.watching = false;
const tick = (t) => {
  if (window.watching) {
    const app = document.querySelector('#c canvas');
    let k = 'none';
    if (app && app.width > 0) {
      pc.drawImage(app, 0, 0, 80, 60);
      const d = pc.getImageData(0, 0, 80, 60).data;
      const seen = new Set();
      for (let i = 0; i < 5; i++) for (let j = 0; j < 5; j++) {
        const x = 34 + i * 3, y = 18 + j * 6, o = (y * 80 + x) * 4;
        seen.add(kind([d[o], d[o + 1], d[o + 2]]));
      }
      k = seen.size === 1 ? [...seen][0] : 'other';
    }
    window.samples.push([t, k]);
  }
  requestAnimationFrame(tick);
};
requestAnimationFrame(tick);
window.started = true;
</script>`;
const types = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.gz': 'application/octet-stream', '.html': 'text/html', '.json': 'application/json', '.png': 'image/png' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || '/').split('?')[0]);
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  if (url === '/host.html') {
    res.setHeader('Content-Type', 'text/html');
    return res.end(host);
  }
  const file = path.join(dist, path.normalize(url).replace(/^(\.\.[/\\])+/, ''));
  if (!file.startsWith(dist) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
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
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
const done = async () => {
  await b.close();
  server.close();
  console.log(failed ? `${failed} FAILED` : 'all passed');
  process.exit(failed ? 1 : 0);
};
await p.goto(`${base}/host.html`);
await p.waitForFunction(() => window.started, null, { timeout: 300000 });
const call = (fn, ...args) =>
  p.evaluate(async ([fn, args]) => {
    try {
      return { ok: true, value: await window.cs[fn](...args) };
    } catch (e) {
      return { ok: false, error: String(e) };
    }
  }, [fn, args]);
const ex = async (command, params = {}, options = null) => {
  const r = await call('execute', command, params, options);
  if (!r.ok) console.log(`  ${command}: ${r.error}`);
  return r;
};
const events = (type) => p.evaluate((t) => window.events.filter((e) => e.type === t), type);
const clear = () => p.evaluate(() => { window.events.length = 0; });
const frames = () => p.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(r, 200)))));
const shot = () => p.locator('#c canvas').screenshot();

const watch = () => p.evaluate(() => { window.samples.length = 0; window.watching = true; });
const stop = () => p.evaluate(() => { window.watching = false; return window.samples; });
// Run `fn` in the page while frames are sampled: the call's time and the frames after it.
const timed = (fn) => p.evaluate(async (src) => {
  const t0 = performance.now();
  const value = await (0, eval)(src)(window.cs);
  return { t0, t1: performance.now(), value };
}, fn);
const settle = () => frames().then(frames);
const summary = (samples, t0, want) => {
  const after = samples.filter(([t]) => t >= t0);
  const first = after.find(([, k]) => k === want);
  const others = after.filter(([t, k]) => (!first || t <= first[0]) && k === 'other').length;
  const late = first ? after.filter(([t, k]) => t > first[0] && k !== want).length : 0;
  return { ms: first ? Math.round(first[0] - t0) : null, frames: first ? after.indexOf(first) : null, others, late };
};
const fillPage = async (doc, color) => {
  const l = (await ex('layer.new.layer', { name: color }, { document: doc, history: 'none' })).value.layer;
  await ex('layer.putPixels', { layer: l, src: pngs[color] }, { document: doc, history: 'none' });
  return l;
};

const v = await call('version');
for (const f of ['document.views', 'editor.showDocument', 'editor.setLayout', 'document.prewarm', 'editor.setMemoryBudget', 'document.memory']) {
  check(v.ok && v.value.features.includes(f), `feature ${f}`);
}
check((await call('request', 'editor.setLayout', { single: true })).ok, 'editor.setLayout {single: true}');

// Page A (shown) with two views; pages B and C hidden, B with type.
const pageA = (await call('newDocument', { width: W, height: H, name: 'page 12', background: '#ffffff' })).value;
const raw = await fillPage(pageA.id, 'red');
const clean = await fillPage(pageA.id, 'yellow');
const pageB = (await call('newDocument', { width: W, height: H, name: 'page 13', hidden: true })).value;
await fillPage(pageB.id, 'blue');
await ex('type.create', { text: 'Hola', x: 60, y: 120, size: 72 }, { document: pageB.id, history: 'none' });
const pageC = (await call('newDocument', { width: W, height: H, name: 'page 14', hidden: true })).value;
await fillPage(pageC.id, 'green');
await ex('document.views.define', { name: 'raw', visible: [raw], hidden: [clean] });
await ex('document.views.define', { name: 'clean', visible: [clean] });
await ex('document.views.apply', { name: 'raw' });
await settle();
const hist0 = (await ex('document.inspect', {})).value.history.length;
await call('zoom', 0.25);
await settle();
const lastView = async (id) => (await events('viewChanged')).filter((e) => e.id === id).pop();
const view0 = await lastView(pageA.id);

// 1. Views: one change, the next frame shows it.
const lines = [];
for (const [name, want] of [['clean', 'yellow'], ['raw', 'red'], ['clean', 'yellow'], ['raw', 'red']]) {
  await watch();
  const r = await timed(`(cs) => cs.execute('document.views.apply', {name: '${name}'})`);
  await settle();
  const s = summary(await stop(), r.t0, want);
  lines.push(s);
  check(s.ms !== null && s.others === 0 && s.late === 0, `views.apply ${name}: ${s.ms} ms (${s.frames} frames) to the view, ${s.others} intermediate frames`);
}
check((await ex('document.inspect', {})).value.history.length === hist0, 'views.apply adds no history step');

// 2. Prefetched page: prewarm while A shows, then the switch.
await watch();
const warm = await timed(`(cs) => cs.prewarmDocument(${pageB.id}, {viewport: 'keep'})`);
await settle();
const duringWarm = (await stop()).filter(([, k]) => k !== 'red').length;
check(warm.value && duringWarm === 0, `prewarm: ${Math.round(warm.t1 - warm.t0)} ms (${warm.value && warm.value.frames} frames, ${warm.value && warm.value.gpuBytes} GPU bytes), page A on screen throughout`);
await watch();
const sw = await timed(`(cs) => cs.showDocument(${pageB.id}, {viewport: 'keep'})`);
await settle();
const sb = summary(await stop(), sw.t0, 'blue');
check(sb.ms !== null && sb.ms <= 100 && sb.others === 0 && sb.late === 0, `prefetched switch: ${sb.ms} ms to the final frame (${sb.frames} frames; reply ${Math.round(sw.t1 - sw.t0)} ms, waited ${sw.value.frames} app frames), ${sb.others} blank/partial frames`);
const view1 = await lastView(pageB.id);
const st1 = (await call('getState')).value;
check(st1.active === pageB.id && st1.documents.filter((d) => !d.hidden).length === 1, 'one shown document: the new page; page A is a hidden helper');
const same = (x, y) => Math.abs(x - y) < 1e-3;
check(view0 && view1 && same(view0.zoom, view1.zoom) && same(view0.center[0], view1.center[0]) && same(view0.center[1], view1.center[1]),
  `zoom and centre kept (${view0 && view0.zoom}@${view0 && view0.center} → ${view1 && view1.zoom}@${view1 && view1.center})`);

// 3. Cold page: drawn offscreen first, page B stays until then.
await watch();
const cold = await timed(`(cs) => cs.showDocument(${pageC.id}, {viewport: 'keep'})`);
await settle();
const sc = summary(await stop(), cold.t0, 'green');
check(sc.ms !== null && sc.others === 0 && sc.late === 0, `cold switch: ${sc.ms} ms to the final frame (${sc.frames} frames, ${cold.value.frames} app frames drawn offscreen), ${sc.others} blank/partial frames`);

// 4. Back to page A, kept warm while hidden: swapped in the frame of the call.
await watch();
const back = await timed(`(cs) => cs.showDocument(${pageA.id}, {viewport: 'keep'})`);
await settle();
const sa = summary(await stop(), back.t0, 'red');
check(back.value.frames === 0 && sa.ms !== null && sa.others === 0, `back to the warm page: ${sa.ms} ms, ${back.value.frames} app frames waited, ${sa.others} blank/partial frames`);

// 5. Memory budget: the hidden pages' textures go, the pages stay; switching still shows no blank.
const mem0 = (await call('request', 'document.memory', {})).value;
const budget = (await call('request', 'editor.setMemoryBudget', { mb: 1 })).value;
const mem1 = (await call('request', 'document.memory', {})).value;
const warmOf = (m) => m.documents.map((d) => `${d.id}:${d.warm ? 'warm' : 'cold'}`).join(' ');
check(budget.evicted.length >= 1 && mem1.documents.filter((d) => d.warm).length === 1 && mem1.documents.length === 3, `setMemoryBudget 1 MB evicted ${JSON.stringify(budget.evicted)} (${warmOf(mem0)} → ${warmOf(mem1)}; ${mem0.gpuMB} → ${mem1.gpuMB} MB)`);
await watch();
const ev = await timed(`(cs) => cs.showDocument(${pageB.id}, {viewport: 'keep'})`);
await settle();
const se = summary(await stop(), ev.t0, 'blue');
check(se.ms !== null && se.others === 0, `switch to an evicted page: ${se.ms} ms, ${ev.value.frames} app frames, ${se.others} blank/partial frames`);
await call('request', 'editor.setMemoryBudget', { mb: null });

const end = (await call('getState')).value;
check(end.documents.length === 3 && end.documents.filter((d) => !d.hidden).length === 1, 'three pages open, one shown');
console.log(`  ${threads ? 'threads' : 'st'} build: views ${JSON.stringify(lines.map((l) => l.ms))} ms, prefetched ${sb.ms} ms, cold ${sc.ms} ms, warm back ${sa.ms} ms, evicted ${se.ms} ms`);
await done();
