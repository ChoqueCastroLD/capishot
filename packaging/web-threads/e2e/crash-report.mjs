// What a crash reports, end to end, against a real collector on a temporary database: the page runs
// as capichan.com (the only host with statistics) through a local TLS server that also forwards
// /capishot/api to the collector, and every batch is kept to look for user data.
//   - a normal close leaves nothing;
//   - a page merely hidden, or frozen behind the file picker, is not reported by another tab that
//     loads meanwhile, and comes back alive;
//   - one killed behind the picker or in the background is reported with that stage, and the
//     collector doesn't count it as a crash; one killed in front it does;
//   - a panic, on the page or on a worker, and a script error each send `crash` at once, with fixed
//     codes and this tree's locations only.
// Raw DevTools protocol, not Playwright: Playwright emulates focus, so its pages are never hidden.
// Threads build or st.
//
//   CHROME=<chromium> COLLECTOR=<collector app dir> node crash-report.mjs <dist dir>
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import https from 'node:https';
import os from 'node:os';
import path from 'node:path';

const [distArg] = process.argv.slice(2);
if (!distArg || !process.env.CHROME || !process.env.COLLECTOR) {
  console.error('usage: CHROME=<chromium> COLLECTOR=<collector app dir> node crash-report.mjs <dist dir>');
  process.exit(2);
}
const dist = path.resolve(distArg);
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'crash-report-'));
const results = [];
const ok = (name, cond, extra = '') => { results.push(`${cond ? 'PASS' : 'FAIL'} ${name} ${extra}`); console.log(results.at(-1)); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (f, ms, step = 500) => { const t0 = Date.now(); for (;;) { const v = await f(); if (v || Date.now() - t0 > ms) return v; await sleep(step); } };
// A breadcrumb older than this may belong to a dead page (breadcrumb.rs DEAD_MS), and a load looks
// at young ones again a beat after that.
const STALE_MS = 20_000;

// The collector, on a database of its own.
const collectorPort = 18600 + Math.floor(Math.random() * 300);
fs.writeFileSync(path.join(tmp, 'token'), 'crash-report-token');
const collector = spawn(process.execPath, [path.join(process.env.COLLECTOR, 'server.mjs')], {
  env: { ...process.env, PORT: String(collectorPort), DATA_DIR: path.join(tmp, 'data'), TOKEN_FILE: path.join(tmp, 'token') },
  stdio: ['ignore', 'ignore', 'inherit'],
});
const stats = async () => (await fetch(`http://127.0.0.1:${collectorPort}/capishot/api/stats?format=json`, { headers: { authorization: 'Bearer crash-report-token' } })).json();

// The site, with what it takes to be cross-origin isolated, in front of the collector.
execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', path.join(tmp, 'key'), '-out', path.join(tmp, 'cert'), '-subj', '/CN=capichan.com', '-days', '2'], { stdio: 'ignore' });
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png' };
const batches = [];
const site = https.createServer({ key: fs.readFileSync(path.join(tmp, 'key')), cert: fs.readFileSync(path.join(tmp, 'cert')) }, async (req, res) => {
  const url = new URL(req.url, 'https://capichan.com');
  if (url.pathname.startsWith('/capishot/api/')) {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const body = Buffer.concat(chunks).toString('utf8');
    const answer = await fetch(`http://127.0.0.1:${collectorPort}${url.pathname}`, { method: req.method, body: req.method === 'POST' ? body : undefined, headers: req.headers.origin ? { origin: req.headers.origin } : {} });
    batches.push({ body, status: answer.status });
    res.writeHead(answer.status, { 'Cross-Origin-Resource-Policy': 'same-origin' });
    res.end(await answer.text());
    return;
  }
  const file = path.join(dist, decodeURIComponent(url.pathname), url.pathname.endsWith('/') ? 'index.html' : '');
  if (!file.startsWith(dist) || !fs.existsSync(file) || !fs.statSync(file).isFile()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream', 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp', 'Cross-Origin-Resource-Policy': 'same-origin' });
  fs.createReadStream(file).pipe(res);
});
await new Promise((r) => site.listen(0, '127.0.0.1', r));

