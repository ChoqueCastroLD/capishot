// Save to device: where the browser can save to a file the user picks (`showSaveFilePicker`),
// Save As asks for the file once and every File › Save after it writes that file in place, with
// no new download; without the API, saves download as before. The picker is the browser's own
// window, so it is stood in for by a handle that records what is written. Phone layout.
//
//   CHROME=<chromium> CAPISHOT_FREEZE_PAGES=<dir with page1.jpg> node save-to-device.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import path from 'node:path';
const [url] = process.argv.slice(2);
const dir = process.env.CAPISHOT_FREEZE_PAGES || '.';
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const results = [];
const ok = (name, pass, extra = '') => { results.push(pass); console.log(`${pass ? 'PASS' : 'FAIL'} ${name} ${extra}`); };
const session = async (picker) => {
  const ctx = await b.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, acceptDownloads: true });
  await ctx.addInitScript((picker) => {
    if (!picker) {
      Object.defineProperty(self, 'showSaveFilePicker', { value: undefined, configurable: true, writable: true });
      return;
    }
    self.__picks = [];
    self.__writes = [];
    self.showSaveFilePicker = async (opts) => {
      self.__picks.push(opts.suggestedName);
      const name = opts.suggestedName;
      return {
        name,
        kind: 'file',
        queryPermission: async () => 'granted',
        createWritable: async () => {
          let size = 0;
          return {
            write: async (d) => { size += d.size ?? d.byteLength ?? 0; },
            close: async () => { self.__writes.push({ name, size }); },
            abort: async () => {},
          };
        },
      };
    };
  }, picker);
  const p = await ctx.newPage();
  const downloads = [];
  p.on('download', (d) => { downloads.push(d.suggestedFilename()); d.cancel().catch(() => {}); });
  await p.goto(url);
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
  const bytes = [...fs.readFileSync(path.join(dir, 'page1.jpg'))];
  await p.evaluate(([bb, n]) => self.photocraft.open(new Uint8Array(bb), n), [bytes, 'page1.jpg']);
  for (let i = 0; i < 120; i++) { const d = await req('engine.execute', { command: 'document.inspect', params: {} }); if (d && d.name === 'page1.jpg') break; await p.waitForTimeout(250); }
  const until = async (f, ms = 120000) => { const t = Date.now(); while (Date.now() - t < ms) { if (await f()) return true; await p.waitForTimeout(250); } return false; };
  return { ctx, p, req, downloads, until };
};

// With the API: Save As picks the file once; Save writes it again in place.
{
  const { ctx, p, req, downloads, until } = await session(true);
  await p.mouse.click(195, 420);
  await p.keyboard.press('Control+Shift+KeyS');
  await until(async () => (await p.evaluate(() => self.__writes.length)) >= 1);
  const first = await p.evaluate(() => ({ picks: self.__picks, writes: self.__writes }));
  ok('Save As asks for a file and writes it', first.picks.length === 1 && first.writes.length === 1 && first.writes[0].size > 1000, JSON.stringify(first));
  await req('engine.execute', { command: 'edit.fill', params: { contents: 'color', color: '#336699' } });
  await p.keyboard.press('Control+KeyS');
  await until(async () => (await p.evaluate(() => self.__writes.length)) >= 2);
  const second = await p.evaluate(() => ({ picks: self.__picks, writes: self.__writes }));
  ok('Save writes the same file again, no picker', second.picks.length === 1 && second.writes.length === 2 && second.writes[1].name === second.writes[0].name, JSON.stringify(second));
  await p.waitForTimeout(1500);
  ok('no download', downloads.length === 0, JSON.stringify(downloads));
  // Written in place, not a download the user may cancel: the document is saved.
  const clean = await until(async () => (await p.evaluate(() => self.photocraft.isDirty())) === false, 10000);
  ok('the document is saved', clean);
  await ctx.close();
}
// Without it: a download, as before.
{
  const { ctx, p, downloads, until } = await session(false);
  await p.mouse.click(195, 420);
  await p.keyboard.press('Control+Shift+KeyS');
  ok('without the API, Save As downloads', await until(async () => downloads.length >= 1), JSON.stringify(downloads));
  await ctx.close();
}
await b.close();
console.log(results.every(Boolean) ? 'ALL PASSED' : 'SOME FAILED');
if (!results.every(Boolean)) process.exitCode = 1;
