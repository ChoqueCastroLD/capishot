// Recovery survives a page killed mid-write: the autosave after a first good checkpoint stops after
// its objects ('objects') or after its descriptor ('descriptor'), the renderer is crashed, and
// Recover on the next load must restore the first checkpoint whole.
//
//   CHROME=<chromium> node recovery-crash.mjs <url> <profile dir> [objects,descriptor]
import { chromium } from 'playwright-core';
import fs from 'node:fs';
const [url, profile, phases = 'objects,descriptor'] = process.argv.slice(2);
const want = (x) => phases.split(',').includes(x);
const results = [];
const ok = (name, cond, extra = '') => { results.push(`${cond ? 'PASS' : 'FAIL'} ${name} ${extra}`); console.log(results.at(-1)); };
const args = ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--hide-crash-restore-bubble', '--no-first-run', '--disable-session-crashed-bubble'];
if (want('recover')) fs.rmSync(profile, { recursive: true, force: true });

async function open(viewport = { width: 1400, height: 860 }) {
  const ctx = await chromium.launchPersistentContext(profile, { executablePath: process.env.CHROME, args, viewport, acceptDownloads: true });
  const p = await ctx.newPage();
  for (const other of ctx.pages()) if (other !== p) await other.close().catch(() => {});
  const errors = [];
  p.on('pageerror', e => { errors.push(e.message); console.log('PAGEERROR', (e.stack || '').slice(0, 3000)); });
  await p.goto(url, { waitUntil: 'load' });
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000, polling: 500 });
  for (let i = 0; i < 200; i++) { if (await p.evaluate(() => self.photocraft.request('engine.commands').then(() => true, () => false))) break; await p.waitForTimeout(1000); }
  p.on('console', m => { const t = m.text(); if (/panic|RuntimeError|unreachable|recover|error/i.test(t)) console.log('console:', t.slice(0, 400)); });
  p.on('crash', () => console.log('page crashed'));
  const req = (m, pr = {}) => Promise.race([p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then(r => r, e => 'ERR ' + String(e)), [m, pr]), new Promise(r => setTimeout(() => r({ timeout: m }), 20000))]);
  return { ctx, p, req, errors };
}
// Close a context whose tab crashed: Playwright may wait forever, so kill its Chrome processes.
async function hardClose(ctx) {
  await Promise.race([ctx.close().catch(() => {}), new Promise(r => setTimeout(r, 5000))]);
  for (const pid of fs.readdirSync('/proc').filter(d => /^\d+$/.test(d))) {
    try { if (fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').includes('--user-data-dir=' + profile)) process.kill(+pid, 'SIGKILL'); } catch (e) {}
  }
  await new Promise(r => setTimeout(r, 1500));
  for (const f of ['SingletonLock', 'SingletonCookie', 'SingletonSocket']) fs.rmSync(`${profile}/${f}`, { force: true });
}
const ex = (req, command, params = {}) => req('engine.execute', { command, params });
const human = async (p, x, y) => {
  await p.mouse.move(x - 3, y - 2); await p.waitForTimeout(60); await p.mouse.move(x, y); await p.waitForTimeout(60);
  await p.mouse.down(); await p.waitForTimeout(50);
  for (const [dx, dy] of [[1, 0], [1, 1]]) { await p.mouse.move(x + dx, y + dy); await p.waitForTimeout(20); }
  await p.mouse.up(); await p.waitForTimeout(300);
};
// Checkpoints in the origin-private file system (or IndexedDB): [{key, files}].
const stored = (p) => Promise.race([new Promise(r => setTimeout(() => r([{ error: 'timeout' }]), 15000)), p.evaluate(async () => {
  const out = [];
  try {
    const root = await (await navigator.storage.getDirectory()).getDirectoryHandle('capishot-recovery', { create: true });
    for await (const [k, h] of root.entries()) { if (h.kind !== 'directory') continue; const files = []; for await (const n of h.keys()) files.push(n); out.push({ key: k, files }); }
  } catch (e) { out.push({ error: String(e) }); }
  return out;
})]);
const dialog = async (req) => { const s = await req('ui.inspect'); return (s.dialogs || []).find(d => d.id === 'recovery'); };
const waitFor = async (p, f, ms, step = 1000) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { const v = await f(); if (v) return v; await p.waitForTimeout(step); } return null; };


const layers = async (req) => ((await ex(req, 'layer.list', { flat: true })).layers || []).map(l => l.name);
const gens = (s) => (s.find(c => c.files) || { files: [] }).files.filter(f => f.startsWith('descriptor.')).sort();
for (const stop of phases.split(',')) {
  fs.rmSync(profile, { recursive: true, force: true });
  const { ctx, p, req } = await open();
  await req('engine.execute', { command: 'prefs.set', params: { path: 'fileHandling.autosaveMinutes', value: 1 } });
  await ex(req, 'file.new', { width: 1200, height: 1600, background: 'white', name: 'Crash ' + stop });
  for (let i = 1; i <= 2; i++) { await ex(req, 'layer.new.layer', { name: `good ${i}` }); await ex(req, 'select.rect', { x: 100 * i, y: 150 * i, width: 400, height: 300 }); await ex(req, 'edit.fill', { contents: 'color', color: '#336699' }); }
  await ex(req, 'select.deselect');
  const first = await waitFor(p, async () => gens(await stored(p)).length >= 1, 150000, 3000);
  const good = await layers(req);
  ok(`${stop}: the first checkpoint is written`, !!first, JSON.stringify(gens(await stored(p))));
  // The next write stops mid-way (a page killed then): after its objects, or after its descriptor.
  await p.evaluate((s) => { self.__capishotRecoveryStop = s; }, stop);
  for (let i = 1; i <= 2; i++) { await ex(req, 'layer.new.layer', { name: `lost ${i}` }); await ex(req, 'select.rect', { x: 120 * i, y: 700, width: 300, height: 300 }); await ex(req, 'edit.fill', { contents: 'color', color: '#cc3333' }); }
  await ex(req, 'select.deselect');
  await p.waitForTimeout(75000);
  console.log(stop, 'stored before the crash', JSON.stringify(gens(await stored(p))), (await stored(p))[0]?.files?.length, 'files');
  const cdp = await ctx.newCDPSession(p);
  await Promise.race([cdp.send('Page.crash').catch(() => {}), new Promise(r => setTimeout(r, 3000))]);
  await hardClose(ctx);
  const b = await open();
  const d = await waitFor(b.p, () => dialog(b.req), 30000, 500);
  ok(`${stop}: Recover is offered`, !!d);
  if (d) {
    await b.p.waitForTimeout(1000);
    const r = (await dialog(b.req)).fields.buttons.recover;
    await human(b.p, (r[0] + r[2]) / 2, (r[1] + r[3]) / 2);
    const back = await waitFor(b.p, async () => { const l = await layers(b.req); return l.length ? l : null; }, 60000, 1000);
    if (!back) {
      const s = await b.req('ui.inspect');
      console.log('DEBUG', JSON.stringify({ dialogs: (s.dialogs || []).map(x => x.id), status: s.status, notices: s.notices, jobs: s.jobs }).slice(0, 1500));
      
    }
    ok(`${stop}: Recover restores the last good checkpoint`, JSON.stringify(back) === JSON.stringify(good), `${JSON.stringify(back)} vs ${JSON.stringify(good)}`);
    const errs = (await b.req('ui.inspect')).notices?.filter(n => n.error).map(n => n.lines) || [];
    ok(`${stop}: no recovery errors`, errs.length === 0, JSON.stringify(errs));
  }
  // Clean up for the next phase.
  await b.p.evaluate(async () => { const root = await navigator.storage.getDirectory(); await root.removeEntry('capishot-recovery', { recursive: true }).catch(() => {}); });
  await hardClose(b.ctx);
}
console.log(results.filter(r => r.startsWith('FAIL')).length ? 'SOME FAILED' : 'ALL PASSED');
