// The Font Finder searches the fonts a person stored, loaded this session or not: fonts are dropped
// on the page, the page reloads (they're stored, not read), and a drag with the tool on lettering
// reads them all first and answers among them.
// FONTS=<a folder of .ttf files> (never part of the repo). With SHOTS=<dir> it saves each step there.
//
//   CHROME=<chromium> FONTS=<dir> [COUNT=40] [SHOTS=<dir>] node font-finder-stored.mjs <url ending in />
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
if (shots) fs.mkdirSync(shots, { recursive: true });
const count = Number(process.env.COUNT || 40);
const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(d, e.name)) : e.name.endsWith('.ttf') && !e.name.includes('[') ? [path.join(d, e.name)] : []));
const files = walk(process.env.FONTS).sort().filter((_, i, all) => i % Math.ceil(all.length / count) === 0).slice(0, count);
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
const ctx = await b.newContext({ viewport: { width: 1280, height: 800 }, locale: 'es-ES' });
await ctx.route('**/*telemetry*', (r) => r.abort());
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
const ready = async () => {
  for (let i = 0; i < 600; i++) {
    try {
      await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 60000 });
      if (!(await req('engine.commands')).err) return;
    } catch { /* a reload while it starts */ }
    await p.waitForTimeout(500);
  }
};
const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return (r && r.result) || r; };
await p.goto(url);
await ready();
for (const f of files) {
  const bytes = [...fs.readFileSync(f)];
  await p.evaluate(([name, bytes]) => {
    const dt = new DataTransfer();
    dt.items.add(new File([new Uint8Array(bytes)], name, { type: 'font/ttf' }));
    for (const c of document.querySelectorAll('canvas')) {
      for (const type of ['dragenter', 'dragover', 'drop']) c.dispatchEvent(new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true }));
    }
  }, [path.basename(f), bytes]);
  await p.waitForTimeout(150);
}
await p.waitForTimeout(4000);
const stored = await ex('type.manageFonts', { list: true });
const n = (stored.fonts || stored.entries || []).length;
check(n >= files.length * 0.9, `${n} of ${files.length} fonts stored`);
await p.reload();
await ready();
const before = await ex('type.manageFonts', { list: true });
const loaded = (before.fonts || before.entries || []).filter((f) => f.loaded).length;
console.log(`after the reload: ${loaded} of ${n} read`);
await ex('file.new', { width: 900, height: 500, background: 'white' });
await ex('type.create', { text: 'NO PUEDE SER', size: 90, x: 60, y: 260, font: 'Inter', color: '#000000' });
await req('ui.set', { tool: 'FontFinder', fit: true });
await p.waitForTimeout(1500);
const raw = await req('ui.inspect');
const v = raw.result || raw;
console.log('tool', v.tool);
const [x0, y0, x1, y1] = v.canvasRect;
const started = Date.now();
await p.mouse.move(x0 + (x1 - x0) * 0.1, y0 + (y1 - y0) * 0.3);
await p.mouse.down();
await p.mouse.move(x0 + (x1 - x0) * 0.9, y0 + (y1 - y0) * 0.7, { steps: 6 });
await p.mouse.up();
await p.waitForTimeout(300);
await shot('1-reading');
let found = null;
for (let i = 0; i < 40 && !found; i++) {
  await p.waitForTimeout(2500);
  const faces = (await ex('font.find', { rect: [40, 150, 800, 150], scope: 'document' })).faces ?? 0;
  if (faces >= n * 0.8) found = faces;
}
console.log(`answered in ${Date.now() - started} ms among ${found} faces`);
check(found >= n * 0.8, 'the search looked through the stored fonts');
await p.waitForTimeout(1500);
await shot('2-answer');
const all = await ex('font.find', { rect: [40, 150, 800, 150] });
check((all.options || []).length > 0, `options: ${(all.options || []).map((o) => o.family).join(', ')}`);
await b.close();
console.log(failed ? `${failed} FAILED` : 'PASS');
process.exit(failed ? 1 : 0);
