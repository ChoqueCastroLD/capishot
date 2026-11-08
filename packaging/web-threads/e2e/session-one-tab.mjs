// One Capishot tab at a time, and a session that comes back.
//  - A page with documents is closed and opened again: the same documents, layers, undo steps,
//    zoom, active document, and a saved document still saved.
//  - A second tab shows "Use here" and doesn't start; choosing it moves the session there, and
//    the first tab pauses with its own "Use here".
//  - The graphics context lost: the page asks how to reload instead of switching to the CPU.
//
//   CHROME=<chromium> PROFILE=<empty dir> node session-one-tab.mjs <url ending in />
import { chromium } from 'playwright-core';

const [url] = process.argv.slice(2);
const ctx = await chromium.launchPersistentContext(process.env.PROFILE, { executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'], viewport: { width: 1200, height: 800 } });
await ctx.route('**/*telemetry*', (r) => r.abort());
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };
const request = (p, m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => (r && r.result) || r, (e) => ({ err: String(e) })), [m, pr]);
const ex = (p, c, pr = {}) => request(p, 'engine.execute', { command: c, params: pr });
const ready = async (p, ms = 120000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    try {
      if (await p.evaluate(() => !!(self.photocraft && self.photocraft.request)) && !(await request(p, 'engine.commands')).err) return true;
    } catch { /* reloading */ }
    await p.waitForTimeout(500);
  }
  return false;
};
const gate = (p) => p.evaluate(() => { const g = document.getElementById('capishot-tab-gate'); return g ? g.innerText : null; }).catch(() => null);
const state = async (p) => {
  const s = await request(p, 'ui.inspect');
  const d = s.document || {};
  const list = (s.session && s.session.documents) || [];
  return {
    docs: list.map((x) => x.name),
    dirty: list.map((x) => x.dirty),
    active: s.session && s.session.active,
    layers: (d.layers || []).length,
    undo: (d.history || []).length,
    zoom: s.views && s.session && s.views[s.session.active] && s.views[s.session.active].zoom,
  };
};

// 1. A session, closed and opened again.
let a = await ctx.newPage();
await a.goto(url);
check(await ready(a), 'Capishot starts');
await ex(a, 'file.new', { width: 900, height: 700, background: 'white', name: 'Primera' });
await ex(a, 'layer.duplicate', {});
await ex(a, 'select.rect', { x: 50, y: 50, width: 200, height: 150 });
await ex(a, 'edit.fill', { color: '#cc2233' });
await ex(a, 'select.deselect');
await ex(a, 'file.new', { width: 600, height: 800, background: 'white', name: 'Segunda' });
await ex(a, 'file.new', { width: 400, height: 400, background: 'white', name: 'Tercera' });
await ex(a, 'document.activate', { document: 1 });
await request(a, 'ui.set', { zoom: 2.5 });
await ex(a, 'layer.duplicate', {});
await ex(a, 'layer.duplicate', {});
const before = await state(a);
console.log('before', JSON.stringify(before));
await a.waitForTimeout(20000);
await a.close({ runBeforeUnload: false });
a = await ctx.newPage();
await a.goto(url);
check(await ready(a), 'opened again');
let after = null;
for (let i = 0; i < 40; i++) {
  await a.waitForTimeout(1000);
  after = await state(a);
  if (after.docs.length >= before.docs.length) break;
}
await a.waitForTimeout(2000);
after = await state(a);
console.log('after ', JSON.stringify(after));
check(JSON.stringify(after.docs) === JSON.stringify(before.docs), 'the same documents, in order');
check(after.layers === before.layers, `the active document's layers (${after.layers})`);
check(after.undo === before.undo, `its undo steps (${after.undo})`);
check(after.zoom === before.zoom, `its zoom (${after.zoom})`);
check(after.active === before.active, `the same active document (${after.active})`);
check(JSON.stringify(after.dirty) === JSON.stringify(before.dirty), `unsaved marks as they were (${after.dirty})`);
check(!(await request(a, 'ui.inspect')).dialogs?.some((d) => d.id === 'recovery'), 'without asking');

