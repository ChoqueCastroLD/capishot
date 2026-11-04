// A phone with a big page must survive Remove with AI and undo/redo (the crash report: an
// Android phone, 4 GB, a 12-24 MP page, Remove with AI over an SFX, then undo, redo, undo: the
// tab was killed; and multi-second freezes after opening and after each removal). 360 x 780
// touch, deviceMemory 4 or 2 (DEVICE_MEMORY), CPU 4x once the app is interactive. Opens the page,
// removes three SFX-sized strokes with AI (Manga LaMa, the weights served locally), then undo /
// redo three times. After every step prints the page's WebAssembly memory, the GPU canvas's
// texture memory and the resident memory of the renderer and GPU processes, and the step's
// longest main-thread task. Fails when the page crashed, a removal failed, a task went over
// the limit, or the renderer + GPU process memory went over the cap.
//
//   CHROME=<chromium> CAPISHOT_BIG_PAGES=<dir> [CAPISHOT_BIG_PAGE=page4k6k.jpg] [DEVICE_MEMORY=4] \
//   [WEBGPU=1] node mobile-remove-ai.mjs <url> <out.json> [task limit ms=1000] [cap MB=1400]
//
// The served site needs `models/` with manga-lama-f16.safetensors next to its index.html (a
// symlink; weights are never part of a build). The page is a real manga page resized to
// 3000 x 6000 or 4000 x 6000 (not committed). Requests off the test server are refused (no
// telemetry, no model CDN).
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
import { execSync } from 'node:child_process';
const [url, outFile, limitArg = '1000', capArg = '1400'] = process.argv.slice(2);
const limit = Number(limitArg);
const cap = Number(capArg);
const dir = process.env.CAPISHOT_BIG_PAGES || '.';
const file = process.env.CAPISHOT_BIG_PAGE || 'page4k6k.jpg';
const gb = Number(process.env.DEVICE_MEMORY || '4');
const origin = new URL(url).origin;
const b = await chromium.launch({
  executablePath: process.env.CHROME, dumpio: !!process.env.DUMPIO,
  // WEBGPU=1: WebGPU on SwiftShader. On a machine without a GPU Chrome drops that device a few
  // frames in (a bare WebGPU page too): the run then checks the app reloads with its documents
  // instead of closing. Otherwise WebGL, which still draws through the GPU canvas.
  args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--enable-precise-memory-info', ...(process.env.WEBGPU === '1' ? ['--enable-unsafe-webgpu', '--use-webgpu-adapter=swiftshader'] : [])],
});
const ctx = await b.newContext({
  viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'en-US',
  userAgent: 'Mozilla/5.0 (Linux; Android 14; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36',
});
await ctx.addInitScript((gb) => {
  Object.defineProperty(navigator, 'deviceMemory', { get: () => gb });
  self.__long = [];
  self.__step = 'load';
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) self.__long.push({ step: self.__step, ms: Math.round(e.duration) }); }).observe({ type: 'longtask', buffered: true });
  } catch (e) {}
}, gb);
// The page from disk; the app's page names `models/` on the test server for the weights; nothing
// off the test server. (Weights through `route.fulfill` would overflow DevTools' 100 MB pipe,
// which closes the page.)
await ctx.route('**/*', async (route) => {
  const r = route.request();
  const u = r.url();
  if (u.startsWith(origin + '/__e2e_pages/')) return route.fulfill({ body: fs.readFileSync(path.join(dir, decodeURIComponent(u.split('/').pop()))) });
  if (!u.startsWith(origin)) return route.abort();
  if (r.resourceType() === 'document') {
    const res = await route.fetch();
    const base = new URL('models/', origin + '/').href;
    return route.fulfill({ response: res, body: (await res.text()).replace(/(<meta name="capishot-ai-models" content=")[^"]*"/, `$1${base}"`) });
  }
  return route.continue();
});
const webgpu = process.env.WEBGPU === '1';
const p = await ctx.newPage();
p.on('close', () => console.log('PAGE CLOSED'));
p.on('framenavigated', (f) => { if (f === p.mainFrame()) console.log('NAVIGATED', f.url()); });
p.on('console', (m) => { const t = m.text(); if (process.env.ALLCONSOLE || /error|panic|lost|out of memory/i.test(t)) console.log('CONSOLE', m.type(), t.slice(0, 300)); });
let crashed = false;
p.on('crash', () => { crashed = true; console.log('PAGE CRASHED'); });
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
const cdp = await ctx.newCDPSession(p);
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = async (command, params = {}, wait) => { const r = await req('engine.execute', wait === undefined ? { command, params } : { command, params, wait }); return (r && r.result) || r || {}; };
const sleep = (ms) => p.waitForTimeout(ms);
const busy = async () => { const r = await req('jobs.list'); const j = (r && r.result) || r || {}; return (j.jobs || []).some((x) => x.state === 'running'); };
const idle = async () => { for (let i = 0; i < 1200 && !crashed && (await busy()); i++) await sleep(250); await sleep(800); };
// Resident memory (MB) of this browser's renderer and GPU processes.
const procMB = () => {
  try {
    const rows = execSync('ps -eo pid=,ppid=,rss=,args=', { encoding: 'utf8' }).split('\n').map((l) => l.trim().split(/\s+/)).filter((r) => r.length > 3);
    const kids = new Set([String(process.pid)]);
    for (let grew = true; grew;) { grew = false; for (const r of rows) if (kids.has(r[1]) && !kids.has(r[0])) { kids.add(r[0]); grew = true; } }
    const sum = (t) => Math.round(rows.filter((r) => kids.has(r[0]) && r.slice(3).join(' ').includes(t)).reduce((a, r) => a + Number(r[2]), 0) / 1024);
    return { renderer: sum('--type=renderer'), gpu: sum('--type=gpu-process') };
  } catch (e) { return { renderer: 0, gpu: 0 }; }
};
const rows = [];
// The processes' memory twice a second: the peak between the steps' readings counts too.
const samples = [];
const sampler = setInterval(() => { const m = procMB(); samples.push({ t: Date.now(), ...m }); }, 500);
const measure = async (op, extra = {}) => {
  if (crashed) return;
  const m = await req('perf.memory');
  const mm = m && !m.err ? (m.result || m) : {};
  const pm = procMB();
  const long = await p.evaluate((s) => self.__long.filter((l) => l.step === s).reduce((a, l) => Math.max(a, l.ms), 0), op).catch(() => 0);
  const row = { op, ...extra, longest: long, wasm: mm.wasmMB || 0, gpuTex: mm.gpuTexturesMB, hist: mm.historyResidentMB, renderer: pm.renderer, gpu: pm.gpu, total: pm.renderer + pm.gpu };
  rows.push(row);
  console.log(JSON.stringify(row));
};
const step = async (name, fn) => {
  if (crashed) return;
  await p.evaluate((n) => { self.__step = n; }, name).catch(() => {});
  const s = Date.now();
  let r;
  try { r = await fn(); } catch (e) { console.log('step error', name, String(e).slice(0, 200)); }
  await idle();
  await sleep(1500);
  await measure(name, { ms: Date.now() - s });
  await p.evaluate(() => { self.__step = 'between'; }).catch(() => {});
  return r;
};

await step('open', async () => {
  await p.evaluate(async (n) => { const r = await fetch('/__e2e_pages/' + encodeURIComponent(n)); self.photocraft.open(await r.blob(), n); }, file);
  for (let i = 0; i < 480; i++) { const d = await ex('document.inspect'); if (d && !d.err && d.name === file) break; await sleep(250); }
  await req('ui.set', { fit: true });
});
const d = await ex('document.inspect');
const [W, H] = [d.width || d.size?.[0], d.height || d.size?.[1]];
console.log(`page ${W} x ${H}, deviceMemory ${gb}, WebGPU ${webgpu}`);
await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
// SFX-sized strokes (a zigzag over a 500 x 400 area) at three places of the page.
const spots = [[0.3, 0.2], [0.65, 0.5], [0.4, 0.8]];
let failed = 0;
for (const [i, [fx, fy]] of spots.entries()) {
  const [cx, cy] = [Math.round(fx * W), Math.round(fy * H)];
  const pts = [];
  for (let k = 0; k <= 10; k++) pts.push([cx - 250 + k * 50, cy + (k % 2 ? 200 : -200), 1]);
  const r = await step(`removeAi ${i + 1}`, async () => {
    const r = await req('engine.execute', { command: 'edit.removeAi', params: { strokes: [pts], size: 60, allowDownload: true }, wait: true });
    for (let k = 0; k < 1200 && !crashed && (await busy()); k++) await sleep(250);
    return r;
  });
  const err = r && (r.err || r.error);
  if (err) { failed++; console.log('removeAi failed', i, String(err).slice(0, 300)); }
}
for (let i = 0; i < 3; i++) {
  await step(`undo ${i + 1}`, () => req('ui.menu.invoke', { id: 'edit.undo' }));
  await step(`redo ${i + 1}`, () => req('ui.menu.invoke', { id: 'edit.redo' }));
}
await step('undo 4', () => req('ui.menu.invoke', { id: 'edit.undo' }));
await step('zoom in', async () => { await req('ui.menu.invoke', { id: 'view.zoomIn' }); await sleep(400); await req('ui.menu.invoke', { id: 'view.zoomIn' }); });
await step('fit', () => req('ui.set', { fit: true }));

const long = await p.evaluate(() => self.__long).catch(() => []);
const worst = long.filter((l) => l.step !== 'load' && l.step !== 'between' && l.step !== 'open').reduce((m, l) => Math.max(m, l.ms), 0);
const openWorst = long.filter((l) => l.step === 'open').reduce((m, l) => Math.max(m, l.ms), 0);
clearInterval(sampler);
const peak = Math.max(0, ...rows.map((r) => r.total), ...samples.map((m) => m.renderer + m.gpu));
const summary = { page: file, size: [W, H], deviceMemory: gb, webgpu, crashed, failed, worstTask: worst, openWorstTask: openWorst, peakMB: peak, peakWasmMB: Math.max(0, ...rows.map((r) => r.wasm)) };
fs.writeFileSync(outFile, JSON.stringify({ summary, rows, long }, null, 1));
console.log(JSON.stringify(summary));
if (crashed || failed || worst > limit || openWorst > limit || peak > cap) process.exitCode = 1;
await b.close();
