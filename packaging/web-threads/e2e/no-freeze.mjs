// No freezes on a phone: no long task over 500 ms (the browser's `longtask` entries) during the
// standard phone tour, a type session, saving and exporting. 360 x 780 touch, deviceMemory 4,
// CPU 4x once the app is interactive, the simple interface. Prints the time to interactive, each
// step's longest task and the app's slow frames with what they spent their time on (`perf_span`),
// and fails when a task went over the limit.
//
//   CHROME=<chromium> CAPISHOT_FREEZE_PAGES=<dir> node no-freeze.mjs <url> <out.json> [limit ms]
//
// The directory holds the pages opened: `page1.jpg` (a manga page, about 1600 x 2400) and, when
// present, `page.psd` (a layered page, 12-24 MP: the class that froze phones for 7-9 s while its
// first composite ran in one frame). Real pages: not committed. With CAPISHOT_E2E_WEBGPU=1 the
// canvas runs on WebGPU (SwiftShader) instead of the CPU canvas headless Chrome falls back to.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
const [url, outFile, limitArg = '500'] = process.argv.slice(2);
const limit = Number(limitArg);
const dir = process.env.CAPISHOT_FREEZE_PAGES || '.';
const webgpu = process.env.CAPISHOT_E2E_WEBGPU === '1';
const args = webgpu
  ? ['--enable-unsafe-webgpu', '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-webgpu-adapter=swiftshader', '--use-angle=swiftshader', '--ignore-gpu-blocklist']
  : ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'];
const b = await chromium.launch({ executablePath: process.env.CHROME, args });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'en-US', acceptDownloads: true });
await ctx.addInitScript(() => {
  Object.defineProperty(navigator, 'deviceMemory', { get: () => 4 });
  // Saves download (save-to-device.mjs covers the file picker).
  Object.defineProperty(self, 'showSaveFilePicker', { value: undefined, configurable: true, writable: true });
  self.__long = [];
  self.__step = 'load';
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) self.__long.push({ step: self.__step, ms: Math.round(e.duration) }); }).observe({ type: 'longtask', buffered: true });
  } catch (e) {}
});
const p = await ctx.newPage();
const slow = [];
p.on('console', (m) => { const t = m.text(); if (t.includes('frame took')) { slow.push(t.replace(/^.*frame took/, 'frame took').slice(0, 300)); } });
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
const cdp = await ctx.newCDPSession(p);
const t0 = Date.now();
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const tti = Date.now() - t0;
console.log(`interactive after ${tti} ms`);
await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
const ex = (command, params = {}) => req('engine.execute', { command, params });
const sleep = (ms) => p.waitForTimeout(ms);
const ui = () => req('ui.inspect');
const idle = async () => { for (let i = 0; i < 240 && ((await ui()).jobs || []).length; i++) await sleep(250); await sleep(800); };
const steps = [];
const step = async (name, fn) => {
  await p.evaluate((n) => { self.__step = n; }, name);
  const s = Date.now();
  try { await fn(); } catch (e) { console.log('step error', name, String(e).slice(0, 200)); }
  await idle();
  steps.push({ name, ms: Date.now() - s });
};
const toScreen = async (x, y) => {
  const u = await ui(); const v = u.views.at(-1); const r = u.canvasRect; const k = v.zoom / 3;
  return [(r[0] + r[2]) / 2 + (x - v.center[0]) * k, (r[1] + r[3]) / 2 + (y - v.center[1]) * k];
};
const tap = async (x, y) => {
  const [sx, sy] = await toScreen(x, y);
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: sx, y: sy }] });
  await sleep(90);
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await sleep(300);
};
const drag = async (pts) => {
  const s = []; for (const [x, y] of pts) s.push(await toScreen(x, y));
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: s[0][0], y: s[0][1] }] });
  for (let i = 1; i < s.length; i++) for (let k = 1; k <= 5; k++) {
    const [x0, y0] = s[i - 1], [x1, y1] = s[i];
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: x0 + (x1 - x0) * k / 5, y: y0 + (y1 - y0) * k / 5 }] });
    await sleep(16);
  }
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
};
// The pages reach the page over a route (a 70 MB PSD is too big to pass as an argument).
await p.route('**/__e2e_pages/*', (route) => route.fulfill({ body: fs.readFileSync(path.join(dir, decodeURIComponent(route.request().url().split('/').pop()))) }));
const open = async (file) => {
  // A Blob, as a picked file arrives (read into the app a slice at a time).
  await p.evaluate(async (n) => { const r = await fetch('/__e2e_pages/' + encodeURIComponent(n)); self.photocraft.open(await r.blob(), n); }, file);
  for (let i = 0; i < 240; i++) { const d = await ex('document.inspect'); if (d && !d.err && d.name === file) break; await sleep(250); }
  await req('ui.set', { fit: true });
};
const download = async (fn) => { const d = p.waitForEvent('download', { timeout: 300000 }); const s = Date.now(); await fn(); const x = await d; await x.cancel().catch(() => {}); return Date.now() - s; };
const commit = async () => { await req('ui.menu.invoke', { id: 'type.commitEdits' }).catch(() => {}); await p.keyboard.press('Control+Enter'); await sleep(200); };
const saves = {};

