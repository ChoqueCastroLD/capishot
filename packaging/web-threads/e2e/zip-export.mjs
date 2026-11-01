// Export Open Documents as ZIP in the browser: three open documents become one downloaded ZIP
// (numbered pages, in tab order); the dialog fits a 360 px phone. Works on either web build.
// With SHOTS=<dir> it saves screenshots there.
//
//   CHROME=<chromium> node zip-export.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
if (shots) fs.mkdirSync(shots, { recursive: true });
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`);
  if (!ok) failed++;
};
async function open(viewport, mobile) {
  const ctx = await b.newContext({ viewport, deviceScaleFactor: mobile ? 3 : 1, isMobile: mobile, hasTouch: mobile, locale: 'es-ES', acceptDownloads: true });
  const p = await ctx.newPage();
  p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
  await p.goto(url);
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
  const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return (r && r.result) || r || {}; };
  return { ctx, p, req, ex };
}

// Desktop-sized: the download.
{
  const { ctx, p, ex } = await open({ width: 1280, height: 860 }, false);
  await ex('prefs.set', { values: { 'interface.language': 'es' } });
  for (const [i, c] of ['#d04030', '#30a050', '#3050d0'].entries()) {
    await ex('file.new', { width: 300, height: 200, background: 'white', name: `pagina-${i + 1}` });
    await ex('select.all');
    await ex('edit.fill', { color: c });
    await ex('select.deselect');
  }
  const dl = p.waitForEvent('download', { timeout: 60000 });
  const r = await ex('file.export.openDocsZip', { path: 'capitulo.zip', naming: 'sequence', format: 'jpg', quality: 80 });
  const d = await dl;
  const file = await d.path();
  const bytes = fs.readFileSync(file);
  const names = [];
  // The ZIP's central directory names.
  for (let i = 0; i + 46 < bytes.length; i++) {
    if (bytes.readUInt32LE(i) === 0x02014b50) {
      const n = bytes.readUInt16LE(i + 28);
      names.push(bytes.subarray(i + 46, i + 46 + n).toString());
    }
  }
  check(d.suggestedFilename() === 'capitulo.zip', `downloaded ${d.suggestedFilename()} (${bytes.length} bytes): ${JSON.stringify(r).slice(0, 120)}`);
  check(JSON.stringify(names) === JSON.stringify(['001.jpg', '002.jpg', '003.jpg']), `pages in order: ${JSON.stringify(names)}`);
  await ctx.close();
}

// Phone: the dialog.
{
  const { ctx, p, req, ex } = await open({ width: 360, height: 780 }, true);
  await ex('prefs.set', { values: { 'interface.language': 'es', 'interface.simpleUi': 'on' } });
  for (let i = 0; i < 4; i++) await ex('file.new', { width: 300, height: 450, background: 'white', name: `p00${i + 1}` });
  await req('ui.menu.invoke', { command: 'file.export.openDocsZip' });
  await p.waitForTimeout(1200);
  if (shots) await p.screenshot({ path: `${shots}/zip-dialogo-movil-360.png` });
  const i = await req('ui.inspect');
  check(!!i, 'the dialog opened');
  await ctx.close();
}
await b.close();
console.log(failed ? `${failed} check(s) failed` : 'open documents export as one ZIP');
process.exit(failed ? 1 : 0);
