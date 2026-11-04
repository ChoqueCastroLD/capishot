// Capikoma #23 / #24 / N3 in a real browser (docs/control-protocol.md › Live sync):
//   - `window.photocraft` pushes the embed events as `photocraft-<type>` window events:
//     documentShown, documentReady {firstFrameMs, finalFrameMs}, viewportChanged, layerChanged,
//     layerPixelsChanged, fontsReady.
//   - A brush stroke painted in page A is reproduced in page B from the layerPixelsChanged events
//     alone (layer.getPixels of each rect on A, layer.putPixels into B, no history step) and the
//     two layers end up pixel for pixel equal; layer.dirtySince covers the stroke.
//   - layer.getPixels never switches, redraws or adds history (no documentShown / documentChanged
//     while reading, the screen unchanged).
//   - Type in a served font that is still downloading is never drawn with a fallback: sampled
//     every animation frame until it arrives, its pixels are either none or the final ones;
//     document.fontsReady resolves once it is drawn.
//   - The embedding module: the same events through onEvent, getPixels with a Uint8Array.
// Prints the measured times (event latency after a stroke, documentReady times). Works on either
// web build's dist directory.
//
//   CHROME=<chromium> node embed-push-events.mjs <dist dir> [port = 8798]
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const [distArg, portArg = '8798'] = process.argv.slice(2);
if (!distArg || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> node embed-push-events.mjs <dist dir> [port]');
  process.exit(2);
}
const dist = path.resolve(distArg);
const port = Number(portArg);

// The site: the build, plus a served font (a renamed JetBrains Mono) for the no-fallback check.
const site = fs.mkdtempSync(path.join(os.tmpdir(), 'push-events-'));
fs.cpSync(dist, site, { recursive: true });
const rename = (bytes, from, to) => {
  const enc = (s) => Buffer.from(s, 'utf16le').swap16();
  const [a, b] = [enc(from), enc(to)];
  const out = Buffer.from(bytes);
  for (let i = out.indexOf(a); i >= 0; i = out.indexOf(a, i + a.length)) b.copy(out, i);
  return out;
};
fs.mkdirSync(path.join(site, 'fonts/lib'), { recursive: true });
const jetbrains = fs.readFileSync(path.join(repo, 'assets/fonts/JetBrainsMono-Regular.ttf'));
fs.writeFileSync(path.join(site, 'fonts/lib/Served_Mono_P1.ttf'), rename(jetbrains, 'JetBrains Mono', 'Served Mono P1'));
fs.writeFileSync(
  path.join(site, 'fonts/manifest.txt'),
  '# family | style | file | scripts | licence | licence file | sha256 | source\n' + 'Served Mono P1 | Regular | fonts/lib/Served_Mono_P1.ttf | Latn | - | - | - | test\n',
);
fs.writeFileSync(
  path.join(site, 'host.html'),
  `<!doctype html><meta charset="utf-8"><body style="margin:0"><div id="box" style="width:900px;height:640px"></div><script type="module">
const glue = ${JSON.stringify(fs.readdirSync(dist).find((f) => /^photocraft-web-.*\.js$/.test(f)))};
const wasm = ${JSON.stringify(fs.readdirSync(dist).find((f) => /^photocraft-web-.*_bg\.wasm$/.test(f)))};
const capishot = await import('./' + glue);
await capishot.default({ module_or_path: './' + wasm });
self.events = [];
capishot.onEvent((e) => self.events.push(e));
await capishot.init({ container: document.getElementById('box'), fontsNotice: false, locale: 'en' });
self.capishot = capishot;
</script>`,
);

// The site with the threads build's headers; the served font arrives 1.5 s late, so the
// type waits for it over many frames.
const types = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html', '.json': 'application/json', '.txt': 'text/plain' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || '/').split('?')[0]);
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  let file = path.join(site, path.normalize(url).replace(/^(\.\.[/\\])+/, ''));
  if (fs.existsSync(file) && fs.statSync(file).isDirectory()) file = path.join(file, 'index.html');
  if (!file.startsWith(site) || !fs.existsSync(file)) {
    res.statusCode = 404;
    return res.end();
  }
  res.setHeader('Content-Type', types[path.extname(file)] || 'application/octet-stream');
  setTimeout(() => fs.createReadStream(file).pipe(res), url.startsWith('/fonts/lib/') ? 1500 : 0);
});
await new Promise((r) => server.listen(port, '127.0.0.1', r));
const url = `http://127.0.0.1:${port}/`;
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
const J = JSON.stringify;
const TYPES = ['documents', 'documentShown', 'documentReady', 'documentChanged', 'viewportChanged', 'viewChanged', 'layerChanged', 'layerPixelsChanged', 'layersReordered', 'fontsReady', 'fontsMissing'];

