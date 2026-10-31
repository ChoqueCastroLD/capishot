// Drags with the dock full stay smooth: on a desktop window (1440 x 860) with the Layers,
// Properties, Navigator and History panels open and a 2000 x 3000 page of 24 layers (every third
// a type layer), real mouse drags with Hand, Move, Elliptical Marquee, Brush and Remove with AI
// must not hold the page for more than `limit` ms (the browser's `longtask` entries), from the
// press to a moment after the release, so the frame that commits the gesture counts too. That
// frame used to rebuild the Navigator's image of the whole page on the UI thread: hundreds of
// milliseconds after every stroke or move. Prints each drag's tasks and the app's slow frames
// with what they spent their time on (`perf_span`), and fails when a task went over the limit.
//
//   CHROME=<chromium> node dock-drags.mjs <url> [limit ms = 100] [cpu throttle = 1]
import { chromium } from 'playwright-core';
const [url, limitArg = '100', throttleArg = '1'] = process.argv.slice(2);
const limit = Number(limitArg);
const throttle = Number(throttleArg);
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: 1440, height: 860 }, locale: 'en-US' });
await ctx.addInitScript(() => {
  self.__long = [];
  self.__step = 'load';
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) self.__long.push({ step: self.__step, ms: Math.round(e.duration) }); }).observe({ type: 'longtask', buffered: true });
  } catch (e) {}
});
const p = await ctx.newPage();
const slow = [];
p.on('console', (m) => { const t = m.text(); if (t.includes('frame took')) slow.push({ step: null, text: t.replace(/^.*frame took/, 'frame took').slice(0, 300) }); });
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = (c, pr = {}) => req('engine.execute', { command: c, params: pr });
await req('ui.set', { panels: { layers: true, properties: true, navigator: true, history: true } });
await ex('file.new', { width: 2000, height: 3000, background: 'white' });
let seed = 1;
const rnd = (n) => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed % n; };
for (let i = 0; i < 24; i++) {
  await ex('layer.new.layer');
  if (i % 3 === 2) {
    await ex('type.create', { text: `Globo ${i}: que fue eso`, x: 100 + rnd(1300), y: 200 + rnd(2600), size: 60 });
    continue;
  }
  await ex('select.rect', { x: rnd(1500), y: rnd(2500), width: 500, height: 500 });
  await ex('edit.fill', { color: '#' + (0x100000 + rnd(0xefffff)).toString(16) });
  await ex('select.deselect');
}
await req('ui.set', { fit: true });
await p.waitForTimeout(3000);
const cdp = await ctx.newCDPSession(p);
if (throttle > 1) await cdp.send('Emulation.setCPUThrottlingRate', { rate: throttle });
// Document pixels to window points.
const screen = async (x, y) => {
  const r = (await req('ui.inspect')).result || (await req('ui.inspect'));
  const v = r.views.at(-1);
  const c = r.canvasRect;
  const k = v.zoom / r.window.pixelsPerPoint;
  return [(c[0] + c[2]) / 2 + (x - v.center[0]) * k, (c[1] + c[3]) / 2 + (y - v.center[1]) * k];
};
const results = [];
const drag = async (name, tool, from, to, before) => {
  await req('ui.set', { tool });
  if (before) await before();
  await p.waitForTimeout(800);
  const [x0, y0] = await screen(...from);
  const [x1, y1] = await screen(...to);
  await p.evaluate((s) => { self.__step = s; }, name);
  const mark = slow.length;
  await p.mouse.move(x0, y0);
  await p.mouse.down();
  await p.mouse.move(x1, y1, { steps: 60 });
  await p.mouse.up();
  await p.waitForTimeout(2000);
  await p.evaluate(() => { self.__step = 'idle'; });
  const tasks = await p.evaluate((s) => self.__long.filter((x) => x.step === s).map((x) => x.ms), name);
  const max = Math.max(0, ...tasks);
  results.push({ name, max, tasks: tasks.length });
  console.log(`${max <= limit ? 'PASS' : 'FAIL'} ${name}: longest task ${max} ms (${tasks.length} over 50 ms)`);
  for (const s of slow.slice(mark)) console.log(`  ${s.text}`);
};
await drag('hand', 'hand', [1000, 1500], [700, 1100], async () => { await req('ui.set', { fit: true }); });
await req('ui.set', { fit: true });
const layers = (await ex('layer.list', { flat: true })).layers;
const type = layers.filter((l) => l.kind === 'Type').at(-1);
const [bx, by, bw, bh] = type.bounds;
await drag('move', 'move', [bx + bw / 2, by + bh / 2], [bx + bw / 2 + 400, by + bh / 2 + 300], async () => { await ex('layer.select', { layer: type.id }); });
await drag('ellipse marquee', 'ellipseMarquee', [400, 600], [1400, 1800]);
await ex('select.deselect');
const pixel = layers.filter((l) => l.kind === 'Pixel').at(-1);
await drag('brush', 'brush', [300, 400], [1700, 2600], async () => { await ex('layer.select', { layer: pixel.id }); });
await drag('remove with AI', 'removeAi', [500, 500], [900, 1500]);
await b.close();
const failed = results.filter((r) => r.max > limit);
console.log(failed.length ? `FAIL ${failed.length} of ${results.length} drags held the page over ${limit} ms` : `PASS all ${results.length} drags under ${limit} ms`);
process.exit(failed.length ? 1 : 0);
