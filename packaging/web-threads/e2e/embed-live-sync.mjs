// The embedding API's live-sync calls in a real browser (docs/control-protocol.md › Live sync): a
// host page loads the module and starts Capishot on its own canvas, then checks `execute`'s
// `document` and `history` options (a change on a background document without a switch or an
// undo step), `layer.putPixels`, `layer.layerMask.set`, `selection.get`/`selection.set`,
// `layer.selectMany`, and the `layerPixelsChanged`, `layersReordered` and `toolChanged` events.
// Serves the build itself, with a host page of its own. Either web build's `dist` directory.
//
//   CHROME=<chromium> node embed-live-sync.mjs <dist dir>
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const [dist] = process.argv.slice(2);
if (!dist || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> node embed-live-sync.mjs <dist dir>');
  process.exit(2);
}
const files = fs.readdirSync(dist);
const js = files.find((f) => /^photocraft-web-.*\.js$/.test(f));
const wasm = files.find((f) => /^photocraft-web-.*_bg\.wasm$/.test(f));
if (!js || !wasm) {
  console.error(`no photocraft-web-*.js / *_bg.wasm in ${dist}`);
  process.exit(2);
}
const host = `<!doctype html><meta charset="utf-8"><body style="margin:0">
<div id="c" style="width:800px;height:600px"></div>
<script type="module">
import load, * as cs from './${js}';
await load({ module_or_path: './${wasm}' });
window.cs = cs;
window.events = [];
cs.onEvent((e) => window.events.push(e));
await cs.init({ container: document.getElementById('c'), mode: 'embedded' });
window.started = true;
</script>`;
const types = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html', '.json': 'application/json' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || '/').split('?')[0]);
  // Cross-origin isolation, for the threads build.
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  if (url === '/host.html') {
    res.setHeader('Content-Type', 'text/html');
    return res.end(host);
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
// Every call returns {ok, value | error}.
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
const png = (w, h, color) =>
  p.evaluate(async ([w, h, color]) => {
    const c = new OffscreenCanvas(w, h);
    const g = c.getContext('2d');
    g.fillStyle = color;
    g.fillRect(0, 0, w, h);
    const bytes = new Uint8Array(await (await c.convertToBlob({ type: 'image/png' })).arrayBuffer());
    return btoa(String.fromCharCode(...bytes));
  }, [w, h, color]);

const v = await call('version');
for (const f of ['execute.document', 'execute.history', 'events.layerPixelsChanged', 'events.layersReordered', 'inspect.pixelRev', 'inspect.textStyle']) {
  check(v.ok && v.value.features.includes(f), `feature ${f}`);
}
const a = (await call('newDocument', { width: 64, height: 64, name: 'remote' })).value;
const bDoc = (await call('newDocument', { width: 64, height: 64, name: 'mine' })).value;
check(a && bDoc && a.id !== bDoc.id, 'two documents');
await frames();
await clear();

// A layer and pixels on the background document: no switch, no undo step.
const made = await ex('layer.new.layer', { name: 'theirs' }, { document: a.id, history: 'none' });
const layer = made.value && made.value.layer;
check(made.ok && typeof layer === 'number', 'layer.new.layer on another document');
const before = (await ex('document.inspect', {}, { document: a.id })).value;
check(before && before.id === a.id && before.layers.some((l) => l.id === layer), 'document.inspect on another document');
const put = await ex('layer.putPixels', { layer, png: await png(8, 8, '#ff0000'), x: 4, y: 4 }, { document: a.id, history: 'none' });
check(put.ok && put.value.rect.join() === '4,4,8,8', 'layer.putPixels');
await frames();
const st = (await call('getState')).value;
check(st.active === bDoc.id, 'the active document stays');
check((await events('documents')).length === 0, 'no documents event (no switch)');
const after = (await ex('document.inspect', {}, { document: a.id })).value;
check(after.history.length === before.history.length, `no undo steps (${before.history.length} → ${after.history.length})`);
const node = after.layers.find((l) => l.id === layer);
check(node && node.pixelRev > 0, `pixelRev moved (${node && node.pixelRev})`);
const pix = await events('layerPixelsChanged');
check(pix.some((e) => e.document === a.id && e.layer === layer && e.pixelRev === node.pixelRev), `layerPixelsChanged ${JSON.stringify(pix)}`);
const mask = await ex('layer.layerMask.set', { layer, png: await png(4, 4, '#000000'), x: 4, y: 4 }, { document: a.id });
check(mask.ok && mask.value.created === true, 'layer.layerMask.set');
const masked = (await ex('document.inspect', {}, { document: a.id })).value.layers.find((l) => l.id === layer);
check(masked.hasMask && masked.pixelRev > node.pixelRev, 'the mask moves pixelRev');

// The person's own document: selection kept and put back, selected layers, reorder and tool events.
await ex('select.rect', { x: 2, y: 3, width: 10, height: 12 });
const saved = await ex('selection.get');
check(saved.ok && saved.value.bounds.join() === '2,3,10,12', 'selection.get');
await ex('select.deselect');
const restored = await ex('selection.set', { id: saved.value.id });
check(restored.ok && restored.value.bounds.join() === '2,3,10,12', 'selection.set');
const l1 = (await ex('layer.new.layer')).value.layer;
const l2 = (await ex('layer.new.layer')).value.layer;
const sel = await ex('layer.selectMany', { layers: [l1, l2], active: l1 });
check(sel.ok && sel.value.active === l1 && sel.value.selected.length === 2, 'layer.selectMany');
await ex('layer.selectMany', { layers: [l2] });
await frames();
await clear();
await ex('layer.arrange.sendBackward');
await frames();
const re = await events('layersReordered');
check(re.some((e) => e.document === bDoc.id && e.group === null && e.layers.indexOf(l2) > e.layers.indexOf(l1)), `layersReordered ${JSON.stringify(re)}`);
await call('setTool', 'eraser');
await frames();
check((await events('toolChanged')).some((e) => e.tool === 'eraser'), 'toolChanged');
const badOpt = await call('execute', 'layer.new.layer', {}, { nope: 1 });
check(!badOpt.ok, 'unknown execute options are refused');
const badDoc = await call('execute', 'layer.new.layer', {}, { document: 999999 });
check(!badDoc.ok, 'an unknown document is refused');
await done();
