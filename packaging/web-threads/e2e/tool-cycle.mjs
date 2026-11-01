// Tool letters cycle their group and a toast shows it (desktop 1280 × 860, Spanish): J from the
// Brush picks the J group's tool, J again steps to the next, ⇧J back; the toast shows the
// previous, current and next tool, slides in, crossfades on a change and fades out. Letters typed
// into the palette's search never switch tools. On a phone (390 × 844) a bar pick shows the toast
// above the bars. Works on either web build. With SHOTS=<dir> it saves a frame sequence there.
//
//   CHROME=<chromium> node tool-cycle.mjs <url>
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
  const ctx = await b.newContext({ viewport, deviceScaleFactor: mobile ? 3 : 1, isMobile: mobile, hasTouch: mobile, locale: 'es-ES' });
  const p = await ctx.newPage();
  p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
  await p.goto(url);
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
  for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
  const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return (r && r.result) || r || {}; };
  const inspect = async () => { const i = await req('ui.inspect'); return (i && i.result) || i || {}; };
  return { ctx, p, req, ex, inspect };
}
const shot = async (p, name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };

// Desktop.
{
  const { ctx, p, req, ex, inspect } = await open({ width: 1280, height: 860 }, false);
  await ex('prefs.set', { values: { 'interface.language': 'es', 'interface.simpleUi': 'off' } });
  await ex('file.new', { width: 1200, height: 800, background: 'white' });
  await req('ui.set', { tool: 'brush' });
  await p.waitForTimeout(800);
  await p.mouse.move(640, 420);
  const tool = async () => JSON.stringify((await inspect()).tool);
  await p.keyboard.press('j');
  await p.waitForTimeout(40);
  await shot(p, '01-j-aparece-40ms');
  await p.waitForTimeout(80);
  await shot(p, '02-j-aparece-120ms');
  await p.waitForTimeout(300);
  await shot(p, '03-j-en-su-sitio');
  const first = await tool();
  check(/Remove|Spot|Healing|Patch|RedEye|Bubble/.test(first), `J from the Brush picks the J group's tool: ${first}`);
  await req('ui.set', { tool: 'removeAi' });
  await p.waitForTimeout(300);
  await p.keyboard.press('j');
  await p.waitForTimeout(60);
  await shot(p, '04-j-otra-vez-cambio-lateral-60ms');
  await p.waitForTimeout(350);
  await shot(p, '05-quitar-texto-en-globo');
  check((await tool()).includes('BubbleTextRemover'), `J again steps to the next: ${await tool()}`);
  const v = (await inspect()).toolToast;
  check(v && /Remove with AI/.test(v.prev) && /Bubble Text/.test(v.current) && /Spot Healing/.test(v.next), `the toast: ${JSON.stringify(v)}`);
  await p.keyboard.press('Shift+J');
  await p.waitForTimeout(60);
  await shot(p, '06-mayus-j-atras-60ms');
  check((await tool()).includes('RemoveAi'), `⇧J goes back: ${await tool()}`);
  await p.waitForTimeout(1500);
  await shot(p, '07-desvaneciendo');
  await p.waitForTimeout(600);
  check(!(await inspect()).toolToast, 'it fades out and goes');
  await shot(p, '08-sin-toast');
  // Letters typed into the palette's search stay text.
  await req('ui.set', { tool: 'brush' });
  await ex('edit.search');
  await p.waitForTimeout(500);
  await p.keyboard.type('jbvtm');
  await p.waitForTimeout(300);
  await shot(p, '09-paleta-escribe-letras');
  check((await tool()).includes('Brush'), `typing in the palette switches no tool: ${await tool()}`);
  await ctx.close();
}

// Phone.
{
  const { ctx, p, ex, inspect } = await open({ width: 390, height: 844 }, true);
  await ex('prefs.set', { values: { 'interface.language': 'es', 'interface.simpleUi': 'on' } });
  await ex('file.new', { width: 1200, height: 1800, background: 'white' });
  await p.waitForTimeout(1200);
  const cdp = await ctx.newCDPSession(p);
  const slot = ((await inspect()).simpleUi.buttons || []).find((x) => x.label.startsWith('slot:tool:') && x.label !== 'slot:tool:Move');
  const [x, y] = [(slot.rect[0] + slot.rect[2]) / 2, (slot.rect[1] + slot.rect[3]) / 2];
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y, id: 0 }] });
  await p.waitForTimeout(16);
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await p.waitForTimeout(350);
  await shot(p, '10-movil-toast-sobre-las-barras');
  const v = (await inspect()).toolToast;
  check(!!(v && v.current), `a bar pick shows the toast: ${JSON.stringify(v)}`);
  await ctx.close();
}

await b.close();
console.log(failed ? `${failed} check(s) failed` : 'tool letters cycle with a toast');
process.exit(failed ? 1 : 0);
