// Screenshots of the given built-in themes, each on a desktop window and a phone (360 × 780, simple
// UI), with a document open: the bare window, then a menu open at the given points. Checks that the
// page itself comes out pure white, so nothing a theme draws lands on it.
//
//   CHROME=<chromium> SHOTS=<dir> node theme-shots.mjs <url> mangaLight mangaDark capibaraLight capibaraDark
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url, ...themes] = process.argv.slice(2);
const shots = process.env.SHOTS ?? 'theme-shots';
fs.mkdirSync(shots, { recursive: true });
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };

// `menu`: the points to click, in order, to open a menu (DESKTOP_MENU, PHONE_MENU as JSON override them).
const FORMS = [
  { name: 'desktop', simple: 'off', context: { viewport: { width: 1440, height: 900 }, locale: 'es-ES' }, menu: JSON.parse(process.env.DESKTOP_MENU ?? '[[60, 16]]') },
  { name: 'phone', simple: 'on', context: { viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES' }, menu: JSON.parse(process.env.PHONE_MENU ?? '[[24, 22]]') },
];

for (const form of FORMS) {
  const ctx = await b.newContext(form.context);
  const p = await ctx.newPage();
  p.on('pageerror', (e) => { console.log('PAGEERROR', e.message.slice(0, 300)); failed++; });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return r.result ?? r; };
  await p.goto(url);
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 600000 });
  for (let i = 0; i < 800; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
  await ex('prefs.set', { values: { 'interface.simpleUi': form.simple, 'interface.language': 'es' } });
  await ex('file.new', { width: 1200, height: 1800, background: 'white' });
  await p.waitForTimeout(3000);
  for (const theme of themes) {
    const set = await req('ui.menu.invoke', { id: `window.theme.${theme}` });
    check(!set.err, `${form.name} ${theme}${set.err ? ': ' + set.err : ''}`);
    await req('ui.set', { fit: true });
    await p.waitForTimeout(2500);
    const bare = `${shots}/${theme}-${form.name}.png`;
    const png = await p.screenshot({ path: bare });
    // The document's centre, fitted in the window: a theme's pattern must not reach it.
    const [cx, cy] = form.page ?? [form.context.viewport.width / 2, form.context.viewport.height / 2];
    const scale = form.context.deviceScaleFactor ?? 1;
    const px = await p.evaluate(async ([data, x, y]) => {
      const img = new Image();
      img.src = `data:image/png;base64,${data}`;
      await img.decode();
      const c = new OffscreenCanvas(img.width, img.height).getContext('2d');
      c.drawImage(img, 0, 0);
      const d = c.getImageData(x - 8, y - 8, 16, 16).data;
      let min = 255;
      for (const v of d) min = Math.min(min, v);
      return min;
    }, [png.toString('base64'), Math.round(cx * scale), Math.round(cy * scale)]);
    check(px === 255, `${form.name} ${theme}: the page is pure white (lowest channel ${px})`);
    let i = 0;
    for (const [x, y] of form.menu) {
      await req('ui.click', { x, y });
      await p.waitForTimeout(1500);
      i++;
    }
    if (i) await p.screenshot({ path: `${shots}/${theme}-${form.name}-menu.png` });
    await req('ui.key', { key: 'Escape' });
    await req('ui.key', { key: 'Escape' });
    await p.waitForTimeout(800);
  }
  await ctx.close();
}
await b.close();
process.exit(failed ? 1 : 0);