const events = (e, kind) => batches.filter((b) => b.status === 204).flatMap((b) => JSON.parse(b.body).events).filter((x) => x.e === e && (!kind || x.kind === kind));

const debugPort = 9300 + Math.floor(Math.random() * 600);
const chrome = spawn(process.env.CHROME, [
  '--headless=new', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${path.join(tmp, 'profile')}`, '--no-first-run', '--no-sandbox',
  '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-certificate-errors',
  `--host-resolver-rules=MAP capichan.com 127.0.0.1:${site.address().port}`, 'about:blank',
], { stdio: 'ignore' });
const finish = (code) => {
  collector.kill();
  site.close();
  chrome.once('exit', () => {
    try { fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 5 }); } catch { /* a temp dir left behind */ }
    process.exit(code);
  });
  chrome.kill();
};

let version;
for (let i = 0; i < 150 && !version; i++) {
  try { version = await (await fetch(`http://127.0.0.1:${debugPort}/json/version`)).json(); } catch { await sleep(200); }
}
if (!version) { console.log('FAIL chrome did not start'); finish(1); }
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise((r, e) => { ws.onopen = r; ws.onerror = e; });
let next = 0;
const pending = new Map();
const chooser = new Set();
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
  if (d.method === 'Page.fileChooserOpened') chooser.add(d.sessionId);
};
const send = (method, params = {}, sessionId) => new Promise((r) => {
  const id = ++next;
  pending.set(id, r);
  ws.send(JSON.stringify({ id, method, params, sessionId }));
  // A crashed tab never answers.
  setTimeout(() => { if (pending.delete(id)) r({ error: 'timeout' }); }, 60_000);
});

async function open({ background = false } = {}) {
  const target = await send('Target.createTarget', { url: 'https://capichan.com/', background });
  const targetId = target.result.targetId;
  const session = (await send('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
  await send('Page.enable', {}, session);
  const js = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, session);
    return r.result?.result?.value;
  };
  const ready = await until(() => js(`self.photocraft && self.photocraft.request ? self.photocraft.request('engine.commands').then(() => true, () => false) : false`), 300_000, 1000);
  if (!ready) throw new Error('the app did not start');
  // Statistics start on the first frames, and a hidden page's are slower.
  return {
    targetId,
    session,
    js,
    crumbs: () => js(`Object.keys(localStorage).filter((k) => k.startsWith('capishot.crumb.')).map((k) => JSON.parse(localStorage.getItem(k)))`),
    hidden: () => js('document.hidden'),
    close: () => send('Target.closeTarget', { targetId }),
    crash: async () => { await Promise.race([send('Page.crash', {}, session), sleep(3000)]); await sleep(500); await send('Target.closeTarget', { targetId }); },
  };
}

// A second page that loaded `STALE_MS` after the first stopped beating has had time to look twice.
const settle = async (page) => { await sleep(STALE_MS + 4000); await until(() => page.js('self.photocraft ? true : false'), 5000); };
const exitsBefore = () => events('unclean_exit').length;
const reportingStage = (from, stage) => events('unclean_exit').slice(from).find((x) => x.stage === stage);