// The phone tour: open a page, tools from the sheet, paint, zoom, the Capi sheet, preferences.
await step('open page1.jpg', () => open('page1.jpg'));
for (const t of ['brush', 'eraser', 'lasso', 'removeAi', 'type', 'move', 'brush']) await step(`tool ${t}`, () => req('ui.set', { tool: t }));
await step('paint', () => drag([[300, 300], [700, 500], [900, 900], [500, 1200]]));
await step('zoom', async () => { await req('ui.set', { zoom: 1 }); await sleep(500); await req('ui.set', { fit: true }); });
await step('prefs', async () => { await ex('prefs.set', { path: 'interface.theme', value: 'light' }); await ex('prefs.set', { path: 'interface.theme', value: 'dark' }); });
await step('undo/redo', async () => { await req('ui.menu.invoke', { id: 'edit.undo' }); await req('ui.menu.invoke', { id: 'edit.redo' }); });
// A type session.
await step('type session', async () => {
  for (let i = 0; i < 8; i++) {
    await req('ui.set', { tool: 'type' });
    await tap(300 + (i % 4) * 250, 400 + Math.floor(i / 4) * 700);
    await p.keyboard.insertText(`Line ${i}: ¿Qué pasa? ¡Vamos!`);
    await commit();
  }
  const t = ((await ex('layer.list', { flat: true })).layers || []).filter((l) => l.kind === 'Type');
  for (const l of t.slice(0, 4)) await ex('type.edit', { layer: l.id, text: 'Edited text, longer than before' });
  await req('ui.menu.invoke', { id: 'edit.undo' });
});
// Saving and exporting.
await step('save psd', async () => { await p.mouse.click(180, 400); saves.psd = await download(() => p.keyboard.press('Control+Shift+KeyS')); });
await step('export png', async () => { saves.png = await download(() => req('ui.menu.invoke', { id: 'file.export.quickExportAsPng' })); });
// A big layered page (the 7-9 s class), then its save.
if (fs.existsSync(path.join(dir, 'page.psd'))) {
  await step('open page.psd', () => open('page.psd'));
  await step('psd tools', async () => { for (const t of ['brush', 'type', 'move']) await req('ui.set', { tool: t }); });
  await step('save big psd', async () => { await p.mouse.click(180, 400); saves.bigPsd = await download(() => p.keyboard.press('Control+Shift+KeyS')); });
}

const long = await p.evaluate(() => self.__long);
const byStep = {};
for (const l of long) byStep[l.step] = Math.max(byStep[l.step] || 0, l.ms);
const after = long.filter((l) => l.step !== 'load');
const worst = after.reduce((m, l) => Math.max(m, l.ms), 0);
fs.writeFileSync(outFile, JSON.stringify({ tti, steps, saves, long, slow, worst }, null, 1));
for (const s of steps) console.log(`${s.name.padEnd(18)} ${String(s.ms).padStart(6)} ms  longest task ${byStep[s.name] || 0} ms`);
console.log('saves (to download, ms):', JSON.stringify(saves));
for (const s of slow) console.log('  ' + s);
console.log(`longest task after load: ${worst} ms (limit ${limit}) ${worst <= limit ? 'PASS' : 'FAIL'}`);
if (worst > limit) process.exitCode = 1;
await b.close();
