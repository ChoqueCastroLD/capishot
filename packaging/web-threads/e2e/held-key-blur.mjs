// A key held when the window loses focus never sends its keyup, and the canvas stays the active
// element, so eframe keeps the key down: Space stays the Hand and the Zoom tool stops zooming.
// After the window's blur the next click must zoom. Works on either web build.
//
//   CHROME=<chromium> node held-key-blur.mjs <url>
import { chromium } from 'playwright-core';
const [url] = process.argv.slice(2);
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const p = await (await b.newContext({ viewport: { width: 1280, height: 860 }, locale: 'en-US' })).newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
await p.goto(url);
await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
const inspect = async () => { const i = await req('ui.inspect'); return (i && i.result) || i || {}; };
const zoom = async () => (await inspect()).views?.[0]?.zoom;
await req('engine.execute', { command: 'file.new', params: { width: 1200, height: 800, background: 'white' } });
await req('ui.set', { tool: 'zoom' });
await p.waitForTimeout(800);
await p.mouse.move(640, 420);
await p.waitForTimeout(100);
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };

const z0 = await zoom();
await p.keyboard.down(' ');
await p.waitForTimeout(100);
await p.mouse.down(); await p.mouse.up();
await p.waitForTimeout(200);
check((await zoom()) === z0, `Space held: the click is the Hand's (${z0})`);

// Focus goes away with Space down; its keyup is never delivered to the canvas.
await p.evaluate(() => window.dispatchEvent(new Event('blur')));
await p.waitForTimeout(100);
await p.mouse.move(650, 420);
await p.waitForTimeout(100);
await p.mouse.down(); await p.mouse.up();
await p.waitForTimeout(400);
const z1 = await zoom();
check(typeof z0 === 'number' && z1 > z0, `after the window's blur the Zoom tool zooms (${z0} -> ${z1})`);
await p.keyboard.up(' ');
await b.close();
process.exit(failed ? 1 : 0);
