// Crash recovery on a phone: a manga page edited (Remove with AI, a text layer), then the tab dies
// and the page is opened again. The recovery prompt must appear with the edits, at most ~30 s of
// work lost, and the usage statistics must report the crash (session_start + unclean_exit).
// 360 x 780 touch, deviceMemory 4 or 2 (DEVICE_MEMORY), CPU 4x once the app is interactive, with
// the default autosave preferences.
//
// Ways the tab dies (MODES, comma-separated, default all):
//   crash      the renderer crashes (CDP Page.crash: out of memory)
//   kill       the renderer process is killed (SIGKILL)
//   close      the page is closed, its `pagehide` runs (Android closing a frozen tab, a swipe)
//   devicelost the WebGPU/WebGL device is lost: the page reloads on the CPU canvas by itself
//   twice      a crash, then a second crash while the recovered page is loading the documents
//
//   CHROME=<chromium> [DEVICE_MEMORY=4] [MODES=crash,close] [LOSS_S=25] \
//     node recovery-phone.mjs <built site dir> <page.jpg> <models dir> <profile dir>
//
// The site is served here (COOP/COEP headers, like serve.py) under the host name capichan.com
// (mapped to 127.0.0.1, so the usage statistics are on; they go to this server, never out).
// `models dir` holds manga-lama-f16.safetensors (never part of a build). The page is a real or
// synthetic manga page (not committed).
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { execSync } from 'node:child_process';

const [site, pageFile, modelsDir, profile] = process.argv.slice(2);
if (!site || !pageFile || !modelsDir || !profile) {
  console.log('usage: node recovery-phone.mjs <site dir> <page.jpg> <models dir> <profile dir>');
  process.exit(2);
}
const gb = Number(process.env.DEVICE_MEMORY || '4');
const lossS = Number(process.env.LOSS_S || '25');
const modes = (process.env.MODES || 'crash,kill,close,devicelost,twice').split(',');
const pageName = path.basename(pageFile);

// Statistics batches the pages sent: [{t, events}].
const beacons = [];
const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml' };
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const headers = { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp', 'Cross-Origin-Resource-Policy': 'same-origin', 'Cache-Control': 'no-cache' };
  if (req.method === 'POST' && url.pathname === '/capishot/api/t') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      try { beacons.push({ t: Date.now(), events: JSON.parse(body).events || [] }); } catch (e) { beacons.push({ t: Date.now(), bad: body.slice(0, 200) }); }
      res.writeHead(204, headers).end();
    });
    return;
  }
  let file;
  if (url.pathname.startsWith('/__e2e_pages/')) file = pageFile;
  else if (url.pathname.startsWith('/models/')) file = path.join(modelsDir, path.basename(url.pathname));
  else file = path.join(site, decodeURIComponent(url.pathname).replace(/\/$/, '/index.html'));
  if (!path.resolve(file).startsWith(path.resolve(site)) && file !== pageFile && !file.startsWith(modelsDir)) return res.writeHead(403).end();
  fs.stat(file, (err, st) => {
    if (err || !st.isFile()) return res.writeHead(404, headers).end();
    const type = types[path.extname(file)] || 'application/octet-stream';
    if (path.basename(file) === 'index.html') {
      // The AI models from this server.
      const meta = `<meta name="capishot-ai-models" content="${origin}/models/">`;
      let html = fs.readFileSync(file, 'utf8').replace(/<meta name="capishot-ai-models"[^>]*>/, '');
      html = html.replace(/<head>/i, `<head>${meta}`);
      return res.writeHead(200, { ...headers, 'Content-Type': type }).end(html);
    }
    res.writeHead(200, { ...headers, 'Content-Type': type, 'Content-Length': st.size });
    fs.createReadStream(file).pipe(res);
  });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
const origin = `http://capichan.com:${port}`;

