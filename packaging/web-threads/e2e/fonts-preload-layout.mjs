// Visoria D.19 / D.20 in a real browser.
//   D.19: the web build lays type out bit for bit as native builds do: `type.layoutDigest` of a
//         fixed scene (the bundled fonts) equals the golden digest the native engine test pins
//         (`crates/engine/src/type_layout_cmds_tests.rs`, read from there).
//   D.20: `fonts.status` says where families stand without fetching; `fonts.preload {id}` sends
//         one `fontsPreloadProgress` per family as it settles (window event with
//         `window.photocraft`, embed `onEvent` with the module), and a family already loaded
//         isn't fetched again.
// Works on either web build's dist directory.
//
//   CHROME=<chromium> node fonts-preload-layout.mjs <dist dir> [port = 8797]
import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const [distArg, portArg = '8797'] = process.argv.slice(2);
if (!distArg) {
  console.error('usage: node fonts-preload-layout.mjs <dist dir> [port]');
  process.exit(2);
}
const dist = path.resolve(distArg);
const port = Number(portArg);
const golden = /const GOLDEN: &str = "([0-9a-f]{64})";/.exec(fs.readFileSync(path.join(repo, 'crates/engine/src/type_layout_cmds_tests.rs'), 'utf8'))?.[1];
if (!golden) {
  console.error('no GOLDEN digest in type_layout_cmds_tests.rs');
  process.exit(2);
}
// The scene of `type_layout_cmds_tests.rs` › `scene()`, call for call.
const scene = [
  { x: 10, y: 60, text: 'AVATAR wave, fi ffi\nSegunda línea — ¿sí?', size: 24, font: 'Inter' },
  { box: [20, 90, 180, 200], text: '¡Qué haces aquí! Esto es un globo de diálogo bastante largo, que debe partirse en varias líneas.', size: 15, font: 'Inter', align: 'justify', kerning: 'optical', fauxItalic: true, tracking: 25 },
  { box: [220, 20, 160, 260], text: 'Monospace 0123 // wrap wrap wrap wrap', size: 13, font: 'JetBrains Mono', align: 'center', horizontalScale: 90 },
];

