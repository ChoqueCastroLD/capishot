// Hidden helper documents, batches and whole-layer replaces in a real browser (embed/README.md ›
// Smooth page switching): a host page starts Capishot on its own canvas with one shown page, then
// prepares other pages in hidden documents (`openDocument(…, {hidden})`, `newDocument({hidden})`),
// edits them with `batch` and `layer.putPixels {src}` (base64, Blob, ArrayBuffer and URL), copies
// layers out of them with `layer.copyToDocument {fromDocument, toDocument}`, and checks that the
// person never sees any of it: the shown page's pixels stay identical in every screenshot taken
// while the work runs (no blank, partial or flickering frame), no `documents`/`viewChanged`
// event, the active document, tool and view unchanged. Frame timing comes from a rAF loop in the
// page (the longest frame while the hidden work runs) and each call is timed.
// Serves the build itself, with a host page of its own. Either web build's `dist` directory.
//
//   CHROME=<chromium> node embed-hidden-batch.mjs <dist dir>
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import zlib from 'node:zlib';

const [dist] = process.argv.slice(2);
if (!dist || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> node embed-hidden-batch.mjs <dist dir>');
  process.exit(2);
}
const files = fs.readdirSync(dist);
const js = files.find((f) => /^photocraft-web-.*\.js$/.test(f));
const wasm = files.find((f) => /^photocraft-web-.*_bg\.wasm$/.test(f));
if (!js || !wasm) {
  console.error(`no photocraft-web-*.js / *_bg.wasm in ${dist}`);
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
const servedPng = solidPng(W, H, [40, 160, 90]);

const host = `<!doctype html><meta charset="utf-8"><body style="margin:0">
<div id="c" style="width:800px;height:600px"></div>
<script type="module">
import load, * as cs from './${js}';
await load({ module_or_path: './${wasm}' });
window.cs = cs;
window.events = [];
cs.onEvent((e) => window.events.push(e));
await cs.init({ container: document.getElementById('c'), mode: 'embedded' });
// Frame timing: every animation frame's start, while 'watching'.
window.frameTimes = [];
window.watching = false;
const tick = (t) => { if (window.watching) window.frameTimes.push(t); requestAnimationFrame(tick); };
requestAnimationFrame(tick);
window.started = true;
</script>`;
const types = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html', '.json': 'application/json', '.png': 'image/png' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || '/').split('?')[0]);
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  if (url === '/host.html') {
    res.setHeader('Content-Type', 'text/html');
    return res.end(host);
  }
  if (url === '/clean.png') {
    res.setHeader('Content-Type', 'image/png');
    return res.end(servedPng);
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
const b64 = servedPng.toString('base64');

const v = await call('version');
for (const f of ['documents.hidden', 'layer.copyToDocument.ids', 'batch', 'layer.setPropsMany', 'layer.putPixels.src']) {
  check(v.ok && v.value.features.includes(f), `feature ${f}`);
}

// The shown page, settled.
const page = (await call('newDocument', { width: W, height: H, name: 'page 12', background: '#ffffff' })).value;
await ex('layer.new.layer', { name: 'ink' });
await ex('select.rect', { x: 100, y: 100, width: 300, height: 200 });
await ex('edit.fill', { contents: 'black' });
await ex('select.deselect');
await frames();
await frames();
const state0 = (await call('getState')).value;
const baseline = await shot();
await clear();

// Hidden pages, edited while the person looks at page 12.
await p.evaluate(() => { window.frameTimes.length = 0; window.watching = true; });
const t0 = Date.now();
const work = p.evaluate(async ([b64, W, H]) => {
  const cs = window.cs;
  const timing = {};
  const time = async (k, f) => { const t = performance.now(); const r = await f(); timing[k] = Math.round(performance.now() - t); return r; };
  const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  const h1 = await time('openHidden', () => cs.openDocument(new Blob([bytes], { type: 'image/png' }), 'p13.png', { hidden: true }));
  const h2 = await time('newHidden', () => cs.newDocument({ width: W, height: H, name: 'p14', hidden: true }));
  const l2 = (await cs.execute('layer.new.layer', {}, { document: h2.id })).layer;
  await time('putPixelsBase64', () => cs.execute('layer.putPixels', { layer: l2, src: b64 }, { document: h2.id, history: 'none' }));
  await time('putPixelsBlob', () => cs.execute('layer.putPixels', { layer: l2, src: new Blob([bytes]) }, { document: h2.id, history: 'none' }));
  await time('putPixelsArrayBuffer', () => cs.execute('layer.putPixels', { layer: l2, src: bytes.buffer }, { document: h2.id, history: 'none' }));
  await time('putPixelsUrl', () => cs.execute('layer.putPixels', { layer: l2, src: location.origin + '/clean.png' }, { document: h2.id, history: 'none' }));
  const batch = await time('batch20', () => cs.batch(Array.from({ length: 20 }, (_, i) => ({ command: 'layer.new.layer', params: { name: `t${i}` } })), { document: h2.id, history: 'none', inspect: true }));
  const all = batch.inspect.layers.length;
  const ids = batch.inspect.layers.filter((l) => /^t\d+$/.test(l.name)).map((l) => l.id);
  await time('setPropsMany', () => cs.execute('layer.setPropsMany', { layers: ids.map((layer, i) => ({ layer, opacity: (i % 10) / 10 + 0.05, visible: i % 2 === 0 })) }, { document: h2.id }));
  return { h1, h2, l2, timing, layers: all, made: ids.length };
}, [b64, W, H]);
// Screenshots while the hidden work runs: each must be the settled page.
const shots = [];
let finished = false;
work.then(() => { finished = true; }, () => { finished = true; });
while (!finished && shots.length < 40) shots.push(await shot());
const r = await work;
const elapsed = Date.now() - t0;
await frames();
shots.push(await shot());
const ft = await p.evaluate(() => { window.watching = false; return window.frameTimes; });
const gaps = ft.slice(1).map((t, i) => t - ft[i]);
const longest = gaps.length ? Math.max(...gaps) : 0;
console.log(`  hidden work: ${elapsed} ms, ${JSON.stringify(r.timing)}, ${shots.length} screenshots, ${gaps.length} frames, longest ${longest.toFixed(1)} ms`);
check(shots.every((s) => s.equals(baseline)), `the shown page never changed on screen (${shots.filter((s) => !s.equals(baseline)).length} of ${shots.length} differ)`);
check(r.h1.hidden === true && r.h1.active === false && r.h2.hidden === true, 'hidden documents');
const st = (await call('getState')).value;
check(st.active === page.id && st.tool === state0.tool, 'the active document and tool stay');
check((await events('documents')).length === 0, 'no documents event');
check((await events('viewChanged')).length === 0 && (await events('zoom')).length === 0, 'no view change');
check(st.documents.filter((d) => d.hidden).length === 2 && st.documents.length === 3, 'listDocuments reports the hidden documents');
check(!(await call('activateDocument', r.h1.id)).ok, 'activateDocument refuses a hidden document');
const h2 = (await ex('document.inspect', {}, { document: r.h2.id })).value;
check(h2 && r.made === 20 && h2.history.length === 3 && h2.layers.length === r.layers, `the hidden page's edits (history ${h2 && h2.history.length}, layers ${h2 && h2.layers.length})`);

// Copy a prepared layer out of the hidden page: one step on the shown page, nothing activated.
await clear();
const before = (await ex('document.inspect', {})).value;
const tc = Date.now();
const copied = await ex('layer.copyToDocument', { fromDocument: r.h2.id, toDocument: page.id, layers: [r.l2] });
const copyMs = Date.now() - tc;
await frames();
const after = (await ex('document.inspect', {})).value;
check(copied.ok && after.layers.length === before.layers.length + 1 && after.history.length === before.history.length + 1, `layer.copyToDocument by id (${copyMs} ms)`);
check((await events('documents')).length === 0 && (await call('getState')).value.active === page.id, 'copyToDocument activates nothing');

// A batch on the shown page: one undo step, drawn once; a failing one changes nothing.
const tb = Date.now();
const one = await call('batch', [{ command: 'layer.setProps', params: { layer: copied.value.layers[0], opacity: 0.5 } }, { command: 'layer.new.layer', params: { name: 'b' } }], { history: 'one' });
const batchMs = Date.now() - tb;
const afterBatch = (await ex('document.inspect', {})).value;
check(one.ok && afterBatch.history.length === after.history.length + 1, `batch: one undo step (${batchMs} ms)`);
const bad = await call('batch', [{ command: 'layer.new.layer' }, { command: 'layer.setProps', params: { layer: 999999, opacity: 0 } }]);
const afterBad = (await ex('document.inspect', {})).value;
check(!bad.ok && afterBad.layers.length === afterBatch.layers.length && afterBad.history.length === afterBatch.history.length, 'a failing batch is undone');
check(!(await call('batch', [{ command: 'file.new' }])).ok && !(await call('batch', [], { render: 'later' })).ok, 'bad batches are refused');

// window.photocraft-style hidden open is covered by the same core; close the helpers.
check((await call('closeDocument', r.h1.id, true)).ok && (await call('closeDocument', r.h2.id, true)).ok, 'hidden documents close');
const end = (await call('getState')).value;
check(end.active === page.id && end.documents.length === 1, 'the shown page is the one left');
await done();
