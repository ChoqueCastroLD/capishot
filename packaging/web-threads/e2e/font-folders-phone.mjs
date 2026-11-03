// A phone's font menu (360 × 780, touch events through the browser): a user font (dropped on the
// window) is filed into a folder with press and hold › Mover a… (Custom, where user fonts are
// listed), dragged onto a folder by finger, and renamed (an alias the menu shows and searches; the
// layer keeps the family). Works on either web build. With SHOTS=<dir> it saves screenshots.
//
//   CHROME=<chromium> FONT=<a .ttf file> node font-folders-phone.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
if (shots) fs.mkdirSync(shots, { recursive: true });
const fontBytes = [...fs.readFileSync(process.env.FONT)];
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES' });
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return r.result ?? r; };
const cdp = await ctx.newCDPSession(p);
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };
const inspect = async () => { const i = await req('ui.inspect'); return i.result || i; };
const rect = async (label) => ((await inspect()).touch?.rects || []).findLast((x) => x.label === label)?.rect;
const labels = async (prefix) => ((await inspect()).touch?.rects || []).map((x) => x.label).filter((l) => l.startsWith(prefix));
const centre = (r) => [(r[0] + r[2]) / 2, (r[1] + r[3]) / 2];
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const tap = async ([x, y]) => { await touches('touchStart', [[x, y]]); await p.waitForTimeout(40); await touches('touchEnd', []); await p.waitForTimeout(600); };
const hold = async ([x, y], ms = 900) => { await touches('touchStart', [[x, y]]); for (let t = 0; t < ms; t += 100) { await p.waitForTimeout(100); await touches('touchMove', [[x, y]]); } await touches('touchEnd', []); await p.waitForTimeout(600); };
// `target()`: where to drop, read once the hold took (the "Mover a…" panel moves the list down).
const holdDrag = async (from, target, ms = 1200, steps = 10) => {
  await touches('touchStart', [from]);
  for (let t = 0; t < ms; t += 100) { await p.waitForTimeout(100); await touches('touchMove', [from]); }
  const to = await target();
  for (let i = 1; i <= steps; i++) { await touches('touchMove', [[from[0] + (to[0] - from[0]) * i / steps, from[1] + (to[1] - from[1]) * i / steps]]); await p.waitForTimeout(40); }
  await p.waitForTimeout(200);
  await touches('touchEnd', []);
  await p.waitForTimeout(800);
};
const folderOf = async (family) => ((await ex('type.font.library')).folders || []).find((f) => (f.fonts || []).includes(family))?.name ?? null;
const waitFor = async (fn, ms = 8000) => { for (let t = 0; t < ms; t += 200) { const v = await fn(); if (v) return v; await p.waitForTimeout(200); } return null; };

await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
// A user font, dropped on the window (before any document: a drop on an open one's canvas
// places it as a layer).
await p.evaluate((bytes) => {
  const dt = new DataTransfer();
  dt.items.add(new File([new Uint8Array(bytes)], 'Usuario.ttf', { type: 'font/ttf' }));
  for (const c of document.querySelectorAll('canvas')) {
    for (const type of ['dragenter', 'dragover', 'drop']) c.dispatchEvent(new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true, clientX: 180, clientY: 300 }));
  }
}, fontBytes);
await p.waitForTimeout(4000);
await ex('file.new', { width: 1200, height: 1800, background: 'white' });
const family = process.env.FAMILY || 'DejaVu Sans';
await p.waitForTimeout(2500);
await ex('type.fontFolder.create', { name: 'Globos' });
await ex('type.fontFolder.create', { name: 'Gritos' });
await ex('type.create', { text: 'HOLA', size: 120, x: 200, y: 600 });
await req('ui.set', { tool: 'type', fit: true });
await p.waitForTimeout(1200);

// Open the font menu from the type bar (editing the layer: tap on its text).
const openMenu = async () => {
  let r = await rect('typeFont');
  if (!r) {
    const v = await inspect();
    const cr = v.canvasRect;
    await tap([(cr[0] + cr[2]) / 2, (cr[1] + cr[3]) / 2]);
    r = await waitFor(() => rect('typeFont'));
  }
  if (!r) return false;
  await tap(centre(r));
  return !!(await waitFor(() => rect('fontTab:Custom')));
};
check(await openMenu(), 'the font menu opens from the type bar');
await tap(centre(await rect('fontTab:Custom')));
await shot('01-personalizadas');
let row = await waitFor(() => rect(`fontRow:${family}`));
check(!!row, `the user font shows in Custom: ${JSON.stringify(await labels('fontRow:'))}`);

// 1. Press and hold › Mover a… › Globos.
if (row) await hold(centre(row));
let target = await waitFor(() => rect('fontMoveTo:Globos'), 3000);
await shot('02-mantener-pulsado');
check(!!target, 'press and hold on the font shows "Mover a…"');
check(!!(await rect('fontTab:Custom')), 'the menu stays open after the hold');
if (target) await tap(centre(target));
check((await folderOf(family)) === 'Globos', `Mover a… › Globos files it: ${await folderOf(family)}`);
await shot('03-movida');

// 2. Dragged by finger onto a folder (Custom tab).
if (!(await rect('fontTab:Custom'))) await openMenu();
await tap(centre(await rect('fontTab:Custom')));
row = await waitFor(() => rect(`fontRow:${family}`), 3000);
check(!!row && !!(await rect('fontFolder:Gritos')), 'the font and the folder show');
if (row) await holdDrag(centre(row), async () => centre(await rect('fontFolder:Gritos')));
check((await folderOf(family)) === 'Gritos', `a finger drag onto Gritos files it: ${await folderOf(family)}`);
await shot('04-arrastrada');

// 3. Rename (an alias): shown and searched, the layer keeps its family.
row = await waitFor(() => rect(`fontRow:${family}`), 3000);
if (row) await hold(centre(row));
const rename = await waitFor(() => rect('fontRename'), 3000);
check(!!rename, 'Renombrar… shows in the press-and-hold panel');
if (rename) {
  await tap(centre(rename));
  await p.waitForTimeout(400);
  await p.keyboard.press('Control+A');
  await p.keyboard.type('Mi Diálogo');
  await tap(centre(await rect('fontAliasSave')));
}
const lib = await ex('type.font.library');
check(lib.aliases && lib.aliases[family] === 'Mi Diálogo', `the alias is kept: ${JSON.stringify(lib.aliases)}`);
await shot('05-renombrada');

console.log(failed ? `${failed} FAILED` : 'all ok');
await b.close();
process.exit(failed ? 1 : 0);
