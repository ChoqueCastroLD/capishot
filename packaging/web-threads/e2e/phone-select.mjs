// A phone's Move tool, by finger (360 × 780, touch events through the browser): a tap on a type
// layer selects it (of two) with its box and actions; a corner of the box dragged out scales it
// with its proportions, committed when the finger lifts (one step); a pinch inside the box scales
// it; the action bar's Delete removes it (Undo brings it back); a tap on an empty spot deselects.
// Works on either web build. With SHOTS=<dir> it saves a screenshot of each step there.
//
//   CHROME=<chromium> node phone-select.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
if (shots) fs.mkdirSync(shots, { recursive: true });
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES' });
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = (c, pr = {}) => req('engine.execute', { command: c, params: pr });
const cdp = await ctx.newCDPSession(p);
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };

await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
await ex('file.new', { width: 1200, height: 1800, background: 'white' });
const a = (await ex('type.create', { text: 'PRIMERO', size: 150, x: 150, y: 520 })).layer;
const c = (await ex('type.create', { text: 'SEGUNDO', size: 150, x: 150, y: 1250 })).layer;
await req('ui.set', { tool: 'move', fit: true });
await p.waitForTimeout(1500);

const inspect = async () => { const i = await req('ui.inspect'); return i.result || i; };
const layers = async () => ((await ex('layer.list', { flat: true })).layers || []);
const selected = async () => (await layers()).filter((l) => l.selected).map((l) => l.id);
const boundsOf = async (id) => (await layers()).find((l) => l.id === id)?.bounds; // [x, y, w, h]
// Document point → screen point (CSS px).
const screen = async (x, y) => {
  const r = await inspect();
  const v = r.views.at(-1);
  const cr = r.canvasRect;
  const k = v.zoom / (r.canvasPpp || 1);
  return [(cr[0] + cr[2]) / 2 + (x - v.center[0]) * k, (cr[1] + cr[3]) / 2 + (y - v.center[1]) * k];
};
const rect = async (label) => ((await inspect()).touch.rects || []).findLast((x) => x.label === label)?.rect;
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const tap = async ([x, y]) => { await touches('touchStart', [[x, y]]); await p.waitForTimeout(16); await touches('touchMove', [[x, y]]); await p.waitForTimeout(16); await touches('touchEnd', []); await p.waitForTimeout(600); };
// A finger pressed, held a moment (the press lands where it went down), then moved and lifted.
const drag = async (from, to, steps = 8) => {
  await touches('touchStart', [from]);
  await p.waitForTimeout(150);
  for (let i = 1; i <= steps; i++) { await touches('touchMove', [[from[0] + (to[0] - from[0]) * i / steps, from[1] + (to[1] - from[1]) * i / steps]]); await p.waitForTimeout(20); }
  await p.waitForTimeout(200);
  await touches('touchEnd', []);
  await p.waitForTimeout(800);
};
const centre = async (id) => { const [x, y, w, h] = await boundsOf(id); return screen(x + w / 2, y + h / 2); };

// 1. A tap selects the layer under the finger.
await tap(await centre(c));
check(JSON.stringify(await selected()) === JSON.stringify([c]), `a tap selects the type layer under the finger: ${JSON.stringify(await selected())}`);
await tap(await centre(a));
check(JSON.stringify(await selected()) === JSON.stringify([a]), 'a tap on the other selects it');
check(!!(await rect('layerBar')) && !!(await rect('layerRotate')), 'its actions and its box (rotate handle) show');
await shot('01-toque-selecciona-caja-y-acciones');

// 2. A corner dragged out scales it with its proportions, one step.
const [x0, y0, w0, h0] = await boundsOf(a);
const corner = await screen(x0 + w0, y0 + h0);
await drag([corner[0] + 1, corner[1] + 1], [corner[0] + 40, corner[1] + 14]);
const [, , w1, h1] = await boundsOf(a);
check(w1 > w0 * 1.1 && Math.abs(w1 / h1 - w0 / h0) / (w0 / h0) < 0.08, `a corner drag scales it proportionally: ${w0}×${h0} → ${w1}×${h1}`);
check((await inspect()).transform == null || !(await inspect()).transform, 'committed when the finger lifted');
await shot('02-esquina-escala-proporcional');

// 3. A pinch inside the box scales it.
const [, , w2] = await boundsOf(a);
const m = await centre(a);
await touches('touchStart', [[m[0] - 20, m[1]], [m[0] + 20, m[1]]]);
await p.waitForTimeout(120);
for (let k = 1; k <= 8; k++) { await touches('touchMove', [[m[0] - 20 - 5 * k, m[1]], [m[0] + 20 + 5 * k, m[1]]]); await p.waitForTimeout(30); }
await touches('touchEnd', []);
await p.waitForTimeout(900);
const [, , w3] = await boundsOf(a);
check(w3 > w2 * 1.3, `a pinch inside the box scales it: ${w2} → ${w3}`);
await shot('03-pellizco-escala');

// 4. Delete (the bar scrolls to it), and Undo.
let del = await rect('layer:layer.delete');
const bar = await rect('layerBar');
if (del && del[2] > bar[2]) { await drag([bar[2] - 20, (bar[1] + bar[3]) / 2], [bar[0] + 20, (bar[1] + bar[3]) / 2], 6); del = await rect('layer:layer.delete'); }
await shot('04-barra-con-eliminar');
await tap([(del[0] + del[2]) / 2, (del[1] + del[3]) / 2]);
check(!(await layers()).some((l) => l.id === a), 'Delete removes it');
await ex('edit.undo');
await p.waitForTimeout(600);
check((await layers()).some((l) => l.id === a), 'one Undo brings it back');

// 5. A tap on an empty spot deselects.
await tap(await centre(c));
await tap(await screen(1050, 900));
check((await selected()).length === 0, 'a tap on an empty spot deselects');
check(!(await rect('layerBar')), 'and the actions go');
await shot('05-toque-vacio-deselecciona');

await b.close();
console.log(failed ? `${failed} check(s) failed` : 'the phone selects, scales and deletes by finger');
process.exit(failed ? 1 : 0);