const main = async () => {
  // A normal close.
  const first = await open();
  await until(() => batches.length > 0, 60_000);
  ok('statistics reach the collector', batches.some((b) => b.status === 204), JSON.stringify(batches.map((b) => b.status)));
  ok('this page has a breadcrumb and it names no picker', (await first.crumbs()).length === 1, JSON.stringify(await first.crumbs()));
  await first.close();
  await sleep(1000);
  const second = await open();
  await settle(second);
  ok('a normal close is no unclean exit', exitsBefore() === 0, JSON.stringify(events('unclean_exit')));
  ok('and leaves no breadcrumb behind', (await second.crumbs()).length === 1, JSON.stringify(await second.crumbs()));

  // A hidden page, alive, while another tab loads.
  const hidden = await open({ background: true });
  ok('the background page really is hidden', (await hidden.hidden()) === true);
  await sleep(STALE_MS);
  const staleHidden = (await second.crumbs()).find((c) => c.stage === 'hidden');
  ok('its breadcrumb went stale and says hidden', !!staleHidden && Date.now() - staleHidden.t > 15_000, JSON.stringify(await second.crumbs()));
  const third = await open();
  await settle(third);
  ok('a live hidden page is not reported by a tab that loads meanwhile', exitsBefore() === 0, JSON.stringify(events('unclean_exit')));
  ok('and is alive', (await hidden.js(`self.photocraft.request('engine.commands').then(() => true, () => false)`)) === true);

  // Killed in the background: reported, with the stage, as no crash.
  await hidden.crash();
  let from = exitsBefore();
  await third.close();
  const fourth = await open();
  await until(() => reportingStage(from, 'hidden'), 90_000);
  ok('killed in the background: reported as stage hidden', !!reportingStage(from, 'hidden'), JSON.stringify(events('unclean_exit').slice(from)));
  ok('without counting towards a crash loop', !reportingStage(from, 'hidden')?.loop);

  // Frozen behind the picker, as Android does it, while another tab loads: not reported.
  await second.close();
  const picking = fourth;
  await send('Page.setInterceptFileChooserDialog', { enabled: true }, picking.session);
  await picking.js(`self.photocraft.request('ui.menu.invoke', { id: 'file.open' }).then(() => true, () => false)`);
  await until(() => chooser.has(picking.session), 30_000);
  ok('the picker came out', chooser.has(picking.session));
  const crumb = (await picking.crumbs()).at(-1);
  ok('the breadcrumb names the picker, not in cmd', crumb?.stage === 'picker' && !String(crumb?.cmd).startsWith('stage.'), JSON.stringify(crumb));
  await send('Page.setWebLifecycleState', { state: 'frozen' }, picking.session);
  await sleep(STALE_MS);
  from = exitsBefore();
  const fifth = await open();
  await settle(fifth);
  ok('a page frozen behind the picker is not reported by a tab that loads meanwhile', exitsBefore() === from, JSON.stringify(events('unclean_exit').slice(from)));
  await send('Page.setWebLifecycleState', { state: 'active' }, picking.session);
  ok('and is alive', (await picking.js(`self.photocraft.request('engine.commands').then(() => true, () => false)`)) === true);

  // Killed behind the picker: reported, with the stage, as no crash.
  await picking.crash();
  from = exitsBefore();
  await fifth.close();
  const sixth = await open();
  await until(() => reportingStage(from, 'picker'), 90_000);
  ok('killed behind the picker: reported as stage picker', !!reportingStage(from, 'picker'), JSON.stringify(events('unclean_exit').slice(from)));

  // Killed in front: a crash.
  await sixth.js(`self.photocraft.request('engine.execute', { command: 'file.new', params: { width: 64, height: 64, name: 'SecretPage' } })`);
  await sixth.js(`self.photocraft.request('engine.execute', { command: 'layer.new.layer', params: { name: 'SecretLayer' } })`);
  await sleep(3000);
  await sixth.crash();
  from = exitsBefore();
  const seventh = await open();
  const killed = await until(() => events('unclean_exit').slice(from).find((x) => x.src !== 'panic' && !['picker', 'hidden'].includes(x.stage)), 90_000);
  ok('killed in front: reported', !!killed, JSON.stringify(killed));
  ok('it counts towards a crash loop', killed?.loop === 1, JSON.stringify(killed));
  ok('with what the page was doing and how much memory', !!killed?.stage && killed?.memMB > 0 && killed?.docs === 1, JSON.stringify(killed));

  // A script error and a rejection: the name and a location, no text.
  await seventh.js(`setTimeout(() => { throw new TypeError('SecretLayer /home/SecretUser/SecretPage.psd'); }, 0); true`);
  await seventh.js(`Promise.reject(new RangeError('SecretPage out of bounds')); true`);
  const js = await until(() => (events('crash', 'js').length >= 2 ? events('crash', 'js') : null), 15_000);
  ok('a script error and a rejection send crash js at once', js?.length === 2, JSON.stringify(events('crash', 'js')));
  ok('with its name', js?.some((x) => x.name === 'TypeError') && js?.some((x) => x.name === 'RangeError'), JSON.stringify(js));

  // A panic on the page: the beacon goes before the page dies.
  await seventh.js(`self.__capishotCrashTest = 'panic'; true`);
  const panic = await until(() => events('crash', 'panic')[0], 15_000);
  ok('a panic sends crash panic at once', !!panic, JSON.stringify(panic));
  ok('with this tree\'s location and a fixed code', /^apps\/photocraft-web\/src\/breadcrumb\.rs:\d+$/.test(panic?.loc || '') && panic?.msg === 'other', JSON.stringify(panic));
  ok('then the trap, as a script error with a fixed code', !!(await until(() => events('crash', 'js').find((x) => x.name === 'RuntimeError'), 10_000)), JSON.stringify(events('crash', 'js')));
  await sleep(1000);
  await seventh.close();

  // A worker's panic, on the threads build.
  from = exitsBefore();
  const eighth = await open();
  const reported = await until(() => events('unclean_exit').slice(from).find((x) => x.src === 'panic'), 30_000);
  // pagehide ran on the panicked page: the close was clean, its report the beacon. Either way only one crash is sent.
  ok('the panic isn\'t sent twice', events('crash', 'panic').length === 1, JSON.stringify({ reported, panics: events('crash', 'panic') }));
  const threads = await eighth.js('self.crossOriginIsolated && !!self.SharedArrayBuffer');
  if (threads) {
    await eighth.js(`self.__capishotCrashTest = 'worker'; true`);
    const worker = await until(() => (events('crash', 'panic').length >= 2 ? events('crash', 'panic')[1] : null), 20_000);
    ok('a worker\'s panic sends crash panic too', /^apps\/photocraft-web\/src\/breadcrumb\.rs:\d+$/.test(worker?.loc || ''), JSON.stringify(events('crash', 'panic')));
  } else {
    console.log('single-threaded build: no worker panic to test');
  }

  // Nothing a user wrote, and nothing the collector refused.
  const all = batches.map((b) => b.body).join('\n');
  for (const secret of ['Secret', '/home/', '/root/', 'crash test', 'out of bounds']) ok(`no batch holds "${secret}"`, !all.includes(secret));
  const refused = batches.filter((b) => b.status !== 204);
  ok('the collector refused nothing', refused.length === 0, JSON.stringify(refused).slice(0, 800));

  // The collector's view: background deaths aren't crashes.
  const s = await stats();
  const n = (src) => s.crashes.byVersion.filter((x) => x.src === src).reduce((sum, x) => sum + x.n, 0);
  console.log('collector', JSON.stringify(s.crashes));
  ok('the dashboard counts the background kills apart', n('background') === 2, JSON.stringify(s.crashes.byVersion));
  ok('and the kill in front as a crash', n('killed') === 1, JSON.stringify(s.crashes.byVersion));
  ok('crash details are on the dashboard', s.crashes.details.some((d) => d.kind === 'panic') && s.crashes.details.some((d) => d.kind === 'js'), JSON.stringify(s.crashes.details));
};

let failed = false;
try {
  await main();
} catch (e) {
  failed = true;
  console.log('FAIL', e);
}
const bad = failed || results.some((r) => r.startsWith('FAIL'));
console.log(bad ? 'SOME FAILED' : 'ALL PASSED');
finish(bad ? 1 : 0);
