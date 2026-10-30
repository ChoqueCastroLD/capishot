// The basic page flows on a module instrumented by wasm-split, then its profile (profile.sh):
//   CHROME=<chromium> CAPISHOT_PROFILE_PAGE=<a page image> node profile-flow.mjs <url> <out.prof> <desktop|phone|phone360> [locale]
import { chromium } from 'playwright-core';
import fs from 'node:fs';
const [url, out, mode = 'desktop', locale = 'es-ES'] = process.argv.slice(2);
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
const phone = mode.startsWith('phone');
const small = mode === 'phone360';
const ctx = await b.newContext(phone
  ? { viewport: small ? { width: 360, height: 780 } : { width: 412, height: 915 }, deviceScaleFactor: small ? 2 : 2.625, isMobile: true, hasTouch: true, locale, acceptDownloads: true }
  : { viewport: { width: 1400, height: 860 }, locale, acceptDownloads: true });
await ctx.addInitScript(() => {
  delete self.showSaveFilePicker; delete Window.prototype.showSaveFilePicker;
  // The app asks the code gate about every command it runs (and `open:<ext>` per file): those
  // are the commands that run without waiting for the chunks (hot-commands*.txt).
  self.__asked = new Set();
  // A made-up command waits twice, so the waiting itself (the job card, the held request) and a
  // failed load (its notice) are profiled: the page must never need a chunk for those.
  let waits = 0;
  self.capishotSplit = {
    ready(id) { if (id === 'app.loadCode.profile') return false; self.__asked.add(id); return true; },
    whenLoaded(progress) {
      const fail = waits++ % 2 === 1;
      return new Promise((r, x) => { setTimeout(() => progress && progress(0.5), 700); setTimeout(() => (fail ? x(new Error('network')) : r()), 1500); });
    },
    firstFrame() {},
  };
});
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 200)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 100; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(500); }
const ex = (command, params = {}) => req('engine.execute', { command, params });
const sleep = (ms) => p.waitForTimeout(ms);
const ui = () => req('ui.inspect');
const toScreen = async (x, y) => { const u = await ui(); const v = u.views.at(-1); const r = u.canvasRect; const k = v.zoom / (phone ? (small ? 2 : 2.625) : 1); return [(r[0] + r[2]) / 2 + (x - v.center[0]) * k, (r[1] + r[3]) / 2 + (y - v.center[1]) * k]; };
const drag = async (pts) => {
  if (phone) {
    const cdp = await ctx.newCDPSession(p);
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: pts[0][0], y: pts[0][1] }] });
    for (const [x, y] of pts.slice(1)) { await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y }] }); await sleep(16); }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  } else {
    await p.mouse.move(pts[0][0], pts[0][1]); await p.mouse.down();
    for (const [x, y] of pts.slice(1)) { await p.mouse.move(x, y); await sleep(16); }
    await p.mouse.up();
  }
  await sleep(500);
};
const tap = async (x, y) => drag([[x, y], [x, y]]);
const step = async (name, f) => { try { await f(); } catch (e) { console.log('step failed', name, String(e).slice(0, 150)); } await sleep(300); };

