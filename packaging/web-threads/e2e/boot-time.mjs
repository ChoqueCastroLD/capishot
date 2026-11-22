// The page's start: how long until the app's first frame is drawn, and that the page's thread
// loads no part of the module synchronously on the way (a split profile that no longer matches the
// code makes the start fetch and compile chunks on the page's thread: packaging/web-split/profile.sh).
// Each run starts a fresh browser (cold: nothing cached) or reuses one that loaded the page once
// (warm), on a desktop window with the page's thread slowed down `CPU` times (default 4).
// Prints each run and the median, p99 and max; fails when any run loaded a chunk synchronously.
//
//   CHROME=<chromium> [CPU=4] [RUNS=5] [WARM=1] node boot-time.mjs <url>
import { chromium } from 'playwright-core';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const [url] = process.argv.slice(2);
if (!url || !process.env.CHROME) {
  console.error('usage: CHROME=<chromium> [CPU=4] [RUNS=5] [WARM=1] node boot-time.mjs <url>');
  process.exit(2);
}
const rate = Number(process.env.CPU || 4);
const runs = Number(process.env.RUNS || 5);
const warm = process.env.WARM === '1';
const profiles = [];
const launch = () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'capishot-boot-'));
  profiles.push(dir);
  return chromium.launchPersistentContext(dir, {
    executablePath: process.env.CHROME, headless: true, viewport: { width: 1366, height: 768 },
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--no-sandbox'],
  });
};
const started = (p) => p.waitForFunction(() => !document.getElementById('photocraft_loading'), null, { timeout: 300000, polling: 50 });

let ctx = await launch();
if (warm) {
  const p = await ctx.newPage();
  await p.goto(url);
  await started(p);
  await p.waitForTimeout(3000);
  await p.close();
}
const rows = [];
for (let i = 0; i < runs; i++) {
  if (!warm && i > 0) {
    await ctx.close();
    ctx = await launch();
  }
  const p = await ctx.newPage();
  const cdp = await ctx.newCDPSession(p);
  await cdp.send('Emulation.setCPUThrottlingRate', { rate });
  let line = '';
  p.on('console', (m) => { if (m.text().includes('startup:')) line = m.text(); });
  await p.goto(url);
  await started(p);
  // The startup line is logged once the first frame is drawn.
  for (let t = 0; t < 100 && !line; t++) await p.waitForTimeout(100);
  const split = await p.evaluate(() => {
    const s = self.capishotSplit && self.capishotSplit.state;
    return s ? { n: s.syncLoads, ms: Math.round(s.syncMs || 0) } : { n: 0, ms: 0 };
  });
  const at = (step) => Number((line.match(new RegExp(`${step} (\\d+) ms`)) || [])[1]);
  const row = { firstFrame: at('first frame drawn'), wasm: Number((line.match(/wasm running at (\d+) ms/) || [])[1]), syncLoads: split.n, syncMs: split.ms };
  rows.push(row);
  console.log(`run ${i + 1}: ${JSON.stringify(row)}`);
  await p.close();
}
await ctx.close();
for (const dir of profiles) {
  try { rmSync(dir, { recursive: true, force: true, maxRetries: 5 }); } catch { /* a temp dir left behind */ }
}
const stats = (k) => {
  const v = rows.map((r) => r[k]).filter(Number.isFinite).sort((a, b) => a - b);
  const pick = (q) => v[Math.min(v.length - 1, Math.max(0, Math.ceil(q * v.length) - 1))];
  return `median ${pick(0.5)} p99 ${pick(0.99)} max ${v.at(-1)}`;
};
console.log(`first frame drawn (ms since navigation, CPU ${rate}x, ${warm ? 'warm' : 'cold'}): ${stats('firstFrame')}`);
console.log(`module running: ${stats('wasm')}`);
const sync = rows.reduce((n, r) => n + r.syncLoads, 0);
console.log(`${sync === 0 ? 'PASS' : 'FAIL'} no synchronous chunk load on the page while starting (${sync} in ${runs} runs)`);
process.exit(sync === 0 ? 0 : 1);
