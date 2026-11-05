// Paint mask, then run on a phone, by finger (360 x 780, touch events through the browser): the
// Remove Tool (content-aware, no model needed) with the switch on; three strokes over three dark
// blobs wait as a mask (nothing runs, no history step); the touch bar's eraser takes the third
// blob back out; the bar's Run fills the mask once (one undo step); Undo brings every blob back.
// Works on either web build. With SHOTS=<dir> it saves a screenshot of each step there.
// TOOL=RemoveAi does the same with Remove with AI: Run downloads the model from `models/` next to
// the site's index.html (a symlink; weights are never part of a build) and fills in one job. It
// must fetch MI-GAN with DEVICE_MEMORY=2 or less and Manga LaMa above that.
//
//   CHROME=<chromium> [SHOTS=<dir>] [TOOL=RemoveAi] [DEVICE_MEMORY=4] node remove-mask-phone.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
const tool = process.env.TOOL || 'Remove';
const ai = tool === 'RemoveAi';
const gb = Number(process.env.DEVICE_MEMORY || '4');
if (shots) fs.mkdirSync(shots, { recursive: true });
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES' });
await ctx.addInitScript((gb) => Object.defineProperty(navigator, 'deviceMemory', { get: () => gb }), gb);
const origin = new URL(url).origin;
await ctx.route('**/*', async (route) => {
  const r = route.request();
  if (!r.url().startsWith(origin)) return route.abort();
  if (ai && r.resourceType() === 'document') {
    const res = await route.fetch();
    const html = (await res.text()).replace(/<meta name="capishot-ai-models"[^>]*>/, '');
    return route.fulfill({ response: res, body: html.replace('</head>', `<meta name="capishot-ai-models" content="${origin}/models/"></head>`) });
  }
  return route.continue();
});
const p = await ctx.newPage();
const weights = [];
p.on('request', (r) => { if (r.url().includes('/models/')) weights.push(r.url().split('/').pop()); });
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return (r && r.result) || r || {}; };
const sleep = (ms) => p.waitForTimeout(ms);
const cdp = await ctx.newCDPSession(p);
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };
const ui = async () => { const r = await req('ui.inspect'); return r.result || r; };
const busy = async () => { const r = await req('jobs.list'); const j = (r && r.result) || r || {}; return (j.jobs || []).some((x) => x.state === 'running'); };
const idle = async () => { for (let i = 0; i < 480 && (await busy()); i++) await sleep(250); await sleep(600); };
const toScreen = async (x, y) => {
  const u = await ui(); const v = u.views.at(-1); const r = u.canvasRect; const k = v.zoom / (u.canvasPpp || 3);
  return [(r[0] + r[2]) / 2 + (x - v.center[0]) * k, (r[1] + r[3]) / 2 + (y - v.center[1]) * k];
};
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const drag = async (from, to) => {
  const a = await toScreen(...from); const z = await toScreen(...to);
  await touches('touchStart', [a]);
  for (let k = 1; k <= 10; k++) { await touches('touchMove', [[a[0] + (z[0] - a[0]) * k / 10, a[1] + (z[1] - a[1]) * k / 10]]); await sleep(16); }
  await touches('touchEnd', []);
  await sleep(400);
};
const rect = async (label) => ((await ui()).touch?.rects || []).findLast((x) => x.label === label)?.rect;
const tapRect = async (label) => {
  const r = await rect(label);
  if (!r) return false;
  const c = [(r[0] + r[2]) / 2, (r[1] + r[3]) / 2];
  await touches('touchStart', [c]); await sleep(60); await touches('touchEnd', []); await sleep(500);
  return true;
};
const mask = async () => (await req('ui.removeMask'))[ai ? 'removeAi' : 'remove'] || {};
const past = async () => ((await ex('document.inspect')).history || []).length;
// A dark blob's centre pixel, read back from the document (0..255 grey).
const grey = async (x, y) => { const c = await ex('document.sampleColor', { x, y, size: 1 }); const v = c.color || c; return Array.isArray(v) ? Math.round(v[0] * 255) : -1; };

await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
await ex('file.new', { width: 1200, height: 1800, background: 'white' });
const blobs = [[300, 400], [800, 900], [400, 1400]];
for (const [x, y] of blobs) await ex('paint.pencil', { points: [[x - 60, y], [x + 60, y]], size: 90, color: '#202020' });
await req('ui.set', { tool, fit: true, brushSize: 200 });
const on = await req('ui.removeMask', { on: true });
check(!on.err && on[ai ? 'removeAi' : 'remove']?.on === true, `the switch is on (${JSON.stringify(on).slice(0, 120)})`);
await sleep(1200);
const before = await past();
check((await grey(...blobs[0])) < 80, 'blobs painted');
await shot('0-blobs');

for (const [x, y] of blobs) await drag([x - 90, y], [x + 90, y]);
await idle();
let m = await mask();
check(m.strokes === 3, `three strokes wait in the mask (${m.strokes})`);
check((await past()) === before, 'no history step yet');
check((await grey(...blobs[0])) < 80, 'nothing removed yet');
await shot('1-mask');

check(await tapRect('removeMaskErase'), 'touch bar: eraser');
check((await req('ui.removeMask')).erase === true, 'eraser on');
await drag([blobs[2][0] - 120, blobs[2][1]], [blobs[2][0] + 120, blobs[2][1]]);
m = await mask();
check(m.strokes === 4 && m.erased === 1, `one erasing stroke (${JSON.stringify(m)})`);
await shot('2-erased');

check(await tapRect('removeMaskRun'), 'touch bar: Run');
await idle();
await sleep(800);
// With AI, Run first downloads the model (no job yet), then fills.
for (let i = 0; ai && i < 600 && (await past()) === before; i++) await sleep(1000);
await idle();
if (ai) {
  const want = gb <= 2 ? 'migan' : 'manga-lama';
  check(weights.some((w) => w.startsWith(want)) && !weights.some((w) => !w.startsWith(want) && w.includes('safetensors')), `${gb} GB fetches ${want} (${[...new Set(weights)]})`);
}
m = await mask();
check(m.strokes === 0, 'the mask is gone after Run');
check((await past()) === before + 1, `one undo step (${before} -> ${await past()})`);
const g = [];
for (const bl of blobs) g.push(await grey(...bl));
check(g[0] > 200 && g[1] > 200, `painted blobs removed (${g})`);
check(g[2] < 80, `erased blob kept (${g[2]})`);
await shot('3-run');

await ex('edit.undo');
await sleep(800);
const u = [];
for (const bl of blobs) u.push(await grey(...bl));
check(u.every((v) => v < 80), `one Undo restores every blob (${u})`);
await shot('4-undo');

console.log(failed ? `${failed} FAILED` : 'all ok');
await b.close();
process.exit(failed ? 1 : 0);