// 2. A second tab: "Use here".
const b = await ctx.newPage();
await b.goto(url);
await b.waitForTimeout(8000);
const shown = await gate(b);
check(!!shown && !(await b.evaluate(() => !!self.photocraft).catch(() => false)), `a second tab shows "Use here" and doesn't start (${shown && shown.split('\n')[0]})`);
await ex(a, 'select.rect', { x: 10, y: 10, width: 50, height: 50 });
await ex(a, 'edit.fill', { color: '#2233cc' });
await ex(a, 'select.deselect');
const moved = await state(a);
await b.click('#capishot-tab-gate button');
check(await ready(b), 'chosen, it starts');
let there = null;
for (let i = 0; i < 40; i++) {
  await b.waitForTimeout(1000);
  there = await state(b);
  if (there.docs.length >= moved.docs.length) break;
}
await b.waitForTimeout(2000);
there = await state(b);
console.log('moved ', JSON.stringify(there));
check(JSON.stringify(there.docs) === JSON.stringify(moved.docs) && there.undo === moved.undo, 'with the session, the last edit too');
await a.waitForTimeout(3000);
check(!!(await gate(a)), 'the first tab pauses with its own "Use here"');

// 3. The graphics context lost: a question, not the CPU.
const lost = await b.evaluate(() => {
  const c = document.getElementById('photocraft_canvas');
  const gl = c.getContext('webgl2') || c.getContext('webgl');
  const ext = gl && gl.getExtension('WEBGL_lose_context');
  if (!ext) return 'no webgl';
  ext.loseContext();
  return 'lost';
}).catch((e) => String(e));
console.log('context:', lost);
if (lost === 'lost') {
  let asked = null;
  for (let i = 0; i < 30 && !asked; i++) {
    await b.waitForTimeout(1000);
    asked = await b.evaluate(() => { const g = document.getElementById('capishot-reload-gpu'); return g ? g.closest('div').innerText : null; }).catch(() => null);
  }
  check(!!asked, 'a lost graphics context asks how to reload');
  check(!b.url().includes('cpu=1'), 'and stays on the graphics card meanwhile');
  if (asked) {
    await b.click('#capishot-reload-gpu');
    check(await ready(b), 'reloaded with the graphics card');
    check(!b.url().includes('cpu=1'), 'not on the CPU');
    await b.waitForTimeout(8000);
    const back = await state(b);
    check(JSON.stringify(back.docs) === JSON.stringify(moved.docs), 'the documents back');
  }
}
// 4. The running tab frozen: "Use here" doesn't wait for it, and its work comes along.
{
  const c = (await ready(b, 5000)) ? b : await (async () => { const p = await ctx.newPage(); await p.goto(url); await ready(p); return p; })();
  await ex(c, 'file.new', { width: 500, height: 500, background: 'white', name: 'Congelada' });
  await ex(c, 'layer.duplicate', {});
  await c.waitForTimeout(20000);
  const frozen = await state(c);
  c.evaluate(() => { const end = Date.now() + 900000; while (Date.now() < end); }).catch(() => {});
  const d = await ctx.newPage();
  await d.goto(url);
  await d.waitForTimeout(6000);
  check(!!(await gate(d)), 'with the running tab frozen, a new one shows "Use here"');
  const t0 = Date.now();
  await d.click('#capishot-tab-gate button');
  check(await ready(d), `chosen, it starts without the frozen tab (${Math.round((Date.now() - t0) / 1000)} s)`);
  let got = null;
  for (let i = 0; i < 40; i++) {
    await d.waitForTimeout(1000);
    got = await state(d);
    if (got.docs.includes('Congelada')) break;
  }
  console.log('frozen', JSON.stringify(frozen), 'got', JSON.stringify(got));
  check(got.docs.includes('Congelada'), "and brings the frozen tab's documents");
}
await ctx.close().catch(() => {});
console.log(failed ? `${failed} FAILED` : 'PASS');
process.exit(failed ? 1 : 0);
