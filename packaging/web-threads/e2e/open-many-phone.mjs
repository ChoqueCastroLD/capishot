// Opening a set of big manga pages at once on a phone: the page must stay alive and responsive and
// every document must open, in name order. 360 x 780 touch, deviceMemory 4 or 2 (DEVICE_MEMORY),
// CPU 4x once the app is interactive. Fails on a crash, a document missing or out of order (what
// doesn't fit must be named in a notice), a task over the limit, or a main thread that doesn't
// answer within HANG_S. Prints the memory peak (renderer + GPU process RSS, sampled twice a second).
//
//   node open-many-phone.mjs --gen <out dir> [count=7] [WxH=5000x7000]
//     writes synthetic manga-like pages, cycling through .jpg, .png and .webp
//   CHROME=<chromium> [DEVICE_MEMORY=4] [BUILD=threads|st] [THREADS=n] [HOW=picker|api] [LIMIT=1000]
//     [HANG_S=20] [SLOW_KBPS=n] [CPU_FROM_START=1] [FREEZE_S=n] [NO_STAGE=1] [PROFILE=<dir>] [SEED_SITE=<older site dir>] [WEBGPU=1] [NOGPU=1] \
//     node open-many-phone.mjs <built site dir> <image> [image...]
//
// The site is served here with the COOP/COEP headers (`st/` holds the single-threaded build) under
// the host name capichan.com, mapped to 127.0.0.1, so the usage statistics are on and go to this
// server. HOW=picker goes through File > Open (as a drop, the queue on small devices); HOW=api is the
// embedding API's open, a host's own call, which isn't queued or ordered. SEED_SITE: the same profile first runs that older build (it writes preferences, a
// workspace and a recovery snapshot), which dies without closing; then the build under test loads
// them. The images are local files, never committed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { execSync } from 'node:child_process';

const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml' };
const argv = process.argv.slice(2);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

if (argv[0] === '--gen') {
  const [, out, countArg = '7', sizeArg = '5000x7000'] = argv;
  const [w, h] = sizeArg.split('x').map(Number);
  fs.mkdirSync(out, { recursive: true });
  const b = await chromium.launch({ executablePath: process.env.CHROME });
  const p = await b.newPage();
  for (let i = 0; i < Number(countArg); i++) {
    const [type, ext] = [['image/jpeg', 'jpg'], ['image/png', 'png'], ['image/webp', 'webp']][i % 3];
    const data = await p.evaluate(async ([w, h, type, seed]) => {
      let s = seed * 7919 + 13;
      const rnd = () => (s = (s * 1664525 + 1013904223) >>> 0) / 4294967296;
      const c = new OffscreenCanvas(w, h);
      const g = c.getContext('2d');
      g.fillStyle = '#fff';
      g.fillRect(0, 0, w, h);
      // Panels with screentone, hatching and speech bubbles: what a scanned page compresses like.
      for (let y = 100; y < h - 300; y += h / 4) {
        for (let x = 100; x < w - 300; x += w / 2) {
          const [pw, ph] = [w / 2 - 150, h / 4 - 150];
          g.save();
          g.beginPath(); g.rect(x, y, pw, ph); g.clip();
          g.fillStyle = `rgb(${200 + rnd() * 40},${200 + rnd() * 40},${200 + rnd() * 40})`;
          g.fillRect(x, y, pw, ph);
          g.fillStyle = '#333';
          for (let k = 0; k < 9000; k++) { g.beginPath(); g.arc(x + rnd() * pw, y + rnd() * ph, 2 + rnd() * 3, 0, 7); g.fill(); }
          g.strokeStyle = '#111';
          for (let k = 0; k < 260; k++) { g.lineWidth = 1 + rnd() * 5; g.beginPath(); g.moveTo(x + rnd() * pw, y + rnd() * ph); g.lineTo(x + rnd() * pw, y + rnd() * ph); g.stroke(); }
          g.fillStyle = '#fff';
          g.beginPath(); g.ellipse(x + pw * 0.7, y + ph * 0.3, pw * 0.2, ph * 0.2, 0, 0, 7); g.fill(); g.stroke();
          g.fillStyle = '#000';
          g.font = `${Math.round(h / 90)}px sans-serif`;
          for (let k = 0; k < 4; k++) g.fillText('LOREM IPSUM', x + pw * 0.56, y + ph * 0.24 + k * h / 80);
          g.restore();
          g.lineWidth = 12; g.strokeStyle = '#000'; g.strokeRect(x, y, pw, ph);
        }
      }
      const blob = await c.convertToBlob({ type, quality: 0.9 });
      const bytes = new Uint8Array(await blob.arrayBuffer());
      let text = '';
      for (let k = 0; k < bytes.length; k += 32768) text += String.fromCharCode(...bytes.subarray(k, k + 32768));
      return btoa(text);
    }, [w, h, type, i + 1]);
    const file = path.join(out, `page_${i + 1}.${ext}`);
    fs.writeFileSync(file, Buffer.from(data, 'base64'));
    console.log(file, fs.statSync(file).size);
  }
  await b.close();
  process.exit(0);
}

