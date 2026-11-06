// The phone's menus (360 × 780, touch events through the browser): the top bar's Menú button opens
// a bottom sheet of pages; submenus go forward with Atrás on top, greyed items do nothing, a finger
// scrolls, and a tap outside, a drag down and the phone's Back all close it. Long press on a layer
// and a dropdown open in the same sheet. Works on either web build. With SHOTS=<dir> (default:
// ./mobile-menus-shots) it saves screenshots.
//
//   CHROME=<chromium> SHOTS=<dir> node mobile-menus.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS || 'mobile-menus-shots';
fs.mkdirSync(shots, { recursive: true });
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
const shot = async (name) => { await p.screenshot({ path: `${shots}/${name}.png` }); };
const inspect = async () => { const i = await req('ui.inspect'); return i.result || i; };
const simple = async () => (await inspect()).simpleUi || {};
const rect = async (label) => ((await simple()).buttons || []).findLast((x) => x.label === label)?.rect;
const page = async () => (await simple()).menu;
const centre = (r) => [(r[0] + r[2]) / 2, (r[1] + r[3]) / 2];
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const tap = async ([x, y]) => { await touches('touchStart', [[x, y]]); await p.waitForTimeout(40); await touches('touchEnd', []); await p.waitForTimeout(500); };
const hold = async ([x, y], ms = 900) => { await touches('touchStart', [[x, y]]); for (let t = 0; t < ms; t += 100) { await p.waitForTimeout(100); await touches('touchMove', [[x, y]]); } await touches('touchEnd', []); await p.waitForTimeout(600); };
const drag = async (from, to, steps = 10) => {
  await touches('touchStart', [from]);
  for (let i = 1; i <= steps; i++) { await touches('touchMove', [[from[0] + (to[0] - from[0]) * i / steps, from[1] + (to[1] - from[1]) * i / steps]]); await p.waitForTimeout(16); }
  await touches('touchEnd', []);
  await p.waitForTimeout(600);
};
const tapLabel = async (label) => { const r = await rect(label); check(!!r, `${label} is on screen`); if (r) await tap(centre(r)); return !!r; };
const openMenu = async () => { await tapLabel('menu:open'); await p.waitForTimeout(300); check((await page()) !== null, 'the Menú button opens the sheet'); };

await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
await ex('file.new', { width: 1200, height: 1800, background: 'white' });
await ex('layer.new.layer', {});
await p.waitForTimeout(2500);
await shot('01-top-bar');

await openMenu();
await shot('02-first-page');
for (const m of ['File', 'Edit', 'Image', 'Layer', 'Type', 'Select', 'Filter', 'View', 'Window', 'Help']) {
  const r = await rect(`sheet:sub:${m}`);
  check(!!r && r[3] - r[1] >= 48 && r[3] <= 780, `${m}: a row at least 48 tall`);
}
const handle = await rect('sheet:handle');
check(handle && handle[0] <= 0.5 && handle[2] >= 359.5, 'the sheet spans the width');

await tapLabel('sheet:sub:File');
await shot('03-file-page');
check(JSON.stringify((await page()).path) === '["File"]', 'File is the next page');
check(!(await rect('sheet:sub:Edit')), 'one level at a time');
check(!!(await rect('sheet:back')), 'Atrás on top');
const before = await rect('sheet:item:file.exit') || await rect('sheet:item:file.print');
await drag([180, 700], [180, 300]);
await p.waitForTimeout(400);
const after = await rect('sheet:item:file.exit') || await rect('sheet:item:file.print');
check(before && after && after[1] < before[1], 'a finger scrolls the page');
await shot('04-file-scrolled');

await tapLabel('sheet:back');
await tapLabel('sheet:sub:Window');
await shot('05-window-checks');
check(((await simple()).buttons || []).some((x) => x.label.startsWith('sheet:check:')), 'active items show their check');
await tapLabel('sheet:sub:Theme');
await shot('06-themes-dynamic');
await tapLabel('sheet:back');
await tapLabel('sheet:back');

// A greyed row: listed, and a tap does nothing.
await tapLabel('sheet:sub:Select');
const list = await req('ui.menu.list');
const greyed = (list.result?.items ?? list.result ?? list.items ?? []).find?.((i) => i.path?.[0] === 'Select' && i.enabled === false && i.path.length === 1 && i.label !== '---');
if (greyed) {
  const r = await rect(`sheet:item:${greyed.id}`);
  if (r) { await tap(centre(r)); check(JSON.stringify((await page())?.path) === '["Select"]', 'a greyed row does nothing'); }
}
await shot('07-select-greyed');
await tapLabel('sheet:back');

// Search.
await tapLabel('sheet:search');
await p.keyboard.type('gauss');
await p.waitForTimeout(600);
await shot('08-search');
await tap([180, 30]);
await p.waitForTimeout(300);

// A tap outside.
if ((await page()) === null) await openMenu();
await tap([180, 24]);
check((await page()) === null, 'a tap outside closes the sheet');

// A drag down.
await openMenu();
const h = centre(await rect('sheet:handle'));
await drag(h, [h[0], h[1] + 500]);
check((await page()) === null, 'a drag down closes the sheet');

// The phone's Back: up a page, then closed; the page stays.
await openMenu();
await tapLabel('sheet:sub:Edit');
await p.goBack().catch(() => {});
await p.waitForTimeout(600);
check(JSON.stringify((await page())?.path) === '[]', 'Back goes up a page');
await p.goBack().catch(() => {});
await p.waitForTimeout(600);
check((await page()) === null, 'Back closes the first page');
check(await p.evaluate(() => !!self.photocraft), 'and the app is still here');

// A menu command runs from its row.
await openMenu();
await tapLabel('sheet:sub:Layer');
await tapLabel('sheet:sub:New');
await tapLabel('sheet:item:layer.new.layer');
await p.waitForTimeout(800);
await shot('09-after-run');
check((await page()) === null, 'choosing a command closes the sheet');
const dialog = (await inspect()).dialogs?.length;
if (dialog) await tap([180, 20]);

// A long press on a layer: its menu in the same sheet.
await req('ui.set', { tool: 'move' });
const capi = await rect('capi');
await tap(centre(capi));
await tapLabel('Capas');
await p.waitForTimeout(500);
await hold([180, 420]);
await shot('10-layer-long-press');
check(!!(await rect('sheet:handle')), 'a long press on a layer opens the sheet');
await tap([180, 24]);

// A dropdown (the blend mode) in the same sheet.
await p.waitForTimeout(400);
await tap([120, 150]);
await shot('11-dropdown');
check(!!(await rect('sheet:handle')), 'a dropdown opens the sheet');
await tap([180, 24]);

await b.close();
console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
