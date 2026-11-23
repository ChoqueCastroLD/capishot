// A tab left open while a new build is published (boot.js `watchBuild`), in Spanish:
//
// 1. While the page says the same build, nothing shows.
// 2. Once the page names another build, coming back to the tab shows the toast, with Reload.
// 3. Closed, it stays closed for that build; a newer one shows it again.
// 4. Reload loads the page again.
//
// No build is needed: the page is a stand-in with the build's version and this checkout's boot.js.
//
//   CHROME=<chromium> node new-build.mjs
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright-core';

const boot = readFileSync(new URL('../../../apps/photocraft-web/boot.js', import.meta.url), 'utf8');
const page = (v) => `<!doctype html><html><head><meta charset="utf-8"><script src="boot.js"></script></head><body>
<script type="module">const version = '${v}'; self.capishotBoot.watchBuild(version);</script></body></html>`;
const b = await chromium.launch({ executablePath: process.env.CHROME });
const results = [];
const ok = (n, c, e = '') => { results.push(`${c ? 'PASS' : 'FAIL'} ${n} ${e}`); console.log(results.at(-1)); };
const ctx = await b.newContext({ locale: 'es-ES' });
let current = 'aaaa';
let loads = 0;
await ctx.route('https://capishot.test/**', (r) => {
  const u = new URL(r.request().url());
  if (u.pathname.endsWith('/boot.js')) return r.fulfill({ contentType: 'text/javascript; charset=utf-8', body: boot });
  if (r.request().resourceType() === 'document') loads++;
  return r.fulfill({ contentType: 'text/html', body: page(current) });
});
const p = await ctx.newPage();
await p.goto('https://capishot.test/capishot/');
const front = async () => {
  // The check waits a minute between looks: the test moves the page's clock past it.
  await p.evaluate(() => { const n = Date.now; const skew = (self.__skew = (self.__skew || 0) + 61000); Date.now = () => n() + skew; });
  await p.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
  await p.waitForTimeout(500);
};
const toast = () => p.evaluate(() => document.getElementById('photocraft_new_build')?.textContent || '');

await front();
ok('the same build: no toast', (await toast()) === '');
current = 'bbbb';
await front();
const t = await toast();
ok('a new build: the toast, with Reload', t.includes('versión nueva de Capishot') && t.includes('Recargar'), t);
await p.click('#photocraft_new_build button[aria-label="Cerrar"]');
await front();
ok('closed: it stays closed for that build', (await toast()) === '');
current = 'cccc';
await front();
ok('a newer build: shown again', (await toast()) !== '');
const before = loads;
await p.click('#photocraft_new_build button:not([aria-label])');
await p.waitForLoadState('load');
ok('Reload loads the page again', loads === before + 1, `${loads - before} loads`);
await b.close();
process.exit(results.every((r) => r.startsWith('PASS')) ? 0 : 1);
