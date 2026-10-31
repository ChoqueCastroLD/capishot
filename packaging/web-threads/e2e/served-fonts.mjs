// Served fonts are never "missing": a site with `fonts/manifest.txt` (paths with spaces, accents
// and "&", as a real font library's folders have them) and a font that is no Capishot font
// (JetBrains Mono renamed to "Served Mono AB", listed in the manifest only as "Served Mono (AB)").
// A PSD whose type uses that font and one that exists nowhere ("Nowhere Font Z") opens in a fresh
// browser (empty cache):
//   - the "Missing fonts" notice never lists the served font and ends listing only the absent one;
//   - `photocraft-fontsMissing` reports only the absent one;
//   - the served file was fetched (its path URL-encoded) and `fonts.preload` says it is ready;
//   - `layer.copyToDocument` of the type layer into another document raises no notice;
//   - the embed module (`init({fontsNotice: false})`) shows no notice and sends `fontsMissing`.
// Works on either web build's dist directory.
//
//   CHROME=<chromium> node served-fonts.mjs <dist dir> [port = 8791]
import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const [distArg, portArg = '8791'] = process.argv.slice(2);
if (!distArg) {
  console.error('usage: node served-fonts.mjs <dist dir> [port]');
  process.exit(2);
}
const dist = path.resolve(distArg);
const port = Number(portArg);

// The site: the build, plus a font library.
const site = fs.mkdtempSync(path.join(os.tmpdir(), 'served-fonts-'));
fs.cpSync(dist, site, { recursive: true });
const rename = (bytes, from, to) => {
  const enc = (s) => Buffer.from(s, 'utf16le').swap16();
  const a = enc(from);
  const b = enc(to);
  const out = Buffer.from(bytes);
  for (let i = out.indexOf(a); i >= 0; i = out.indexOf(a, i + a.length)) b.copy(out, i);
  return out;
};
const folder = 'Fuentes Para Manga & Comic/1. Diálogos';
fs.mkdirSync(path.join(site, 'fonts', folder), { recursive: true });
const jetbrains = fs.readFileSync(path.join(repo, 'assets/fonts/JetBrainsMono-Regular.ttf'));
fs.writeFileSync(path.join(site, 'fonts', folder, 'Served_Mono_AB.ttf'), rename(jetbrains, 'JetBrains Mono', 'Served Mono AB'));
fs.writeFileSync(
  path.join(site, 'fonts/manifest.txt'),
  `# family | style | file | scripts | licence | licence file | sha256 | source\nServed Mono (AB) | Regular | fonts/${folder}/Served_Mono_AB.ttf | Latn | - | - | - | test\n`,
);
fs.writeFileSync(
  path.join(site, 'host.html'),
  `<!doctype html><meta charset="utf-8"><body style="margin:0"><div id="box" style="width:900px;height:640px"></div><script type="module">
const glue = ${JSON.stringify(fs.readdirSync(dist).find((f) => /^photocraft-web-.*\.js$/.test(f)))};
const wasm = ${JSON.stringify(fs.readdirSync(dist).find((f) => /^photocraft-web-.*_bg\.wasm$/.test(f)))};
const capishot = await import('./' + glue);
await capishot.default({ module_or_path: './' + wasm });
self.events = [];
capishot.onEvent((e) => self.events.push(e));
await capishot.init({ container: document.getElementById('box'), fontsNotice: false, locale: 'en' });
self.capishot = capishot;
</script>`,
);

const server = spawn('python3', [path.join(here, '../serve.py'), site, String(port)], { stdio: 'ignore' });
const url = `http://127.0.0.1:${port}/`;
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};

async function app(page = '') {
  const ctx = await b.newContext({ viewport: { width: 1280, height: 860 }, locale: 'en-US' });
  const p = await ctx.newPage();
  const fetched = [];
  p.on('response', (r) => {
    if (r.url().includes('/fonts/') && !r.url().endsWith('manifest.txt')) fetched.push([r.status(), r.url()]);
  });
  p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
  await p.goto(url + page);
  return { ctx, p, fetched };
}

async function ready(p) {
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  for (let i = 0; i < 400; i++) {
    if (!(await req('engine.commands')).err) break;
    await p.waitForTimeout(250);
  }
  return req;
}

const notices = async (req) => {
  const i = await req('ui.inspect');
  return ((i.result || i).notices || []).filter((n) => n.title === 'Missing fonts').map((n) => n.lines);
};

