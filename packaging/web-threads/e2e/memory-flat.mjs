// A type-heavy phone session must keep the page's memory flat (the phone crash report: a tab
// killed out of memory after an hour of typesetting). 360 x 780 touch, deviceMemory 4, CPU 4x,
// the simple interface; Type and Bubble Type clicks, pasting, editing, moving, undo; a second page.
// It prints the WebAssembly memory and `perf.memory` after every operation and fails unless the
// last third of the first page's operations grows the memory by under 10%.
//
//   CHROME=<chromium> CAPISHOT_MEMORY_PAGES=<dir with page1.jpg and page2.jpg> \
//     node memory-flat.mjs <url of a threads build served by serve.py> <out.json> [ops1] [ops2]
//
// The pages are real manga pages (about 1600 x 2400): not committed. The page must expose the
// module's exports as `self.__wasmExports` (the tests' copy of index.html does).
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
const [url, outFile, ops1 = '60', ops2 = '20', skip = ''] = process.argv.slice(2);
const skipKinds = skip.split(',').filter(Boolean).map(Number);
const dir = process.env.CAPISHOT_MEMORY_PAGES || '.';
const pages = ['page1.jpg', 'page2.jpg'].map((f) => path.join(dir, f));
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--enable-precise-memory-info', '--js-flags=--expose-gc'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'en-US' });
await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: new URL(url).origin });
await ctx.addInitScript(() => { Object.defineProperty(navigator, 'deviceMemory', { get: () => 4 }); });
const p = await ctx.newPage();
let crashed = false;
p.on('crash', () => { crashed = true; console.log('PAGE CRASHED'); });
p.on('pageerror', e => console.log('PAGEERROR', e.message.slice(0, 300)));
const cdp = await ctx.newCDPSession(p);
await cdp.send('Performance.enable');
await p.goto(url); await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then(r => r, e => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 200; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(1000); }
await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
const ex = (command, params = {}) => req('engine.execute', { command, params });
const sleep = ms => p.waitForTimeout(ms);
const ui = () => req('ui.inspect');
const toScreen = async (x, y) => {
  const u = await ui(); const v = u.views.at(-1); const r = u.canvasRect; const k = v.zoom / 3;
  return [(r[0] + r[2]) / 2 + (x - v.center[0]) * k, (r[1] + r[3]) / 2 + (y - v.center[1]) * k];
};
const tap = async (x, y) => {
  const [sx, sy] = await toScreen(x, y);
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: sx, y: sy }] });
  await sleep(90);
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await sleep(400);
};
const commit = async () => { await req('ui.menu.invoke', { id: 'type.commitEdits' }).catch(() => {}); await p.keyboard.press('Control+Enter'); await sleep(300); };
const typeLayers = async () => ((await ex('layer.list', { flat: true })).layers || []).filter(l => l.kind === 'Type');
const metrics = async (op) => {
  const m = Object.fromEntries((await cdp.send('Performance.getMetrics')).metrics.map(x => [x.name, x.value]));
  const wasm = await p.evaluate(() => self.__wasmExports ? self.__wasmExports.memory.buffer.byteLength : 0);
  const mem = await req('perf.memory').catch(() => null);
  const u = await ui();
  return { op, js: m.JSHeapUsedSize, jsTotal: m.JSHeapTotalSize, wasm, gpu: u.perf?.timings?.gpu_budget_state ?? null, app: mem && !mem.err ? mem : null, layers: (await ex('layer.list', { flat: true })).layers?.length };
};
const text = (i) => `Line ${i}: Lorem ipsum dolor sit amet, consectetur adipiscing elit. ¿Qué pasa? ¡Vamos!`;
const bubbles = [[[1304, 246], [564, 1120], [270, 1870]], [[400, 400], [1200, 600], [800, 1600]]];
const rows = [];
const open = async (k) => {
  const bytes = [...fs.readFileSync(pages[k])];
  await p.evaluate(([b, n]) => self.photocraft.open(new Uint8Array(b), n), [bytes, `page${k + 1}.jpg`]);
  await sleep(1500);
  await req('ui.set', { fit: true }); await sleep(500);
};
await open(0);
rows.push(await metrics('open1'));
let op = 0;
const run = async (k, n) => {
  for (let i = 0; i < n && !crashed; i++, op++) {
    let kind = op % 6;
    if (skipKinds.includes(kind)) kind = 0;
    const rnd = (a, b) => a + Math.floor(Math.random() * (b - a));
    try {
      if (kind === 0) {
        await req('ui.set', { tool: 'type' }); await tap(rnd(200, 1400), rnd(300, 2100));
        await p.keyboard.insertText(text(op)); await commit();
      } else if (kind === 1) {
        await req('ui.set', { tool: 'bubbleType' }); const [x, y] = bubbles[k][op % 3]; await tap(x, y);
        await p.keyboard.insertText(text(op)); await commit();
      } else if (kind === 2) {
        await p.evaluate((t) => navigator.clipboard.writeText(t), text(op).repeat(3));
        await req('ui.set', { tool: 'type' }); await tap(rnd(200, 1400), rnd(300, 2100));
        await p.keyboard.press('Control+V'); await sleep(500); await commit();
      } else if (kind === 3) {
        const t = await typeLayers(); if (t.length) await ex('type.edit', { layer: t[rnd(0, t.length)].id, text: text(op) + ' (edited)' });
      } else if (kind === 4) {
        const t = await typeLayers(); if (t.length) await ex('type.edit', { layer: t[rnd(0, t.length)].id, move: [rnd(-40, 40), rnd(-40, 40)] });
      } else {
        await ex('edit.undo');
      }
    } catch (e) { console.log('op error', op, String(e).slice(0, 200)); }
    if (crashed) break;
    const m = await metrics(`p${k + 1}-${op}-${kind}`);
    rows.push(m);
    console.log(JSON.stringify({ op: m.op, jsMB: (m.js / 1e6).toFixed(1), wasmMB: (m.wasm / 1e6).toFixed(1), gpu: m.gpu, app: m.app && { hist: m.app.historyResidentMB, page: m.app.pageCacheMB } }));
  }
};
await run(0, +ops1);
if (!crashed) { await open(1); rows.push(await metrics('open2')); await run(1, +ops2); }
fs.writeFileSync(outFile, JSON.stringify({ crashed, rows }, null, 1));
const w = rows.map(r => r.wasm).filter(Boolean);
// Flat: over the last third of page 1's operations the WebAssembly memory grows by under 10%.
const p1 = rows.filter(r => r.op.startsWith('p1-'));
const third = p1.slice(Math.floor(p1.length * 2 / 3));
const growth = third.length ? (third.at(-1).wasm - third[0].wasm) / third[0].wasm : 0;
console.log(`last third of page 1: ${(third[0]?.wasm / 1e6).toFixed(0)} -> ${(third.at(-1)?.wasm / 1e6).toFixed(0)} MB (${(growth * 100).toFixed(1)}%) ${growth < 0.1 ? 'FLAT' : 'GROWING'}`);
if (crashed || growth >= 0.1) process.exitCode = 1;
console.log(`crashed ${crashed}; wasm first ${(w[0] / 1e6).toFixed(0)} MB, max ${(Math.max(...w) / 1e6).toFixed(0)} MB, last ${(w.at(-1) / 1e6).toFixed(0)} MB; js max ${(Math.max(...rows.map(r => r.js)) / 1e6).toFixed(0)} MB`);
await b.close();
