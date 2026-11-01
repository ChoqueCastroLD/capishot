// The Bubble Text Remover on a phone (360 × 780, touch events, Spanish): the tool is picked from
// the Capi sheet; a tap on a letter in a speech bubble removes its text in one undo step, the
// outline untouched; a tap in a narration box cleans it too; a tap in a textured bubble is
// refused with a notice and changes nothing. Works on either web build. With SHOTS=<dir> it saves
// a screenshot of each step there.
//
//   CHROME=<chromium> node bubble-clean.mjs <url>
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
const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return (r && r.result) || r || {}; };
const cdp = await ctx.newCDPSession(p);
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };

await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
await ex('file.new', { width: 1200, height: 1800, background: 'white' });
// A speech bubble with two lines, a narration box, and a bubble with a grainy (textured) inside.
await ex('shape.create', { kind: 'ellipse', rect: [150, 150, 900, 520], fill: '#ffffff', stroke: { width: 6, color: '#000000' } });
await ex('type.create', { text: '¡HOLA! ¿CÓMO', size: 72, x: 330, y: 400 });
await ex('type.create', { text: 'ESTÁS HOY?', size: 72, x: 380, y: 490 });
await ex('shape.create', { kind: 'rect', rect: [150, 860, 900, 260], fill: '#fff6dc', stroke: { width: 5, color: '#000000' } });
await ex('type.create', { text: 'Mientras tanto...', size: 64, x: 300, y: 1010 });
await ex('shape.create', { kind: 'ellipse', rect: [250, 1260, 700, 420], fill: '#ffffff', stroke: { width: 6, color: '#000000' } });
await ex('layer.flattenImage');
await ex('select.rect', { x: 240, y: 1250, width: 720, height: 440 });
await ex('filter.noise.addNoise', { amount: 30, distribution: 'gaussian', monochromatic: true, seed: 3 });
await ex('select.deselect');
await ex('type.create', { text: 'RUIDO', size: 72, x: 470, y: 1500 });
await ex('layer.flattenImage');
await req('ui.set', { fit: true });
await p.waitForTimeout(1500);

const inspect = async () => { const i = await req('ui.inspect'); return i.result || i; };
const screen = async (x, y) => {
  const r = await inspect();
  const v = r.views.at(-1);
  const cr = r.canvasRect;
  const k = v.zoom / (r.canvasPpp || 1);
  return [(cr[0] + cr[2]) / 2 + (x - v.center[0]) * k, (cr[1] + cr[3]) / 2 + (y - v.center[1]) * k];
};
const button = async (label) => {
  const r = ((await inspect()).simpleUi.buttons || []).findLast((x) => x.label === label)?.rect;
  return r && [(r[0] + r[2]) / 2, (r[1] + r[3]) / 2];
};
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const tap = async ([x, y]) => { await touches('touchStart', [[x, y]]); await p.waitForTimeout(16); await touches('touchEnd', []); await p.waitForTimeout(900); };
const history = async () => (await ex('document.inspect')).history || [];
const doc = async () => ex('document.inspect');
await shot('01-pagina-con-globos');

// 1. The tool from the Capi sheet.
await tap(await button('capi'));
await p.waitForTimeout(600);
let tool = await button('tool:BubbleTextRemover');
check(!!tool, 'the Capi sheet lists Bubble Text Remover');
await shot('02-hoja-capi-con-la-herramienta');
// Scroll the sheet when the button is below the screen.
for (let i = 0; i < 6 && tool && tool[1] > 740; i++) {
  await touches('touchStart', [[180, 600]]);
  for (let k = 1; k <= 6; k++) { await touches('touchMove', [[180, 600 - k * 50]]); await p.waitForTimeout(20); }
  await touches('touchEnd', []);
  await p.waitForTimeout(500);
  tool = await button('tool:BubbleTextRemover');
}
await tap(tool);
check(JSON.stringify((await inspect()).tool).includes('BubbleTextRemover'), `the tool is picked: ${JSON.stringify((await inspect()).tool)}`);
await shot('03-herramienta-elegida');

// 2. A tap on a letter of the bubble.
const before = (await history()).length;
await tap(await screen(360, 375));
const h1 = await history();
check(h1.length === before + 1, `one undo step: ${JSON.stringify(h1.slice(-1))}`);
await shot('04-globo-limpio');

// 3. The narration box.
await tap(await screen(600, 900));
check((await history()).length === before + 2, 'the narration box cleaned in a step');
await shot('05-cartela-limpia');

// 4. The textured bubble: refused with a notice, nothing changed.
await tap(await screen(600, 1300));
await p.waitForTimeout(300);
const notices = (await inspect()).notices || [];
check((await history()).length === before + 2, 'the textured bubble changed nothing');
check(notices.some((n) => /fondo liso/.test(JSON.stringify(n)) || /plain background/.test(JSON.stringify(n))), `a notice: ${JSON.stringify(notices).slice(0, 200)}`);
await shot('06-globo-con-textura-rechazado');

// 5. Undo brings the narration's text back.
await ex('edit.undo');
await p.waitForTimeout(600);
check((await doc()).canRedo, 'Undo takes the step back');

await b.close();
console.log(failed ? `${failed} check(s) failed` : 'the bubble text remover works by finger');
process.exit(failed ? 1 : 0);