const [site, ...images] = argv;
if (!site || !images.length) {
  console.log('usage: node open-many-phone.mjs <site dir> <image> [image...] | --gen <out dir> [count] [WxH]');
  process.exit(2);
}
const gb = Number(process.env.DEVICE_MEMORY || '4');
const st = process.env.BUILD === 'st';
const how = process.env.HOW || 'picker';
const limit = Number(process.env.LIMIT || '1000');
const hangS = Number(process.env.HANG_S || '20');
const profile = process.env.PROFILE || fs.mkdtempSync(path.join(os.tmpdir(), 'open-many-'));

// SLOW_KBPS: the whole site at a phone network's speed, so the module's chunks are still arriving
// when the file opens (localhost has them all before the first click).
const slow = Number(process.env.SLOW_KBPS || '0') * 1024;
let credit = 0;
if (slow) setInterval(() => { credit = Math.min(credit + slow / 20, slow); }, 50).unref();
async function send(stream, res) {
  if (!slow) return stream.pipe(res);
  stream.on('error', () => res.destroy());
  for await (const chunk of stream) {
    for (let at = 0; at < chunk.length;) {
      while (credit <= 0) await sleep(25);
      const n = Math.min(credit, chunk.length - at, 16384);
      credit -= n;
      if (!res.write(chunk.subarray(at, at + n))) await new Promise((r) => res.once('drain', r));
      at += n;
    }
  }
  res.end();
}
const beacons = [];
let root = site;
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const headers = { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp', 'Cross-Origin-Resource-Policy': 'same-origin', 'Cache-Control': 'no-store' };
  if (req.method === 'POST' && url.pathname === '/capishot/api/t') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      try { beacons.push(...(JSON.parse(body).events || [])); } catch (e) { /* not ours */ }
      res.writeHead(204, headers).end();
    });
    return;
  }
  const file = path.join(root, decodeURIComponent(url.pathname).replace(/\/$/, '/index.html'));
  if (!path.resolve(file).startsWith(path.resolve(root))) return res.writeHead(403).end();
  fs.stat(file, (err, s) => {
    if (err || !s.isFile()) return res.writeHead(404, headers).end();
    // As the host does: versioned files are cached for good, the rest revalidates.
    const cache = /\/v\/|-[0-9a-f]{16}/.test(url.pathname) ? { 'Cache-Control': 'public, max-age=31536000, immutable' } : {};
    res.writeHead(200, { ...headers, ...cache, 'Content-Type': types[path.extname(file)] || 'application/octet-stream', 'Content-Length': s.size });
    send(fs.createReadStream(file), res);
  });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const origin = `http://capichan.com:${server.address().port}`;
const pageUrl = (extra = '') => {
  const q = [process.env.THREADS ? `threads=${process.env.THREADS}` : '', process.env.WEBGPU || process.env.NOGPU ? '' : 'gpu=1', extra].filter(Boolean).join('&');
  return `${origin}/${st ? 'st/' : ''}${q ? `?${q}` : ''}`;
};