const results = [];
const ok = (name, cond, extra = '') => { results.push(`${cond ? 'PASS' : 'FAIL'} ${name} ${extra}`); console.log(results.at(-1)); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const args = [
  '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--hide-crash-restore-bubble', '--no-first-run',
  '--disable-session-crashed-bubble', `--host-resolver-rules=MAP capichan.com 127.0.0.1`, `--unsafely-treat-insecure-origin-as-secure=${origin}`,
];

// Kills every process of the profile's browser (a crashed tab can leave Playwright waiting).
function killProfile() {
  for (const pid of fs.readdirSync('/proc').filter((d) => /^\d+$/.test(d))) {
    try { if (fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').includes('--user-data-dir=' + profile)) process.kill(+pid, 'SIGKILL'); } catch (e) { /* gone */ }
  }
}
async function hardClose(ctx) {
  await Promise.race([ctx.close().catch(() => {}), sleep(5000)]);
  killProfile();
  await sleep(1500);
  for (const f of ['SingletonLock', 'SingletonCookie', 'SingletonSocket']) fs.rmSync(`${profile}/${f}`, { force: true });
}
// The renderer processes of this test's browser.
function renderers() {
  const rows = execSync('ps -eo pid=,ppid=,args=', { encoding: 'utf8' }).split('\n').map((l) => l.trim().split(/\s+/)).filter((r) => r.length > 2);
  const kids = new Set([String(process.pid)]);
  for (let grew = true; grew;) { grew = false; for (const r of rows) if (kids.has(r[1]) && !kids.has(r[0])) { kids.add(r[0]); grew = true; } }
  return rows.filter((r) => kids.has(r[0]) && r.slice(2).join(' ').includes('--type=renderer') && !r.slice(2).join(' ').includes('--extension-process')).map((r) => +r[0]);
}

async function open() {
  const ctx = await chromium.launchPersistentContext(profile, {
    executablePath: process.env.CHROME, args,
    viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES',
    userAgent: 'Mozilla/5.0 (Linux; Android 14; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36',
  });
  await ctx.addInitScript((gb) => Object.defineProperty(navigator, 'deviceMemory', { get: () => gb }), gb);
  const p = await ctx.newPage();
  for (const other of ctx.pages()) if (other !== p) await other.close().catch(() => {});
  const s = { ctx, p, crashed: false, navigations: 0 };
  p.on('crash', () => { s.crashed = true; });
  p.on('framenavigated', (f) => { if (f === p.mainFrame()) s.navigations++; });
  p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
  p.on('console', (m) => { const t = m.text(); if (/panic|recover|lost|didn't close/i.test(t)) console.log('console:', t.slice(0, 300)); });
  await p.goto(origin + '/', { waitUntil: 'load' });
  s.req = (m, pr = {}) => Promise.race([
    p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]).catch((e) => ({ err: String(e) })),
    sleep(30000).then(() => ({ err: 'timeout ' + m })),
  ]);
  await booted(s);
  return s;
}
async function booted(s) {
  for (let i = 0; i < 600; i++) {
    const ready = await s.p.evaluate(() => !!(self.photocraft && self.photocraft.request)).catch(() => false);
    if (ready && !(await s.req('engine.commands')).err) return true;
    await sleep(500);
  }
  return false;
}
const ex = async (s, command, params = {}, wait) => { const r = await s.req('engine.execute', wait === undefined ? { command, params } : { command, params, wait }); return (r && r.result) || r || {}; };
const busy = async (s) => { const r = await s.req('jobs.list'); const j = (r && r.result) || r || {}; return (j.jobs || []).some((x) => x.state === 'running'); };
const idle = async (s) => { for (let i = 0; i < 1200 && (await busy(s)); i++) await sleep(250); };
const dialog = async (s) => { const r = await s.req('ui.inspect'); return ((r && r.dialogs) || []).find((d) => d.id === 'recovery'); };
const waitFor = async (f, ms, step = 500) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { const v = await f(); if (v) return v; await sleep(step); } return null; };
const layers = async (s) => ((await ex(s, 'layer.list', { flat: true })).layers || []).map((l) => l.name);
// The page's pixels where the edits went.
const spots = (W, H) => [[0.3, 0.2], [0.32, 0.21], [0.28, 0.19], [0.5, 0.5]].map(([fx, fy]) => [Math.round(fx * W), Math.round(fy * H)]);
const pixels = async (s, W, H) => { const out = []; for (const [x, y] of spots(W, H)) out.push(JSON.stringify(await ex(s, 'document.pixel', { x, y }))); return out; };
const tap = async (s, rect) => { const [x, y] = [(rect[0] + rect[2]) / 2, (rect[1] + rect[3]) / 2]; await s.p.mouse.move(x, y); await s.p.mouse.down(); await sleep(60); await s.p.mouse.up(); };
const statsEvents = (since) => beacons.filter((b) => b.t >= since).flatMap((b) => b.events || []);

// Opens the page, edits it like a scanlator: Remove with AI over an SFX, then a text layer.
async function edit() {
  const s = await open();
  await s.p.evaluate(async (n) => { const r = await fetch('/__e2e_pages/' + encodeURIComponent(n)); self.photocraft.open(await r.blob(), n); }, pageName);
  await waitFor(async () => { const d = await ex(s, 'document.inspect'); return d && d.name === pageName; }, 180000);
  await idle(s);
  const d = await ex(s, 'document.inspect');
  const [W, H] = [d.width || d.size?.[0], d.height || d.size?.[1]];
  const cdp = await s.ctx.newCDPSession(s.p);
  await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  const [cx, cy] = spots(W, H)[0];
  const pts = [];
  for (let k = 0; k <= 8; k++) pts.push([cx - 200 + k * 50, cy + (k % 2 ? 150 : -150), 1]);
  const ai = await s.req('engine.execute', { command: 'edit.removeAi', params: { strokes: [pts], size: 60, allowDownload: true }, wait: true });
  await idle(s);
  const typed = await ex(s, 'type.create', { x: Math.round(W * 0.2), y: Math.round(H * 0.4), text: 'RECUPERAR 123', size: 48 });
  const editedAt = Date.now();
  const want = { layers: await layers(s), pixels: await pixels(s, W, H) };
  console.log('edited', JSON.stringify({ W, H, ai: ai && (ai.err || ai.error || 'ok'), typed: typed.err || 'ok', layers: want.layers }));
  return { s, cdp, W, H, want, editedAt };
}

// Opens the page again: the prompt must list the document; Recover must bring the edits back.
async function reopen(name, want, W, H, { auto = false, crashWhileLoading = false } = {}) {
  const since = Date.now();
  const s = await open();
  if (!auto) {
    const d = await waitFor(() => dialog(s), 60000);
    ok(`${name}: the recovery prompt appears`, !!d, d ? JSON.stringify(d.fields?.entries || d.fields?.names || '').slice(0, 200) : '');
    if (!d) return s;
    await sleep(800);
    const r = (await dialog(s))?.fields?.buttons?.recover;
    if (r) await tap(s, r);
    if (crashWhileLoading) return s;
  }
  const back = await waitFor(async () => { const l = await layers(s); return l.length >= want.layers.length ? l : null; }, 120000, 1000);
  ok(`${name}: the layers come back`, JSON.stringify(back) === JSON.stringify(want.layers), `${JSON.stringify(back)} vs ${JSON.stringify(want.layers)}`);
  await idle(s);
  const px = back ? await pixels(s, W, H) : [];
  ok(`${name}: the edited pixels come back`, JSON.stringify(px) === JSON.stringify(want.pixels), px[0]);
  // Statistics: this page's session_start, and the previous page's unclean exit, within seconds.
  const ev = await waitFor(async () => { const e = statsEvents(since); return e.some((x) => x.e === 'session_start') ? e : null; }, 20000);
  ok(`${name}: session_start reaches the collector within 20 s`, !!ev);
  // A page reopened seconds after the crash finds a recent heartbeat: it reports the crash once
  // that heartbeat is 15 s old.
  if (name !== 'close' && !auto) ok(`${name}: unclean_exit reaches the collector within 40 s`, !!(await waitFor(async () => statsEvents(since).some((x) => x.e === 'unclean_exit'), 40000)));
  return s;
}

for (const mode of modes) {
  console.log(`--- ${mode} (deviceMemory ${gb})`);
  fs.rmSync(profile, { recursive: true, force: true });
  const { s, cdp, W, H, want, editedAt } = await edit();
  // The checkpoint must be written well within the loss window.
  await sleep(Math.max(0, lossS * 1000 - (Date.now() - editedAt)));
  if (mode === 'crash' || mode === 'twice') await Promise.race([cdp.send('Page.crash').catch(() => {}), sleep(3000)]);
  else if (mode === 'kill') for (const pid of renderers()) { try { process.kill(pid, 'SIGKILL'); } catch (e) { /* gone */ } }
  else if (mode === 'close') await Promise.race([s.p.close({ runBeforeUnload: false }), sleep(10000)]);
  else if (mode === 'devicelost') {
    const r = await s.req('ui.gpu.simulateLoss', { destroy: true });
    console.log('simulateLoss', JSON.stringify(r).slice(0, 200));
    // No device to destroy (WebGL): lose the window's WebGL context instead, as a driver reset does.
    if (!(r && (r.destroyed || r.result?.destroyed))) {
      const lost = await s.p.evaluate(() => {
        for (const c of document.querySelectorAll('canvas')) {
          const gl = c.getContext('webgl2') || c.getContext('webgl');
          const ext = gl && gl.getExtension('WEBGL_lose_context');
          if (ext) { ext.loseContext(); return true; }
        }
        return false;
      }).catch((e) => String(e));
      console.log('WEBGL_lose_context', lost);
    }
    // The page saves, then reloads itself on the CPU canvas and reopens the documents.
    const reloaded = await waitFor(async () => s.navigations > 1, 60000);
    ok('devicelost: the page reloads', !!reloaded);
    await booted(s);
    const back = await waitFor(async () => { const l = await layers(s); return l.length >= want.layers.length ? l : null; }, 120000, 1000);
    ok('devicelost: the documents reopen by themselves', JSON.stringify(back) === JSON.stringify(want.layers), `${JSON.stringify(back)} vs ${JSON.stringify(want.layers)}`);
    await idle(s);
    const px = back ? await pixels(s, W, H) : [];
    ok('devicelost: the edited pixels come back', JSON.stringify(px) === JSON.stringify(want.pixels), px[0]);
    await hardClose(s.ctx);
    continue;
  }
  await hardClose(s.ctx);
  if (mode === 'twice') {
    const b = await reopen('twice (first)', want, W, H, { crashWhileLoading: true });
    await Promise.race([(await b.ctx.newCDPSession(b.p)).send('Page.crash').catch(() => {}), sleep(3000)]);
    await hardClose(b.ctx);
  }
  const b = await reopen(mode, want, W, H);
  await hardClose(b.ctx);
}
server.close();
const failed = results.filter((r) => r.startsWith('FAIL')).length;
console.log(failed ? `SOME FAILED (${failed})` : 'ALL PASSED');
process.exit(failed ? 1 : 0);
