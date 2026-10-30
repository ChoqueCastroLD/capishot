// Loading under a bad network, on a phone (360 x 780, touch, Spanish), for either web build:
//
// 1. Every module file fails to download (the first-load module and its chunks): after one
//    automatic retry the page shows why, with Reload.
// 2. The chunks arrive late: a command that needs one waits for it (no synchronous load on the
//    page) and then runs.
// 3. The chunks never arrive: that command fails cleanly, the app keeps working, and a notice
//    offers to try again.
// 4. With ?split=sync (no waiting) the page's last-resort synchronous load fails: the toast with
//    its reload button shows.
//
//   CHROME=<chromium> node load-failures.mjs <url>
import { chromium } from 'playwright-core';
const [url] = process.argv.slice(2);
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const results = [];
const ok = (n, c, e = '') => { results.push(`${c ? 'PASS' : 'FAIL'} ${n} ${e}`); console.log(results.at(-1)); };
const phone = () => b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, locale: 'es-ES' });
const MODULE = /_bg(\.c\d+)?\.wasm/;
const CHUNK = /_bg\.c\d+\.wasm/;
// A filter: its code is in a chunk.
const COLD = { command: 'filter.noise.addNoise', params: { amount: 10 } };
const start = async (p, u) => {
  await p.goto(u);
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
  await req('engine.execute', { command: 'file.new', params: { width: 600, height: 400, background: 'white' } });
  return req;
};

{
  const ctx = await phone();
  const p = await ctx.newPage();
  let hits = 0;
  await ctx.route(MODULE, (r) => { hits++; r.abort(); });
  await p.goto(url).catch(() => {});
  await p.waitForSelector('#photocraft_load_failed', { timeout: 120000 }).catch(() => {});
  const text = await p.evaluate(() => document.getElementById('photocraft_load_failed')?.textContent || '');
  ok('a failed download shows the reload message', text.includes('No se pudo cargar Capishot'), `${text} (module fetches: ${hits})`);
  ok('after one automatic retry', hits >= 2, String(hits));
  await ctx.close();
}

{
  const ctx = await phone();
  const p = await ctx.newPage();
  await ctx.route(CHUNK, async (r) => { await new Promise((x) => setTimeout(x, 8000)); await r.continue(); });
  const req = await start(p, url);
  const r = await req('engine.execute', COLD);
  const split = await p.evaluate(() => self.capishotSplit && { sync: self.capishotSplit.state.syncLoads });
  ok('a command waits for its late chunk, then runs', r && r.filter && r.filter.filter === 'addNoise', JSON.stringify(r).slice(0, 120));
  ok('no synchronous load on the page', split && split.sync === 0, JSON.stringify(split));
  await ctx.close();
}

{
  const ctx = await phone();
  const p = await ctx.newPage();
  await ctx.route(CHUNK, (r) => r.abort());
  const req = await start(p, url);
  const r = await req('engine.execute', COLD);
  // The notice shows now (notices hide by themselves after a while).
  const notices = JSON.stringify((await req('ui.inspect')).notices || []);
  ok('without its chunk the command fails cleanly', /didn't load/.test(JSON.stringify(r)), JSON.stringify(r).slice(0, 160));
  ok('a notice offers to try again', notices.includes('app.loadCode.retry'), notices.slice(0, 200));
  const after = await req('engine.execute', { command: 'layer.new.layer', params: {} });
  ok('the app keeps working', after && !after.err && after.ok !== false, JSON.stringify(after).slice(0, 120));
  await ctx.close();
}

{
  const ctx = await phone();
  const p = await ctx.newPage();
  await ctx.route(CHUNK, (r) => r.abort());
  const u = new URL(url);
  u.searchParams.set('split', 'sync');
  const req = await start(p, u.href);
  // Not awaited: the call that needed the chunk never returns.
  p.evaluate((c) => { self.photocraft.request('engine.execute', c).catch(() => {}); }, COLD).catch(() => {});
  await p.waitForSelector('#photocraft_chunk_failed', { timeout: 30000 }).catch(() => {});
  const toast = await p.evaluate(() => document.getElementById('photocraft_chunk_failed')?.textContent || '');
  ok('a failed synchronous load shows the toast', toast.includes('No se pudo cargar una parte de Capishot'), toast);
  await ctx.close();
}

await b.close();
const failed = results.filter((r) => r.startsWith('FAIL')).length;
console.log(failed ? `${failed} FAILED` : 'ALL PASSED');
process.exit(failed ? 1 : 0);
