// Phone dialogs and the on-screen keyboard: on a 360 x 780 touch phone with a simulated soft
// keyboard (a focused text input shrinks the page by 320 px after 120 ms, as Android's
// resizes-content does, and its blur gives the room back), each dialog opens with no field
// focused (the keyboard stays down), and a field tapped open keeps the focus for 3 s: one focus,
// no blur, at most one keyboard-driven resize. Before the fix the dialog's first field took the
// focus on open, and the keyboard opening, the page re-laying out and the focus bouncing kept
// opening and closing the keyboard. Works on either web build.
//
//   CHROME=<chromium> node keyboard-focus.mjs <url>
import { chromium } from 'playwright-core';
const [url] = process.argv.slice(2);
const W = 360;
const H = 780;
const KB = 320;
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: W, height: H }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'en-US' });
await ctx.exposeBinding('__kb', async ({ page }, open) => {
  await page.setViewportSize({ width: W, height: open ? H - KB : H });
});
await ctx.addInitScript(() => {
  self.__kbLog = [];
  let open = false;
  let timer = 0;
  const t0 = performance.now();
  const log = (k) => self.__kbLog.push([Math.round(performance.now() - t0), k]);
  const isText = (el) => el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA');
  document.addEventListener('focusin', (e) => {
    if (!isText(e.target)) return;
    log('focus');
    clearTimeout(timer);
    timer = setTimeout(() => { if (!open) { open = true; log('kb-open'); self.__kb(true); } }, 120);
  }, true);
  document.addEventListener('focusout', (e) => {
    if (!isText(e.target)) return;
    log('blur');
    clearTimeout(timer);
    timer = setTimeout(() => { if (open) { open = false; log('kb-close'); self.__kb(false); } }, 120);
  }, true);
  addEventListener('resize', () => log('resize'));
});
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = (c, pr = {}) => req('engine.execute', { command: c, params: pr });
const inspect = async () => { const i = await req('ui.inspect'); return i.result || i; };
await req('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'en' } });
await ex('file.new', { width: 1200, height: 1800, background: 'white' });
await p.waitForTimeout(1500);

const kbLog = () => p.evaluate(() => self.__kbLog.splice(0));
// Samples whether a text field has the focus for `ms`, as a run-length list of `F`/`-`.
const sample = async (ms) => {
  const runs = [];
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const k = (await inspect()).textFieldFocused ? 'F' : '-';
    if (runs.at(-1) !== k) runs.push(k);
    await p.waitForTimeout(40);
  }
  return runs.join('');
};
const closeAll = async () => {
  for (const d of (await inspect()).dialogs || []) await req('ui.dialog.cancel', { dialog: d.id });
  // The canvas holds the page's focus, as after a first touch on a phone (which a synthetic
  // tap does not give it); this also takes it from a text input left focused.
  await p.evaluate(() => document.querySelector('canvas').focus({ preventScroll: true }));
  await p.waitForTimeout(600);
  if ((await p.evaluate(() => innerHeight)) !== H) await p.setViewportSize({ width: W, height: H });
  await p.waitForTimeout(400);
  await kbLog();
};

const dialogs = [
  ['New Document', () => req('ui.dialog.open', { kind: 'newDocument' })],
  ['Image Size', () => req('ui.menu.invoke', { id: 'image.imageSize' })],
  ['Canvas Size', () => req('ui.menu.invoke', { id: 'image.canvasSize' })],
  ['Color Picker', () => req('ui.dialog.open', { kind: 'colorPicker' })],
];
let failed = 0;
for (const [name, open] of dialogs) {
  await closeAll();
  const opened = await open();
  if (opened.err || opened.ok === false) { console.log(`FAIL ${name}: did not open ${JSON.stringify(opened).slice(0, 200)}`); failed++; continue; }
  // 1. Opening: no field focused, no keyboard, for 1.5 s.
  const onOpen = await sample(1500);
  const openLog = await kbLog();
  const autofocus = onOpen.includes('F') || openLog.some(([, k]) => k === 'focus');
  // 2. A tap on the first field: the focus holds for 3 s through the keyboard opening.
  const rects = (await inspect()).dialogFieldRects || [];
  if (!rects.length) { console.log(`FAIL ${name}: open ${onOpen} ${JSON.stringify(openLog)}, no field rects`); failed++; continue; }
  const [x0, y0, x1, y1] = rects[0];
  await p.touchscreen.tap((x0 + x1) / 2, (y0 + y1) / 2);
  await p.waitForTimeout(300);
  const held = await sample(3000);
  const tapLog = await kbLog();
  const count = (k) => tapLog.filter(([, e]) => e === k).length;
  const ok = !autofocus && /^-?F$/.test(held) && count('focus') === 1 && count('blur') === 0 && count('resize') <= 1;
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: open ${onOpen} ${openLog.length ? JSON.stringify(openLog) : 'quiet'} | tap -> ${held}, focus ${count('focus')}, blur ${count('blur')}, resize ${count('resize')} ${JSON.stringify(tapLog)}`);
  if (!ok) failed++;
}
await b.close();
console.log(failed ? `${failed} dialog(s) failed` : 'all dialogs kept the keyboard steady');
process.exit(failed ? 1 : 0);