// Open a real page (JPEG) and a layered PSD-like document; paint; move; type; bubble; save.
const page = [...fs.readFileSync(process.env.CAPISHOT_PROFILE_PAGE)];
const pageName = process.env.CAPISHOT_PROFILE_PAGE.split('/').pop();
await step('open jpg', async () => { await p.evaluate(([b, n]) => self.photocraft.open(new Uint8Array(b), n), [page, pageName]); await sleep(2500); await req('ui.set', { fit: true }); });
await step('code wait', () => req('engine.execute', { command: 'app.loadCode.profile', params: {} }));
await step('code wait fails', () => req('engine.execute', { command: 'app.loadCode.profile', params: {} }));
await step('new layer', () => ex('layer.new.layer', {}));
await step('brush', async () => { await req('ui.set', { tool: 'brush' }); const [ax, ay] = await toScreen(300, 600); const [bx, by] = await toScreen(1200, 1800); await drag(Array.from({ length: 30 }, (_, i) => [ax + (bx - ax) * i / 29, ay + (by - ay) * i / 29])); });
await step('eraser', async () => { await req('ui.set', { tool: 'eraser' }); const [ax, ay] = await toScreen(400, 700); await drag(Array.from({ length: 12 }, (_, i) => [ax + i * 6, ay + i * 4])); });
await step('move', async () => { await req('ui.set', { tool: 'move' }); const [mx, my] = await toScreen(800, 1200); await drag(Array.from({ length: 20 }, (_, i) => [mx + i * 3, my + i * 2])); });
await step('marquee+fill', async () => { await ex('select.rect', { x: 100, y: 100, width: 300, height: 200 }); await ex('edit.fill', { color: '#ffffff' }); await ex('select.deselect'); });
await step('type', async () => { await req('ui.set', { tool: 'type' }); const [x, y] = await toScreen(500, 500); await tap(x, y); await p.keyboard.insertText('Hola mundo, esto es una prueba.'); await p.keyboard.press('Control+Enter'); });
await step('bubble', async () => { await req('ui.set', { tool: 'bubbleType' }); const [x, y] = await toScreen(1304, 246); await tap(x, y); await p.keyboard.insertText('¡Texto en el globo!'); await p.keyboard.press('Control+Enter'); });
await step('type edit', async () => { const l = (await ex('layer.list', { flat: true })).layers || []; const t = l.find((x) => x.kind === 'Type'); if (t) await ex('type.edit', { layer: t.id, size: 24 }); });
await step('undo redo', async () => { for (let i = 0; i < 4; i++) await ex('edit.undo'); for (let i = 0; i < 4; i++) await ex('edit.redo'); });
await step('zoom pan', async () => { await req('ui.set', { zoom: 2 }); await req('ui.set', { fit: true }); });
await step('layers', async () => { await ex('layer.new.layer', {}); await ex('layer.duplicate', {}); await ex('layer.delete', {}); });
await step('save psd', async () => { const dl = p.waitForEvent('download', { timeout: 120000 }); await p.keyboard.press('Control+Shift+KeyS'); await sleep(1500); await p.keyboard.press('Enter'); const f = await dl; await f.cancel().catch(() => {}); });
await step('export png', async () => { await ex('file.export.quickPng', {}).catch(() => {}); });
await step('new doc', () => ex('file.new', { width: 800, height: 600, background: 'white' }));
for (const t of ['move', 'marquee', 'lasso', 'quickSelection', 'crop', 'eyedropper', 'healing', 'brush', 'cloneStamp', 'eraser', 'gradient', 'paintBucket', 'type', 'bubbleType', 'pen', 'rectangle', 'hand', 'zoom']) {
  await step('tool ' + t, async () => { await req('ui.set', { tool: t }); await sleep(250); });
}
await step('panels', async () => { for (const id of ['window.layers', 'window.history', 'window.character', 'window.paragraph', 'window.properties']) { await req('ui.menu.invoke', { id }).catch(() => {}); await sleep(300); } });
await step('menus', async () => { for (const k of ['Alt+KeyF', 'Escape', 'Alt+KeyE', 'Escape', 'Alt+KeyL', 'Escape']) { await p.keyboard.press(k); await sleep(250); } });
await step('reopen', async () => { await p.evaluate(([b, n]) => self.photocraft.open(new Uint8Array(b), 'again-' + n), [page, pageName]); await sleep(2000); });
await sleep(2000);
// The profile: wasm-split's __write_profile into a buffer from the module's allocator (profile.sh
// makes the glue keep the page's instance in self.__wasm).
const prof = await p.evaluate(() => {
  const w = self.__wasm;
  const size = 8 * 1024 * 1024;
  const ptr = w.__wbindgen_malloc(size, 1);
  const n = w.__write_profile(ptr, size);
  const bytes = new Uint8Array(w.memory.buffer, ptr, n).slice();
  return Array.from(bytes);
});
fs.writeFileSync(out, Buffer.from(prof));
fs.writeFileSync(`${out}.commands`, (await p.evaluate(() => [...self.__asked].join('\n'))) + '\n');
console.log('profile bytes', prof.length);
await b.close();