// The site: the build, plus two served fonts and one whose file is not there.
const site = fs.mkdtempSync(path.join(os.tmpdir(), 'fonts-preload-'));
fs.cpSync(dist, site, { recursive: true });
const rename = (bytes, from, to) => {
  const enc = (s) => Buffer.from(s, 'utf16le').swap16();
  const a = enc(from);
  const b = enc(to);
  const out = Buffer.from(bytes);
  for (let i = out.indexOf(a); i >= 0; i = out.indexOf(a, i + a.length)) b.copy(out, i);
  return out;
};
fs.mkdirSync(path.join(site, 'fonts/lib'), { recursive: true });
const jetbrains = fs.readFileSync(path.join(repo, 'assets/fonts/JetBrainsMono-Regular.ttf'));
fs.writeFileSync(path.join(site, 'fonts/lib/Served_Mono_A1.ttf'), rename(jetbrains, 'JetBrains Mono', 'Served Mono A1'));
fs.writeFileSync(path.join(site, 'fonts/lib/Served_Mono_B2.ttf'), rename(jetbrains, 'JetBrains Mono', 'Served Mono B2'));
fs.writeFileSync(
  path.join(site, 'fonts/manifest.txt'),
  '# family | style | file | scripts | licence | licence file | sha256 | source\n' +
    'Served Mono A1 | Regular | fonts/lib/Served_Mono_A1.ttf | Latn | - | - | - | test\n' +
    'Served Mono B2 | Regular | fonts/lib/Served_Mono_B2.ttf | Latn | - | - | - | test\n' +
    'Served Gone C3 | Regular | fonts/lib/Served_Gone_C3.ttf | Latn | - | - | - | test\n',
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
const J = JSON.stringify;

async function app(page = '') {
  const ctx = await b.newContext({ viewport: { width: 1280, height: 860 }, locale: 'en-US' });
  const p = await ctx.newPage();
  const fetched = [];
  p.on('request', (r) => {
    if (r.url().includes('/fonts/lib/')) fetched.push(decodeURIComponent(r.url().split('/fonts/lib/')[1]));
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

try {
  // 1. window.photocraft: layout digest, then fonts.status / fonts.preload with progress.
  const a = await app();
  const req = await ready(a.p);
  const ex = (c, pr = {}) => req('engine.execute', { command: c, params: pr });
  await ex('file.new', { width: 400, height: 300, background: 'transparent' });
  for (const s of scene) await ex('type.create', s);
  const digest = await ex('type.layoutDigest', {});
  check(digest.layers && digest.layers.length === 3, `three type layers: ${J(digest).slice(0, 300)}`);
  check(digest.digest === golden, `layout digest equals the native golden: ${digest.digest} vs ${golden}`);
  const one = await ex('type.layoutDigest', { layer: digest.layers?.[0]?.layer, fontHashes: true });
  check(one.fonts && one.fonts.length >= 1 && /^[0-9a-f]{64}$/.test(one.fonts[0].sha256), `fonts with hashes: ${J(one.fonts)}`);

  await a.p.evaluate(() => {
    self.progress = [];
    addEventListener('photocraft-fontsPreloadProgress', (e) => self.progress.push(e.detail));
  });
  for (let i = 0; i < 40 && (await req('fonts.status', {})).manifest !== 'loaded'; i++) await a.p.waitForTimeout(250);
  const st0 = await req('fonts.status', { families: ['Served Mono A1', 'Served Mono B2', 'Served Gone C3', 'Nowhere Font Z', 'Inter'] });
  check(J(st0.available) === J(['Served Mono A1', 'Served Mono B2', 'Served Gone C3']) && J(st0.loaded) === J(['Inter']) && J(st0.missing) === J(['Nowhere Font Z']), `fonts.status before: ${J(st0)}`);
  const all = await req('fonts.status', {});
  check(['Served Mono A1', 'Served Mono B2', 'Served Gone C3'].every((f) => all.available.includes(f)) && all.manifest === 'loaded', `fonts.status without families: ${J(all)}`);
  check(a.fetched.length === 0, `status fetched nothing: ${J(a.fetched)}`);
  const pre = await req('fonts.preload', { families: ['Served Mono A1', 'Served Gone C3', 'Nowhere Font Z', 'Inter'], id: 'work-12', timeoutMs: 20000 });
  check(J(pre.ready) === J(['Served Mono A1', 'Inter']) && J(pre.missing) === J(['Nowhere Font Z']) && J(pre.failed) === J(['Served Gone C3']), `fonts.preload: ${J(pre)}`);
  await a.p.waitForTimeout(500);
  const prog = await a.p.evaluate(() => self.progress);
  check(prog.length === 4 && prog.every((e) => e.id === 'work-12' && e.total === 4), `four progress events: ${J(prog)}`);
  check(J(prog.map((e) => e.done)) === J([1, 2, 3, 4]), 'done counts up to total');
  const byFamily = Object.fromEntries(prog.map((e) => [e.family, e.status]));
  const want = { 'Served Mono A1': 'loaded', 'Served Gone C3': 'failed', 'Nowhere Font Z': 'missing', Inter: 'loaded' };
  check(Object.keys(want).every((f) => byFamily[f] === want[f]), `statuses: ${J(byFamily)}`);
  const st1 = await req('fonts.status', { families: ['Served Mono A1', 'Served Mono B2', 'Served Gone C3'] });
  check(J(st1.loaded) === J(['Served Mono A1']) && J(st1.available) === J(['Served Mono B2']) && J(st1.failed) === J(['Served Gone C3']), `fonts.status after: ${J(st1)}`);
  const fetchedA1 = a.fetched.filter((f) => f === 'Served_Mono_A1.ttf').length;
  await req('fonts.preload', { families: ['Served Mono A1'], id: 'again' });
  await a.p.waitForTimeout(500);
  check(a.fetched.filter((f) => f === 'Served_Mono_A1.ttf').length === fetchedA1 && fetchedA1 === 1, `a loaded family isn't fetched again: ${J(a.fetched)}`);
  const again = await a.p.evaluate(() => self.progress.filter((e) => e.id === 'again'));
  check(again.length === 1 && again[0].family === 'Served Mono A1' && again[0].status === 'loaded' && again[0].done === 1 && again[0].total === 1, `already loaded: one event at once: ${J(again)}`);
  await a.ctx.close();

  // 2. The embed module: the same digest, and progress through onEvent.
  const e = await app('host.html');
  await e.p.waitForFunction(() => self.capishot, null, { timeout: 300000 });
  const embedDigest = await e.p.evaluate(async (scene) => {
    await self.capishot.execute('file.new', { width: 400, height: 300, background: 'transparent' });
    for (const s of scene) await self.capishot.execute('type.create', s);
    return (await self.capishot.execute('type.layoutDigest', {})).digest;
  }, scene);
  check(embedDigest === golden, `embed layout digest equals the native golden: ${embedDigest}`);
  const features = await e.p.evaluate(() => self.capishot.version().features);
  check(['type.layoutDigest', 'fonts.status', 'events.fontsPreloadProgress'].every((f) => features.includes(f)), 'features listed');
  for (let i = 0; i < 40 && (await e.p.evaluate(() => self.capishot.request('fonts.status', {}))).manifest !== 'loaded'; i++) await e.p.waitForTimeout(250);
  const ePre = await e.p.evaluate(() => self.capishot.request('fonts.preload', { families: ['Served Mono B2'], id: 'embed-1' }));
  check(J(ePre.ready) === J(['Served Mono B2']), `embed fonts.preload: ${J(ePre)}`);
  await e.p.waitForTimeout(500);
  const eProg = await e.p.evaluate(() => self.events.filter((x) => x.type === 'fontsPreloadProgress'));
  const e0 = eProg[0] || {};
  check(eProg.length === 1 && e0.id === 'embed-1' && e0.family === 'Served Mono B2' && e0.status === 'loaded' && e0.done === 1 && e0.total === 1, `embed fontsPreloadProgress: ${J(eProg)}`);
  await e.ctx.close();
} finally {
  await b.close();
  server.kill();
  fs.rmSync(site, { recursive: true, force: true });
}
console.log(failed ? `${failed} check(s) failed` : 'type lays out as natively; font preload reports progress and status');
process.exit(failed ? 1 : 0);