try {
  // 1. A PSD whose type uses the served font (by its own name) and an absent one.
  const a = await app();
  const reqA = await ready(a.p);
  const ex = (c, pr = {}) => reqA('engine.execute', { command: c, params: pr });
  await ex('file.new', { width: 600, height: 400, background: 'white' });
  await ex('type.create', { text: 'Served text', x: 40, y: 120, size: 48, font: 'Served Mono AB' });
  await ex('type.create', { text: 'Absent text', x: 40, y: 260, size: 48, font: 'Nowhere Font Z' });
  const psd = await a.p.evaluate(() => self.photocraft.save('psd').then((u) => [...u]));
  await a.ctx.close();

  // 2. Opened in a fresh browser.
  const o = await app();
  const req = await ready(o.p);
  await o.p.evaluate(() => {
    self.missingEvents = [];
    addEventListener('photocraft-fontsMissing', (e) => self.missingEvents.push(e.detail.families));
  });
  await o.p.evaluate(([bytes]) => self.photocraft.open(new Uint8Array(bytes), 'page.psd'), [psd]);
  // Every notice seen while the fonts come (a notice hides itself after a few seconds).
  const seen = new Set();
  for (let t = 0; t < 8000; t += 250) {
    for (const lines of await notices(req)) seen.add(JSON.stringify(lines));
    await o.p.waitForTimeout(250);
  }
  check(![...seen].some((l) => l.includes('Served Mono')), 'the served font is never listed as missing');
  check(JSON.stringify([...seen]) === JSON.stringify([JSON.stringify(['Nowhere Font Z'])]), `the notice lists only the absent font: ${JSON.stringify([...seen])}`);
  const events = await o.p.evaluate(() => self.missingEvents);
  check(JSON.stringify(events) === JSON.stringify([['Nowhere Font Z']]), `photocraft-fontsMissing: ${JSON.stringify(events)}`);
  check(o.fetched.some(([s, u]) => s === 200 && u.includes('Fuentes%20Para%20Manga%20&%20Comic')), `the served file was fetched: ${JSON.stringify(o.fetched)}`);
  const pre = (r) => JSON.stringify([r.ready, r.missing, r.failed]);
  const preloaded = await req('fonts.preload', { families: ['Served Mono (AB)', 'served mono ab', 'Nowhere Font Z'], timeoutMs: 5000 });
  check(pre(preloaded.result || preloaded) === pre({ ready: ['Served Mono (AB)', 'served mono ab'], missing: ['Nowhere Font Z'], failed: [] }), `fonts.preload: ${JSON.stringify(preloaded.result || preloaded)}`);

  // 3. layer.copyToDocument of the served type layer into another document.
  const listed = await req('engine.execute', { command: 'layer.list', params: { flat: true } });
  const layers = (listed.result || listed).layers || [];
  const typeLayer = layers.find((l) => /Served/.test(l.name || ''));
  if (!typeLayer) console.log('layers', JSON.stringify(layers).slice(0, 400));
  await req('engine.execute', { command: 'file.new', params: { width: 400, height: 300 } });
  const before = (await notices(req)).length;
  const copied = await req('engine.execute', { command: 'layer.copyToDocument', params: { source: 0, document: 1, layers: [typeLayer && typeLayer.id] } });
  await o.p.waitForTimeout(2000);
  check(!copied.err && copied.ok !== false && typeLayer, `copyToDocument ran: ${JSON.stringify(copied).slice(0, 160)}`);
  check((await notices(req)).length === before, 'the copy raised no notice');
  await o.ctx.close();

  // 4. The embed module, notice off.
  const e = await app('host.html');
  await e.p.waitForFunction(() => self.capishot, null, { timeout: 300000 });
  await e.p.evaluate(([bytes]) => self.capishot.openDocument(new Uint8Array(bytes), 'page.psd'), [psd]);
  await e.p.waitForTimeout(6000);
  const embedEvents = await e.p.evaluate(() => self.events.filter((x) => x.type === 'fontsMissing').map((x) => x.families));
  check(JSON.stringify(embedEvents) === JSON.stringify([['Nowhere Font Z']]), `embed fontsMissing: ${JSON.stringify(embedEvents)}`);
  const embedNotices = await e.p.evaluate(() => self.capishot.request('ui.inspect', {}).then((r) => (r.notices || []).filter((n) => n.title === 'Missing fonts')));
  check(embedNotices.length === 0, 'embed: no notice with fontsNotice: false');
  const embedPre = await e.p.evaluate(() => self.capishot.request('fonts.preload', { families: ['Served Mono (AB)'] }));
  check(JSON.stringify(embedPre.ready) === JSON.stringify(['Served Mono (AB)']), `embed fonts.preload: ${JSON.stringify(embedPre)}`);
  await e.ctx.close();
} finally {
  await b.close();
  server.kill();
  fs.rmSync(site, { recursive: true, force: true });
}
console.log(failed ? `${failed} check(s) failed` : 'served fonts are never missing');
process.exit(failed ? 1 : 0);
