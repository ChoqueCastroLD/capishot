// Clean Selected Area with AI must keep a phone page's memory flat (the crash report: Android
// pages killed out of memory, 730-950 MB, right after `edit.inpaintAi`). 360 x 780 touch,
// deviceMemory 4, CPU 4x; a 2000 x 3000 page; 20 rectangle selections over speech bubbles, each
// cleaned with AI (the model the phone picks: Auto). Prints the WebAssembly memory after every
// clean (and the renderer's resident memory, which holds the AI worker's too, during and a few
// seconds after each clean) and fails when the page crashed, a clean failed, the page's memory
// went over the cap, or the renderer's memory between cleans grew 10% or more over the last half.
//
//   CHROME=<chromium> CAPISHOT_MEMORY_PAGES=<dir with page2k3k.jpg> \
//     node ai-clean-memory.mjs <url> <out.json> [cleans=20] [cap MB=600]
//
// The page names the AI worker and a models URL holding the weights (`capishot-ai-models`
// meta: the test serves them next to the build). The page is a real manga page resized to about
// 2000 x 3000: not committed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
import { execSync } from 'node:child_process';
const [url, outFile, nArg = '20', capArg = '600'] = process.argv.slice(2);
const n = Number(nArg);
const cap = Number(capArg);
const dir = process.env.CAPISHOT_MEMORY_PAGES || '.';
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--enable-precise-memory-info'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'en-US' });
await ctx.addInitScript(() => { Object.defineProperty(navigator, 'deviceMemory', { get: () => 4 }); });
const p = await ctx.newPage();
let crashed = false;
p.on('crash', () => { crashed = true; console.log('PAGE CRASHED'); });
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
const cdp = await ctx.newCDPSession(p);
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = (command, params = {}) => req('engine.execute', { command, params });
const sleep = (ms) => p.waitForTimeout(ms);
const bytes = [...fs.readFileSync(path.join(dir, 'page2k3k.jpg'))];
await p.evaluate(([b, n]) => self.photocraft.open(new Uint8Array(b), n), [bytes, 'page.jpg']);
await sleep(2000);
await req('ui.set', { fit: true });
await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
// Resident memory of the renderer processes (the page and its workers: the AI worker's own
// WebAssembly memory is not in the page's), MB.
const rendererMB = () => {
  try {
    const rows = execSync('ps -eo pid=,ppid=,rss=,args=', { encoding: 'utf8' }).split('\n').map((l) => l.trim().split(/\s+/)).filter((r) => r.length > 3);
    const kids = new Set([String(process.pid)]);
    for (let grew = true; grew;) { grew = false; for (const r of rows) if (kids.has(r[1]) && !kids.has(r[0])) { kids.add(r[0]); grew = true; } }
    return Math.round(rows.filter((r) => kids.has(r[0]) && r.slice(3).join(' ').includes('--type=renderer')).reduce((a, r) => a + Number(r[2]), 0) / 1024);
  } catch (e) { return 0; }
};
const mem = async () => { const m = await req('perf.memory'); return m && !m.err ? m : {}; };
const wasmMB = async () => (await mem()).wasmMB || 0;
const idle = async () => {
  for (let i = 0; i < 1200; i++) {
    const r = await req('jobs.list'); const j = (r && r.result) || r || {};
    if (!(j.jobs || []).some((x) => x.state === 'running')) return;
    await sleep(250);
  }
};
// Text areas of the page (2055 x 3000): rectangles over bubbles, shifted a little each round.
const spots = [[1500, 200, 300, 260], [600, 1290, 260, 300], [250, 2230, 240, 260], [1300, 1700, 280, 220]];
const rows = [{ op: 'open', wasm: await wasmMB(), rss: rendererMB() }];
let failed = 0;
for (let i = 0; i < n && !crashed; i++) {
  const [x, y, w, h] = spots[i % spots.length];
  const d = (i >> 2) * 7;
  await ex('select.rect', { x: x + d, y: y + d, width: w, height: h });
  const t0 = Date.now();
  const r = await req('engine.execute', { command: 'edit.inpaintAi', params: { allowDownload: true }, wait: true });
  await idle();
  const ms = Date.now() - t0;
  const busy = rendererMB();
  // A phone's idle AI worker goes a few seconds after its run (its memory back to the system).
  await sleep(4000);
  const err = r && r.err ? r.err : (r && r.error) || null;
  const res = (r && r.result) || {};
  if (err) { failed++; console.log('clean failed', i, String(err).slice(0, 200)); }
  const m = await mem();
  const row = { op: `clean${i}`, ms, wasm: m.wasmMB || 0, busy, rss: rendererMB(), hist: m.historyResidentMB, layers: ((await ex('layer.list', { flat: true })).layers || []).length };
  rows.push(row);
  console.log(JSON.stringify(row));
}
await ex('select.deselect').catch(() => {});
fs.writeFileSync(outFile, JSON.stringify({ crashed, failed, rows }, null, 1));
const cleans = rows.filter((r) => r.op.startsWith('clean'));
// Flat: between cleans the renderer's memory (the page plus its idle workers) doesn't climb
// over the last half; the page's own memory stays under the cap (its layers and history grow it
// a little with every clean).
const half = cleans.slice(Math.floor(cleans.length / 2));
const growth = half.length > 1 ? (half.at(-1).rss - half[0].rss) / half[0].rss : 0;
const max = Math.max(...rows.map((r) => r.wasm));
console.log(`renderer RSS open ${rows[0].rss} MB, during a clean max ${Math.max(...cleans.map((r) => r.busy))} MB, between cleans max ${Math.max(...cleans.map((r) => r.rss))} MB, last ${rows.at(-1).rss} MB; last half ${(growth * 100).toFixed(1)}%`);
console.log(`wasm open ${rows[0].wasm} MB, max ${max} MB, last ${rows.at(-1).wasm} MB; failed ${failed}; crashed ${crashed}`);
if (crashed || failed || max >= cap || growth >= 0.1) process.exitCode = 1;
await b.close();
