// A page opened (or reloaded) in a background tab boots without ever being shown: a hidden tab runs
// no ResizeObserver and no requestAnimationFrame, which eframe relied on for the first frame, so
// `window.photocraft` never appeared. Opens the app in a background tab, waits for
// `window.photocraft`, then runs commands through it, checking the tab stayed hidden throughout.
// Raw DevTools protocol, not Playwright: Playwright emulates focus, so its pages are never hidden.
// Either web build.
//
//   CHROME=<chromium> node hidden-tab-boot.mjs <url> [timeout s]
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const [url, timeoutArg = '240'] = process.argv.slice(2);
if (!url || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> node hidden-tab-boot.mjs <url> [timeout s]');
  process.exit(2);
}
const port = 9300 + Math.floor(Math.random() * 600);
const profile = mkdtempSync(path.join(tmpdir(), 'capishot-hidden-'));
const chrome = spawn(
  process.env.CHROME,
  ['--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, '--no-first-run', '--no-sandbox',
    '--use-angle=swiftshader', '--enable-unsafe-swiftshader', 'about:blank'],
  { stdio: 'ignore' },
);
const results = [];
const ok = (name, cond, extra = '') => { results.push(`${cond ? 'PASS' : 'FAIL'} ${name} ${extra}`); console.log(results.at(-1)); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const finish = (code) => {
  // Chrome writes its profile until it has exited.
  chrome.once('exit', () => {
    try { rmSync(profile, { recursive: true, force: true, maxRetries: 5 }); } catch { /* a temp dir left behind */ }
    process.exit(code);
  });
  chrome.kill();
};

let version;
for (let i = 0; i < 150 && !version; i++) {
  try { version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json(); } catch { await sleep(200); }
}
if (!version) { console.log('FAIL chrome did not start'); finish(1); }
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise((r, e) => { ws.onopen = r; ws.onerror = e; });
let next = 0;
const pending = new Map();
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
};
const send = (method, params = {}, sessionId) => new Promise((r) => {
  const id = ++next;
  pending.set(id, r);
  ws.send(JSON.stringify({ id, method, params, sessionId }));
});

// The app opens behind the foreground tab and is never activated.
const target = await send('Target.createTarget', { url, background: true });
const attached = await send('Target.attachToTarget', { targetId: target.result.targetId, flatten: true });
const session = attached.result.sessionId;
const evaluate = async (expression) => {
  const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, session);
  return r.result?.exceptionDetails ? { err: r.result.exceptionDetails.text } : r.result?.result?.value;
};

const t0 = Date.now();
let state;
let alwaysHidden = true;
while (Date.now() - t0 < Number(timeoutArg) * 1000) {
  state = await evaluate('({hidden: document.hidden, api: !!(self.photocraft && self.photocraft.request)})');
  if (state && state.hidden === false) alwaysHidden = false;
  if (state && state.api) break;
  await sleep(1000);
}
const bootS = ((Date.now() - t0) / 1000).toFixed(1);
ok('the page is a hidden tab', alwaysHidden, JSON.stringify(state));
ok('window.photocraft appears in the hidden tab', !!(state && state.api), `after ${bootS} s`);
if (!(state && state.api)) finish(1);

const req = (method, params = {}) =>
  evaluate(`self.photocraft.request(${JSON.stringify(method)}, ${JSON.stringify(params)}).then((r) => r, (e) => ({err: String(e)}))`);
let commands;
for (let i = 0; i < 120; i++) {
  commands = await req('engine.commands');
  if (commands && !commands.err) break;
  await sleep(500);
}
ok('engine commands answer while hidden', Array.isArray(commands) ? commands.length > 100 : !!(commands && !commands.err), JSON.stringify(commands).slice(0, 80));
const made = await req('engine.execute', { command: 'file.new', params: { width: 320, height: 200, background: 'white' } });
const inspect = await req('engine.execute', { command: 'document.inspect', params: {} });
ok('a document opens while hidden', !!(inspect && !inspect.err && JSON.stringify(inspect).includes('320')), JSON.stringify(made).slice(0, 80));
ok('still hidden at the end', (await evaluate('document.hidden')) === true);
finish(results.every((r) => r.startsWith('PASS')) ? 0 : 1);