try {
  const ctx = await b.newContext({ viewport: { width: 1280, height: 860 }, locale: 'en-US' });
  const p = await ctx.newPage();
  p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
  await p.goto(url);
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  for (let i = 0; i < 400 && (await req('engine.commands')).err; i++) await p.waitForTimeout(250);
  const ex = (c, pr = {}, opt = {}) => req('engine.execute', { command: c, params: pr, ...opt });
  await p.evaluate((types) => {
    self.ev = [];
    for (const t of types) addEventListener(`photocraft-${t}`, (e) => self.ev.push({ ...e.detail, type: t, at: performance.now() }));
  }, TYPES);
  const evs = (type) => p.evaluate((type) => self.ev.filter((e) => e.type === type), type);
  const clear = () => p.evaluate(() => (self.ev.length = 0));
  const waitEv = (type, pred = '() => true', timeout = 20000) =>
    p.waitForFunction(([type, pred]) => self.ev.some((e) => e.type === type && eval(pred)(e)), [type, pred], { timeout }).then(() => true, () => false);

  // 1. Two pages: documentShown, documentReady, layerChanged.
  const docId = async () => (await ex('document.inspect', {})).id;
  await ex('file.new', { width: 512, height: 512, background: 'white', name: 'A' });
  const a = await docId();
  const la = (await ex('layer.new.layer', {})).layer;
  await ex('file.new', { width: 512, height: 512, background: 'white', name: 'B' });
  const bDoc = await docId();
  const lb = (await ex('layer.new.layer', {})).layer;
  check(await waitEv('documentShown', `(e) => e.id === ${bDoc}`), `documentShown for B: ${J(await evs('documentShown'))}`);
  check(await waitEv('layerChanged', `(e) => e.document === ${bDoc} && e.layer === ${lb} && e.what.includes('added')`), 'layerChanged added');
  check(await waitEv('documentReady', `(e) => e.id === ${bDoc}`), `documentReady for a new document: ${J((await evs('documentReady')).slice(-1))}`);
  await ex('document.activate', { document: 0 }); // A is the first tab
  check((await docId()) === a, 'A active again');
  check(await waitEv('documentShown', `(e) => e.id === ${a} && e.previous === ${bDoc}`), 'documentShown back to A');
  await ex('layer.renameLayer', { layer: la, name: 'ink' });
  check(await waitEv('layerChanged', `(e) => e.layer === ${la} && e.what.includes('name')`), 'layerChanged name');

  // 2. viewportChanged.
  await clear();
  await req('ui.set', { zoom: 2 });
  check(await waitEv('viewportChanged', '(e) => e.zoom === 2'), `viewportChanged: ${J((await evs('viewportChanged')).slice(-1))}`);
  const vp = (await evs('viewportChanged')).slice(-1)[0] || {};
  check(vp.document === a && vp.canvas?.[0] > 0 && Math.abs(vp.rect[2] - vp.canvas[0] / 2) < 1, 'its canvas and visible rect');
  await req('ui.set', { fit: true });

  // 3. A stroke in A, mirrored into B from the events.
  const rev0 = (await ex('layer.pixelRevision', { layer: la })).pixelRev;
  await clear();
  await req('ui.set', { tool: 'brush', brushSize: 24 });
  await ex('tools.setColors', { foreground: '#d02050' });
  const pts = Array.from({ length: 24 }, (_, i) => ({ kind: i === 0 ? 'down' : i === 23 ? 'up' : 'move', x: 60 + i * 16, y: 100 + Math.round(80 * Math.sin(i / 4)), pressure: 0.8 }));
  const t0 = await p.evaluate(() => performance.now());
  await req('ui.pointer', { events: pts });
  check(await waitEv('layerPixelsChanged', `(e) => e.document === ${a} && e.layer === ${la}`), 'layerPixelsChanged for the stroke');
  await p.waitForTimeout(300);
  const changes = (await evs('layerPixelsChanged')).filter((e) => e.document === a && e.layer === la);
  console.log(`  stroke → first layerPixelsChanged ${(changes[0].at - t0).toFixed(1)} ms after the call (${changes.length} events)`);
  const before = await docId();
  const shot0 = await p.screenshot();
  await clear();
  for (const c of changes) {
    const px = await ex('layer.getPixels', { layer: la, rect: c.rect }, { document: a });
    const put = await ex('layer.putPixels', { layer: lb, png: px.data, x: px.rect[0], y: px.rect[1] }, { document: bDoc, history: 'none' });
    if (put.err) check(false, `putPixels: ${put.err}`);
  }
  const full = (doc, layer) => ex('layer.getPixels', { layer, rect: [0, 0, 512, 512], format: 'rgba' }, { document: doc });
  const tg = await p.evaluate(async ([a, la]) => {
    const t = performance.now();
    await self.photocraft.getPixels({ layer: la, rect: [0, 0, 512, 512], format: 'rgba' }, { document: a });
    const t1 = performance.now();
    await self.photocraft.getPixels({ layer: la, rect: [0, 0, 512, 512] }, { document: a });
    return [t1 - t, performance.now() - t1];
  }, [a, la]);
  console.log(`  getPixels 512×512 (call → bytes): rgba ${tg[0].toFixed(1)} ms, png ${tg[1].toFixed(1)} ms`);
  const [pa, pb] = [await full(a, la), await full(bDoc, lb)];
  check(pa.data && pa.data.length > 1000 && pa.data === pb.data, `B's layer equals A's pixel for pixel (${pa.data?.length} base64 chars)`);
  const dirty = await ex('layer.dirtySince', { layer: la, rev: rev0 }, { document: a });
  const u = changes.reduce((r, c) => [Math.min(r[0], c.rect[0]), Math.min(r[1], c.rect[1]), Math.max(r[2], c.rect[0] + c.rect[2]), Math.max(r[3], c.rect[1] + c.rect[3])], [1e9, 1e9, -1e9, -1e9]);
  const d = dirty.rect || [0, 0, 0, 0];
  check(!dirty.full && d[0] <= u[0] && d[1] <= u[1] && d[0] + d[2] >= u[2] && d[1] + d[3] >= u[3], `dirtySince covers the stroke: ${J(dirty)} vs ${J(u)}`);
  // Reading changed nothing: no switch, no edit of A, the same screen.
  await p.waitForTimeout(200);
  const after = await docId();
  const noisy = (await evs('documentShown')).length + (await evs('documentChanged')).filter((e) => e.id === a).length;
  check(noisy === 0 && after === before, `getPixels switched or edited nothing (${noisy} events)`);
  check(Buffer.compare(shot0, await p.screenshot()) === 0, 'the screen is unchanged by reads and a background putPixels');
  const hist = await ex('layer.getPixels', { layer: lb, rect: [0, 0, 1, 1] }, { document: bDoc });
  check(!hist.err, 'getPixels on B (not active)');

  // 4. documentReady after an open.
  await clear();
  const opened = await p.evaluate(async () => {
    const c = new OffscreenCanvas(1600, 2400);
    const g = c.getContext('2d');
    g.fillStyle = '#fff';
    g.fillRect(0, 0, 1600, 2400);
    g.fillStyle = '#123';
    g.fillRect(200, 300, 900, 1200);
    const blob = await c.convertToBlob({ type: 'image/png' });
    await self.photocraft.open(blob, 'page.png');
    return true;
  });
  check(opened && (await waitEv('documentReady', '() => true', 60000)), 'documentReady after open');
  const ready = (await evs('documentReady'))[0] || {};
  console.log(`  open 1600×2400 PNG: firstFrameMs ${ready.firstFrameMs}, finalFrameMs ${ready.finalFrameMs}`);
  check(ready.firstFrameMs >= 0 && ready.finalFrameMs >= ready.firstFrameMs, `documentReady times: ${J(ready)}`);

  // 5. Type in a served font that is still downloading: never a fallback frame.
  for (let i = 0; i < 40 && (await req('fonts.status', {})).manifest !== 'loaded'; i++) await p.waitForTimeout(250);
  await ex('file.new', { width: 400, height: 120, background: 'white' });
  const fdoc = await docId();
  await clear();
  const samples = await p.evaluate(async (fdoc) => {
    const run = (c, params, extra = {}) => self.photocraft.request('engine.execute', { command: c, params, ...extra });
    const t = await run('type.create', { x: 10, y: 70, text: 'No fallback, ever', size: 36, font: 'Served Mono P1' });
    const out = [];
    let done = false;
    const ready = self.photocraft.request('document.fontsReady', { document: fdoc, timeoutMs: 30000 }).then((r) => ((done = true), r));
    while (!done) {
      const px = await run('layer.getPixels', { layer: t.layer, rect: [0, 0, 400, 120], format: 'rgba' });
      out.push(px.data);
      await new Promise(requestAnimationFrame);
    }
    const fin = await run('layer.getPixels', { layer: t.layer, rect: [0, 0, 400, 120], format: 'rgba' });
    const empty = (await run('layer.getPixels', { layer: t.layer, rect: [0, 0, 0, 0] })).data;
    return { result: await ready, samples: out, final: fin.data, empty };
  }, fdoc);
  const blank = samples.samples.filter((s) => !/[^A]/.test(s.replace(/=+$/, ''))).length;
  const finals = samples.samples.filter((s) => s === samples.final).length;
  check(J(samples.result.ready) === J(['Served Mono P1']), `document.fontsReady: ${J(samples.result)}`);
  check(blank + finals === samples.samples.length, `${samples.samples.length} frames sampled: ${blank} without the type, ${finals} with its own font, ${samples.samples.length - blank - finals} with a fallback`);
  check(/[^A]/.test(samples.final), 'drawn once the font is in');
  await ctx.close();

  // 6. The module (the single-threaded build is the one published as the embedding module): the
  // same events through onEvent, getPixels as bytes.
  if (!fs.readdirSync(dist).some((f) => /^photocraft-web-.*\.js$/.test(f))) {
    console.log('  (no embedding module in this build: its onEvent checks run on the single-threaded build)');
  } else {
    const hctx = await b.newContext({ viewport: { width: 1280, height: 860 } });
    const h = await hctx.newPage();
    h.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
    await h.goto(url + 'host.html');
    await h.waitForFunction(() => self.capishot, null, { timeout: 300000 });
    const m = await h.evaluate(async () => {
      const cs = self.capishot;
      const d1 = await cs.newDocument({ width: 256, height: 256 });
      const l = await cs.execute('layer.new.layer', {});
      await cs.execute('select.rect', { x: 10, y: 10, width: 20, height: 20 });
      await cs.execute('edit.fill', { contents: 'black' });
      const d2 = await cs.newDocument({ width: 64, height: 64 });
      const px = await cs.getPixels({ layer: l.layer, format: 'rgba' }, { document: d1.id });
      await new Promise((r) => setTimeout(r, 400));
      const types = [...new Set(self.events.map((e) => e.type))];
      return { isBytes: px.data instanceof Uint8Array, len: px.data.length, rect: px.rect, types, d2: d2.id, features: cs.version().features };
    });
    check(m.isBytes && m.len === 20 * 20 * 4 && J(m.rect) === J([10, 10, 20, 20]), `module getPixels: ${J({ ...m, types: undefined, features: undefined })}`);
    check(['documentShown', 'documentReady', 'layerChanged', 'layerPixelsChanged', 'viewportChanged'].every((t) => m.types.includes(t)), `module events: ${J(m.types)}`);
    check(['layer.getPixels', 'layer.dirtySince', 'events.documentReady', 'document.fontsReady', 'window.events', 'fonts.noFallbackFlash'].every((f) => m.features.includes(f)), 'features listed');
    await hctx.close();
  }
} finally {
  await b.close();
  server.close();
  fs.rmSync(site, { recursive: true, force: true });
}
console.log(failed ? `${failed} check(s) failed` : 'events push, a stroke mirrors pixel for pixel, reads touch nothing, type never flashes a fallback');
process.exit(failed ? 1 : 0);
