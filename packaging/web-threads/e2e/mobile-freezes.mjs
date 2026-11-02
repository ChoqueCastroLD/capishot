// The phone freezes the usage statistics reported (main-thread long tasks of seconds): Bubble
// Text Remover clicks, moving a TypeR line (TextShapeR reshaping it), zooming and moving on a
// big page, switching to the Type tool. 360 x 780 touch, deviceMemory 4, CPU 4x once the app is
// interactive. Prints each step's longest task (the browser's `longtask` entries) and fails when
// one went over the limit.
//
//   CHROME=<chromium> CAPISHOT_FREEZE_PAGES=<dir> [CAPISHOT_FREEZE_PAGE=page1.jpg] \
//     node mobile-freezes.mjs <url> <out.json> [limit ms=300]
//
// The page is a real manga page (about 1600 x 2400 or 2000 x 3000, not committed); the bubbles
// clicked are given as fractions of its size in BUBBLES below (page1.jpg's).
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
const [url, outFile, limitArg = '300'] = process.argv.slice(2);
const limit = Number(limitArg);
const dir = process.env.CAPISHOT_FREEZE_PAGES || '.';
const file = process.env.CAPISHOT_FREEZE_PAGE || 'page1.jpg';
// Speech bubbles of page1.jpg (x, y as fractions of the page).
const BUBBLES = [[0.793, 0.1025], [0.343, 0.4667], [0.164, 0.779]];
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'en-US' });
await ctx.addInitScript(() => {
  Object.defineProperty(navigator, 'deviceMemory', { get: () => 4 });
  self.__long = [];
  self.__step = 'load';
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) self.__long.push({ step: self.__step, ms: Math.round(e.duration) }); }).observe({ type: 'longtask', buffered: true });
  } catch (e) {}
});
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
const cdp = await ctx.newCDPSession(p);
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = async (command, params = {}) => { const r = await req('engine.execute', { command, params }); return (r && r.result) || r || {}; };
const sleep = (ms) => p.waitForTimeout(ms);
const ui = async () => { const r = await req('ui.inspect'); return r.result || r; };
const busy = async () => { const r = await req('jobs.list'); const j = (r && r.result) || r || {}; return (j.jobs || []).some((x) => x.state === 'running'); };
const idle = async () => { for (let i = 0; i < 480 && (await busy()); i++) await sleep(250); await sleep(600); };
const steps = [];
const step = async (name, fn) => {
  await p.evaluate((n) => { self.__step = n; }, name);
  const s = Date.now();
  try { await fn(); } catch (e) { console.log('step error', name, String(e).slice(0, 200)); }
  await idle();
  steps.push({ name, ms: Date.now() - s });
  await p.evaluate(() => { self.__step = 'between'; });
};
const toScreen = async (x, y) => {
  const u = await ui(); const v = u.views.at(-1); const r = u.canvasRect; const k = v.zoom / (u.canvasPpp || 3);
  return [(r[0] + r[2]) / 2 + (x - v.center[0]) * k, (r[1] + r[3]) / 2 + (y - v.center[1]) * k];
};
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const tap = async (x, y) => { const s = await toScreen(x, y); await touches('touchStart', [s]); await sleep(60); await touches('touchEnd', []); await sleep(300); };
const drag = async (from, to) => {
  const a = await toScreen(...from); const z = await toScreen(...to);
  await touches('touchStart', [a]);
  for (let k = 1; k <= 8; k++) { await touches('touchMove', [[a[0] + (z[0] - a[0]) * k / 8, a[1] + (z[1] - a[1]) * k / 8]]); await sleep(16); }
  await touches('touchEnd', []);
  await sleep(300);
};

await p.route('**/__e2e_pages/*', (route) => route.fulfill({ body: fs.readFileSync(path.join(dir, decodeURIComponent(route.request().url().split('/').pop()))) }));
await p.evaluate(async (n) => { const r = await fetch('/__e2e_pages/' + encodeURIComponent(n)); self.photocraft.open(await r.blob(), n); }, file);
for (let i = 0; i < 240; i++) { const d = await ex('document.inspect'); if (d && !d.err && d.name === file) break; await sleep(250); }
await req('ui.set', { fit: true });
await idle();
const d = await ex('document.inspect');
const [W, H] = [d.width || d.size?.[0], d.height || d.size?.[1]];
const bubbles = BUBBLES.map(([fx, fy]) => [Math.round(fx * W), Math.round(fy * H)]);
await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });

await step('tool BubbleTextRemover', () => req('ui.set', { tool: 'bubbleTextRemover' }));
for (const [i, [x, y]] of bubbles.entries()) await step(`bubble click ${i + 1}`, () => tap(x, y));
await step('undo', () => req('ui.menu.invoke', { id: 'edit.undo' }));
await step('bubble click again', () => tap(...bubbles[0]));
// A TypeR line pasted in a bubble, the TypeR panel open (TextShapeR shapes it), then moved.
await step('typer paste', async () => {
  await req('ui.set', { panels: { typer: true } }).catch(() => {});
  await ex('typer.script.set', { text: 'Page 1\nNunca pensé que llegaríamos tan lejos, pero aquí estamos todavía juntos.\nSegunda línea.' });
  const [x, y] = bubbles[1];
  await ex('select.rect', { x: x - 90, y: y - 120, width: 180, height: 240 });
  await ex('typer.paste');
  await ex('select.deselect');
  await sleep(1500);
});
await step('tool Move', () => req('ui.set', { tool: 'move' }));
for (let i = 0; i < 3; i++) await step(`move ${i + 1}`, () => drag(bubbles[1], [bubbles[1][0] + 20, bubbles[1][1] + 15]));
await step('zoom in', async () => { await req('ui.menu.invoke', { id: 'view.zoomIn' }); await sleep(400); await req('ui.menu.invoke', { id: 'view.zoomIn' }); await sleep(400); });
await step('fit', () => req('ui.set', { fit: true }));
await step('tool Type', () => req('ui.set', { tool: 'type' }));

const long = await p.evaluate(() => self.__long);
const byStep = {};
for (const l of long) byStep[l.step] = Math.max(byStep[l.step] || 0, l.ms);
const measured = long.filter((l) => l.step !== 'load' && l.step !== 'between');
const worst = measured.reduce((m, l) => Math.max(m, l.ms), 0);
fs.writeFileSync(outFile, JSON.stringify({ steps, long, worst }, null, 1));
for (const s of steps) console.log(`${s.name.padEnd(24)} ${String(s.ms).padStart(6)} ms  longest task ${byStep[s.name] || 0} ms`);
console.log(`longest task: ${worst} ms (limit ${limit}) ${worst <= limit ? 'PASS' : 'FAIL'}`);
if (worst > limit) process.exitCode = 1;
await b.close();
