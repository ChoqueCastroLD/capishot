// Text Selection on a phone, by finger (390 x 800): the mask model downloads from `models/` next to
// the site's index.html and runs in the AI worker; the pieces it suggests become the selection; a
// tap takes one away; Remove with AI cleans the rest and they leave the pieces.
// PAGE=<a manga page's png> (never part of the repo). With SHOTS=<dir> it saves each step there.
//
//   CHROME=<chromium> PAGE=<png> [SHOTS=<dir>] node text-select-phone.mjs <url ending in />
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
if (shots) fs.mkdirSync(shots, { recursive: true });
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
const ctx = await b.newContext({ viewport: { width: 390, height: 800 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, locale: 'es-ES' });
await ctx.addInitScript(() => Object.defineProperty(navigator, 'deviceMemory', { get: () => 4 }));
const origin = new URL(url).origin;
await ctx.route('**/*', async (route) => {
  const r = route.request();
  // A test site may not have the weights.
  if (r.url().includes('/models/') && process.env.MODELS) {
    const w = await fetch(process.env.MODELS + r.url().split('/').pop());
    return route.fulfill({ status: w.status, headers: { 'access-control-allow-origin': '*', 'cross-origin-resource-policy': 'cross-origin', 'content-type': 'application/octet-stream' }, body: Buffer.from(await w.arrayBuffer()) });
  }
  if (!r.url().startsWith(origin)) return route.abort();
  if (r.resourceType() === 'document') {
    const res = await route.fetch();
    const html = (await res.text()).replace(/<meta name="capishot-ai-models"[^>]*>/, '');
    return route.fulfill({ response: res, body: html.replace('</head>', `<meta name="capishot-ai-models" content="${origin}/models/"></head>`) });
  }
  return route.continue();
});
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
const weights = [];
p.on('request', (r) => { if (r.url().includes('/models/')) weights.push(r.url().split('/').pop()); });
await p.goto(url);
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 600; i++) {
  try {
    await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 60000 });
    if (!(await req('engine.commands')).err) break;
  } catch { /* a reload while it starts */ }
  await p.waitForTimeout(500);
}
const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return (r && r.result) || r; };
const sleep = (ms) => p.waitForTimeout(ms);
const cdp = await ctx.newCDPSession(p);
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };
const ui = async () => { const r = await req('ui.inspect'); return r.result || r; };
const touches = (type, pts) => cdp.send('Input.dispatchTouchEvent', { type, touchPoints: pts.map(([x, y], i) => ({ x, y, id: i })) });
const tap = async (x, y) => { await touches('touchStart', [[x, y]]); await sleep(60); await touches('touchEnd', []); await sleep(600); };
const rect = async (label) => ((await ui()).touch?.rects || []).findLast((x) => x.label === label)?.rect;
const tapRect = async (label) => { const r = await rect(label); if (!r) return false; await tap((r[0] + r[2]) / 2, (r[1] + r[3]) / 2); return true; };
const toScreen = async (x, y) => { const u = await ui(); const v = u.views.at(-1); const r = u.canvasRect; const k = v.zoom / (u.canvasPpp || 2); return [(r[0] + r[2]) / 2 + (x - v.center[0]) * k, (r[1] + r[3]) / 2 + (y - v.center[1]) * k]; };
const running = async () => { const r = await req('jobs.list'); const j = (r && r.result) || r || {}; return (j.jobs || []).filter((x) => x.progress !== 1 && !x.result); };
const covered = async (x, y) => (await ex('select.sample', { x, y })).coverage;

await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
const size = JSON.parse(process.env.PAGE_SIZE || '[1284,1826]');
await ex('file.new', { width: size[0], height: size[1], background: 'white' });
const doc = await ex('document.inspect');
await ex('layer.putPixels', { layer: doc.activeLayer ?? doc.layers?.[0]?.id, src: fs.readFileSync(process.env.PAGE).toString('base64') });
await req('ui.set', { tool: 'TextSelection', fit: true });
await sleep(1500);
await shot('0-tool');
check(await tapRect('textSelectFind'), 'the Find button is there');
const t0 = Date.now();
for (let i = 0; i < 200 && !(await rect('textSelectDone')); i++) await sleep(1500);
console.log(`found in ${Math.round((Date.now() - t0) / 1000)} s`);
check(weights.includes('manga-text-mask-precise-web.safetensors'), `the browser's model came down (${weights})`);
await sleep(1000);
await shot('1-pieces');
// The bar along the top takes touches, and scrolls.
const steps = async () => ((await ex('document.inspect')).history || []).length;
let before = await steps();
check(await tapRect('textSelect:none'), 'None is on the bar');
await sleep(400);
check((await steps()) > before, 'None took the selection');
await shot('1b-none');
before = await steps();
check(await tapRect('textSelect:suggested'), 'Suggested is on the bar');
await sleep(400);
check((await steps()) > before, 'Suggested brought it back');
check(await tapRect('textSelectPaint'), 'the switch for what a drag does is there');
await shot('1c-erasing');
await tapRect('textSelectPaint');
const clean = await tapRect('textSelectClean');
check(clean, 'Remove with AI is on the bar');
for (let i = 0; i < 400 && (await running()).length; i++) await sleep(1500);
await sleep(1500);
await shot('2-cleaned');
await b.close();
process.exit(failed ? 1 : 0);