const launch = async () => {
  const ctx = await chromium.launchPersistentContext(profile, {
    executablePath: process.env.CHROME,
    args: [
      ...(process.env.WEBGPU ? ['--enable-unsafe-webgpu', '--use-webgpu-adapter=swiftshader', '--enable-features=Vulkan'] : []),
      '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--hide-crash-restore-bubble', '--no-first-run',
      '--host-resolver-rules=MAP capichan.com 127.0.0.1', `--unsafely-treat-insecure-origin-as-secure=${origin}`,
    ],
    viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES',
    userAgent: 'Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36',
  });
  await ctx.addInitScript((gb) => {
    Object.defineProperty(navigator, 'deviceMemory', { get: () => gb });
    self.__long = [];
    try { new PerformanceObserver((l) => { for (const e of l.getEntries()) self.__long.push({ at: Math.round(e.startTime), ms: Math.round(e.duration) }); }).observe({ type: 'longtask', buffered: true }); } catch (e) { /* no entries */ }
  }, gb);
  const p = await ctx.newPage();
  for (const other of ctx.pages()) if (other !== p) await other.close().catch(() => {});
  const s = { ctx, p, crashed: false, log: [] };
  p.on('crash', () => { s.crashed = true; console.log('PAGE CRASHED'); });
  p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 400)));
  p.on('console', (m) => { const t = m.text(); s.log.push(t); if (process.env.ALLCONSOLE || m.type() === 'error' || /panic|lost|out of memory|didn't close|recover/i.test(t)) console.log('console:', m.type(), t.slice(0, 400)); });
  s.req = (m, pr = {}) => Promise.race([
    p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]).catch((e) => ({ err: String(e) })),
    sleep(hangS * 1000).then(() => ({ err: `no answer in ${hangS} s: ${m}`, hang: true })),
  ]);
  return s;
};
const boot = async (s) => {
  // CPU_FROM_START: a phone loads the module slowly too.
  if (process.env.CPU_FROM_START) { s.cdp = await s.ctx.newCDPSession(s.p); await s.cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 }); }
  await s.p.goto(pageUrl(), { waitUntil: 'load' });
  for (let i = 0; i < 600; i++) {
    const ready = await s.p.evaluate(() => !!(self.photocraft && self.photocraft.request)).catch(() => false);
    if (ready && !(await s.req('engine.commands')).err) return true;
    await sleep(500);
  }
  return false;
};
const killProfile = () => {
  for (const pid of fs.readdirSync('/proc').filter((d) => /^\d+$/.test(d))) {
    try { if (fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').includes('--user-data-dir=' + profile)) process.kill(+pid, 'SIGKILL'); } catch (e) { /* gone */ }
  }
};
const hardClose = async (s) => {
  killProfile();
  await Promise.race([s.ctx.close().catch(() => {}), sleep(3000)]);
  await sleep(1000);
  for (const f of ['SingletonLock', 'SingletonCookie', 'SingletonSocket']) fs.rmSync(`${profile}/${f}`, { force: true });
};
const ex = async (s, command, params = {}) => { const r = await s.req('engine.execute', { command, params }); return (r && r.result) || r || {}; };

const results = [];
const ok = (name, cond, extra = '') => { results.push(`${cond ? 'PASS' : 'FAIL'} ${name} ${extra}`); console.log(results.at(-1)); };

// An older build's saved state: preferences changed, a custom workspace, a document edited, then
// the tab dies, so the build under test finds all of it.
if (process.env.SEED_SITE) {
  root = process.env.SEED_SITE;
  const s = await launch();
  ok('seed: the older build boots', await boot(s));
  const file = images[0];
  await s.p.route('**/__e2e_seed', (r) => r.fulfill({ body: fs.readFileSync(file) }));
  await s.p.evaluate(async (n) => { const r = await fetch('/__e2e_seed'); self.photocraft.open(await r.blob(), n); }, path.basename(file));
  for (let i = 0; i < 240; i++) { if (((await ex(s, 'document.inspect')).name || '') === path.basename(file)) break; await sleep(250); }
  console.log('seed edit', JSON.stringify(await ex(s, 'type.create', { x: 40, y: 40, text: 'SEED', size: 48 })).slice(0, 120));
  await ex(s, 'prefs.set', { values: { 'interface.showToolTips': false } });
  await sleep(Number(process.env.SEED_WAIT_S || '40') * 1000); // the autosave's first checkpoint
  await hardClose(s);
  root = site;
}

const s = await launch();
const { p } = s;
const t0 = Date.now();
ok('boots', await boot(s));
console.log('boot ms', Date.now() - t0, 'threads', await p.evaluate(() => self.photocraftThreads));
const cdp = await s.ctx.newCDPSession(p);
await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
// The recovery prompt of a seeded profile: recover what's there, as a person would.
if (process.env.SEED_SITE) {
  await sleep(4000);
  const u = await s.req('ui.inspect');
  const d = ((u && (u.result || u).dialogs) || []).find((x) => x.id === 'recovery');
  console.log('recovery dialog after the older build:', d ? JSON.stringify(d).slice(0, 300) : 'none');
  if (d) {
    if (process.env.SHOT) await p.screenshot({ path: `${process.env.SHOT}.recovery.png` }).catch(() => {});
    // As a person: the button's rectangle, in the page's coordinates.
    const r = d.fields.buttons[process.env.SEED_ANSWER || 'recover'];
    await p.mouse.move((r[0] + r[2]) / 2, (r[1] + r[3]) / 2); await p.mouse.down(); await sleep(60); await p.mouse.up();
    await sleep(6000);
    const after = await s.req('ui.inspect');
    console.log('recovered documents:', JSON.stringify(((after.result || after).session?.documents || []).map((x) => x.name)));
  }
}

const procMB = () => {
  try {
    const rows = execSync('ps -eo pid=,ppid=,rss=,args=', { encoding: 'utf8' }).split('\n').map((l) => l.trim().split(/\s+/)).filter((r) => r.length > 3);
    const kids = new Set([String(process.pid)]);
    for (let grew = true; grew;) { grew = false; for (const r of rows) if (kids.has(r[1]) && !kids.has(r[0])) { kids.add(r[0]); grew = true; } }
    const sum = (t) => Math.round(rows.filter((r) => kids.has(r[0]) && r.slice(3).join(' ').includes(t)).reduce((a, r) => a + Number(r[2]), 0) / 1024);
    return sum('--type=renderer') + sum('--type=gpu-process');
  } catch (e) { return 0; }
};
let peak = 0;
const sampler = setInterval(() => { peak = Math.max(peak, procMB()); }, 500);
const base = procMB();
const wasmMB = async () => { const m = await s.req('perf.memory'); return (m && (m.result || m).wasmMB) || 0; };

await p.evaluate(() => { self.__long.length = 0; });
const names = images.map((f) => path.basename(f));
const started = Date.now();
if (how === 'picker') {
  const chooser = p.waitForEvent('filechooser', { timeout: 30000 });
  // Through the app's File > Open, as a person: the browser asks for the files.
  const asked = s.req('ui.menu.invoke', { id: 'file.open' });
  const fc = await chooser;
  // A page killed behind the picker must say so in its breadcrumb (`stage`: `picker`).
  const crumb = await p.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith('capishot.crumb.')).map((k) => JSON.parse(localStorage.getItem(k)).stage));
  console.log('breadcrumb behind the picker:', JSON.stringify(crumb));
  if (!process.env.NO_STAGE) ok('the breadcrumb names the picker', crumb.includes('picker'));
  // FREEZE_S: Android puts the page behind the picker (hidden, then frozen) until a file is chosen.
  if (process.env.FREEZE_S) {
    await cdp.send('Page.enable');
    await cdp.send('Page.setWebLifecycleState', { state: 'frozen' });
    await sleep(Number(process.env.FREEZE_S) * 1000);
    await cdp.send('Page.setWebLifecycleState', { state: 'active' });
  }
  await fc.setFiles(images);
  await asked;
} else {
  for (const [i, f] of images.entries()) await p.route(`**/__e2e_pages/${i}`, (r) => r.fulfill({ body: fs.readFileSync(f) }));
  await p.evaluate(async (names) => {
    const files = await Promise.all(names.map(async (n, i) => [await (await fetch(`/__e2e_pages/${i}`)).blob(), n]));
    for (const [b, n] of files) self.photocraft.open(b, n);
  }, names);
}
// Answers from the main thread and the documents' arrival.
const arrivals = [];
let hang = 0;
let open = [];
let refused = null;
let shot = false;
const deadline = Date.now() + Number(process.env.OPEN_S || '600') * 1000;
const doneWhen = names.length;
while (Date.now() < deadline && !s.crashed) {
  const r = await s.req('ui.inspect');
  if (r && r.hang) { hang++; console.log(`main thread: no answer in ${hangS} s (at ${Math.round((Date.now() - started) / 1000)} s)`); if (hang >= 3) break; continue; }
  const u = (r && (r.result || r)) || {};
  open = ((u.session && u.session.documents) || []).map((d) => (typeof d === 'string' ? d : d.name || d.title));
  if (open.length > arrivals.length) { arrivals.push(...open.slice(arrivals.length).map((n) => ({ n, s: Math.round((Date.now() - started) / 1000) }))); console.log('open', Math.round((Date.now() - started) / 1000), 's:', open.join(', '), 'wasm', await wasmMB(), 'rss', procMB()); }
  const jobs = await s.req('jobs.list');
  const running = (((jobs && (jobs.result || jobs).jobs) || [])).filter((j) => j.state === 'running');
  const waiting = ((u.jobs && u.jobs.queued) || []).length + ((u.jobs && u.jobs.opening) || []).length;
  // All in, or the rest refused with a notice.
  refused = (u.notices || []).find((n) => /\d+ (de|of) \d+/.test(n.title || '') && n.error);
  if (refused && process.env.SHOT && !shot) { shot = true; await p.screenshot({ path: `${process.env.SHOT}.notice.png` }).catch(() => {}); }
  if ((open.length >= doneWhen || refused) && !running.length && !waiting) break;
  await sleep(500);
}
const openMs = Date.now() - started;
const split = await Promise.race([p.evaluate(() => { const v = self.capishotSplit && self.capishotSplit.state; return v && { loaded: Object.keys(v.loaded).length, chunks: Object.keys(v.chunks).length, calls: v.calls, syncLoads: v.syncLoads }; }).catch(() => null), sleep(5000)]);
console.log('split', JSON.stringify(split));
await sleep(6000); // whatever follows the landing (prewarm, thumbnails, the first checkpoint)
const long = await p.evaluate(() => self.__long).catch(() => []);
const worst = long.reduce((m, l) => Math.max(m, l.ms), 0);
clearInterval(sampler);
const wasm = await wasmMB();
const events = beacons.filter((e) => ['unclean_exit', 'gpu_lost', 'doc_open'].includes(e.e));
console.log(JSON.stringify({ gb, build: st ? 'st' : 'threads', how, images: names.length, openMs, base, peakMB: peak, wasm, worstTask: worst, longOver: long.filter((l) => l.ms > limit).map((l) => l.ms), events: events.slice(0, 12).map((e) => `${e.e} ${JSON.stringify(e.data || e).slice(0, 110)}`) }));
const want = [...names].sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
ok('no crash', !s.crashed);
ok('the main thread answers', hang === 0, `(${hang} silences of ${hangS} s)`);
// What doesn't fit stays closed with a notice naming it: the rest are the first in order.
const lefts = refused ? want.filter((n) => (refused.lines || []).includes(n)) : [];
const shouldOpen = want.filter((n) => !lefts.includes(n));
ok('every document open, in order', JSON.stringify(how === 'api' ? open.filter((n) => want.includes(n)).sort((a, b) => a.localeCompare(b, undefined, { numeric: true })) : open.filter((n) => want.includes(n))) === JSON.stringify(shouldOpen), JSON.stringify(open));
ok('nothing silently missing', shouldOpen.length + lefts.length === want.length && (!lefts.length || JSON.stringify(shouldOpen) === JSON.stringify(want.slice(0, shouldOpen.length))), refused ? `refused ${JSON.stringify(lefts)}: ${refused.title}` : '');
ok(`no task over ${limit} ms`, worst <= limit, `(longest ${worst} ms)`);
if (results.some((r) => r.startsWith('FAIL'))) process.exitCode = 1;
if (process.env.SHOT) await p.screenshot({ path: process.env.SHOT }).catch(() => {});
await hardClose(s);
server.close();
process.exit(process.exitCode || 0);
