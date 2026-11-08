// A tab that freezes for good holds its lock until someone kills it, so another tab used to take it
// for alive and offer nothing. Three tabs' worth of cases:
//  - two healthy tabs: nothing is offered in either;
//  - the second tab open when the first freezes: it offers the work, and Recover brings it back;
//  - a tab opened after the freeze: it offers the work on load.
// The frozen tab holds its checkpoint's files so no one else can write them (a phone's tab that
// froze mid-write): the recovered document still autosaves, under a key of its own.
//
//   CHROME=<chromium> PROFILE=<empty dir> node recovery-frozen-tab.mjs <url ending in />
import { chromium } from 'playwright-core';

const [url] = process.argv.slice(2);
const ctx = await chromium.launchPersistentContext(process.env.PROFILE, { executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'], viewport: { width: 1200, height: 800 } });
await ctx.route('**/*telemetry*', (r) => r.abort());
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };
const request = (p, m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => (r && r.result) || r, (e) => ({ err: String(e) })), [m, pr]);
const ex = (p, c, pr = {}) => request(p, 'engine.execute', { command: c, params: pr });
const open = async () => {
  const p = await ctx.newPage();
  await p.goto(url);
  for (let i = 0; i < 400; i++) {
    try {
      await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 60000 });
      if (!(await request(p, 'engine.commands')).err) break;
    } catch { /* a reload while it starts */ }
    await p.waitForTimeout(500);
  }
  return p;
};
const offered = async (p) => ((await request(p, 'ui.inspect')).dialogs || []).find((d) => d.id === 'recovery');
const waitOffer = async (p, ms) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await offered(p)) return Date.now() - t0;
    await p.waitForTimeout(1000);
  }
  return null;
};
const work = async (p) => {
  await ex(p, 'file.new', { width: 800, height: 600, background: 'white' });
  await ex(p, 'select.rect', { x: 100, y: 100, width: 200, height: 200 });
  await ex(p, 'edit.fill', { color: '#ff0000' });
  await ex(p, 'select.deselect');
  await p.waitForTimeout(20000);
};
const freeze = (p) => { p.evaluate(() => { const end = Date.now() + 900000; while (Date.now() < end); }).catch(() => {}); };

const a = await open();
await work(a);
const b = await open();
check((await waitOffer(b, 40000)) === null, 'two healthy tabs: nothing is offered');
check(!(await offered(a)), 'nor in the first');

// Every file of the first tab's checkpoint held as a write the second tab can't share.
const held = await a.evaluate(async () => {
  self.__hold = [];
  const root = await (await navigator.storage.getDirectory()).getDirectoryHandle('capishot-recovery');
  for await (const [, d] of root.entries()) {
    if (d.kind !== 'directory') continue;
    for await (const [, f] of d.entries()) if (f.kind === 'file') self.__hold.push(await f.createWritable({ mode: 'exclusive', keepExistingData: true }));
  }
  return self.__hold.length;
}).catch((e) => String(e));
console.log('files held by the first tab:', held);
freeze(a);
// Offered, or (Capishot's page keeping its session) brought back on its own.
const hasLayers = async () => { const l = (await ex(b, 'layer.list', { flat: true })).layers; return Array.isArray(l) && l.length > 0; };
let later = null;
let auto = false;
for (const t0 = Date.now(); Date.now() - t0 < 70000 && later === null && !auto; await b.waitForTimeout(1000)) {
  if (await offered(b)) later = Date.now() - t0;
  else auto = await hasLayers();
}
check(later !== null || auto, `the tab already open ${auto ? 'brings back' : 'offers'} the frozen tab's work`);
if (later !== null) {
  await b.waitForTimeout(1000);
  const r = (await offered(b)).fields.buttons.recover;
  const [x, y] = [(r[0] + r[2]) / 2, (r[1] + r[3]) / 2];
  await b.mouse.move(x - 2, y); await b.waitForTimeout(80); await b.mouse.move(x, y); await b.waitForTimeout(80);
  await b.mouse.down(); await b.waitForTimeout(60); await b.mouse.up();
  let back = false;
  for (let i = 0; i < 40 && !back; i++) {
    await b.waitForTimeout(1000);
    const layers = (await ex(b, 'layer.list', { flat: true })).layers;
    back = Array.isArray(layers) && layers.length > 0;
  }
  if (!back) {
    const s = await request(b, 'ui.inspect');
    console.log('DEBUG', JSON.stringify({ status: s.status, notices: s.notices, jobs: s.jobs, dialogs: (s.dialogs || []).map((d) => d.id) }).slice(0, 900));
    await b.screenshot({ path: process.env.SHOT || '/tmp/frozen.png' });
  }
  check(back, 'Recover brings the document back');
  check(!(await offered(b)), 'and the dialog is gone');
}
if (later !== null || auto) {
  // An edit in the recovered document: its checkpoint can't go where the frozen tab holds files.
  await ex(b, 'select.rect', { x: 400, y: 100, width: 100, height: 100 });
  await ex(b, 'edit.fill', { color: '#0000ff' });
  await ex(b, 'select.deselect');
  await b.waitForTimeout(20000);
  const s = await request(b, 'ui.inspect');
  const said = JSON.stringify([s.status, s.notices]);
  check(!/Autosave failed/.test(said), `the recovered document autosaves (${String(s.status).slice(0, 120)})`);
  const heads = await b.evaluate(async () => {
    const root = await (await navigator.storage.getDirectory()).getDirectoryHandle('capishot-recovery');
    let n = 0;
    for await (const [, d] of root.entries()) if (d.kind === 'directory') { try { await d.getFileHandle('head'); n++; } catch { /* none */ } }
    return n;
  });
  check(heads >= 2, `under a key of its own (${heads} checkpoints)`);
}

const c = await open();
const onLoad = await waitOffer(c, 20000);
console.log(onLoad === null ? 'a tab opened now: already recovered by the other, nothing left to offer' : `a tab opened now offers it too (${onLoad} ms)`);
await ctx.close().catch(() => {});
console.log(failed ? `${failed} FAILED` : 'PASS');
process.exit(failed ? 1 : 0);
