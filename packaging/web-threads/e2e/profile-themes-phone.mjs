// Edit › Preferences › Perfil y temas on a phone (360 × 780, simple UI, Spanish): the dialog's
// themes tab, the Theme editor with its live preview, and the export and import tabs; a custom
// theme in use survives a reload. Works on either web build. With SHOTS=<dir> it saves screenshots.
//
//   CHROME=<chromium> node profile-themes-phone.mjs <url>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [url] = process.argv.slice(2);
const shots = process.env.SHOTS;
if (shots) fs.mkdirSync(shots, { recursive: true });
const b = await chromium.launch({ executablePath: process.env.CHROME, args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'] });
const ctx = await b.newContext({ viewport: { width: 360, height: 780 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, locale: 'es-ES' });
const p = await ctx.newPage();
p.on('pageerror', (e) => console.log('PAGEERROR', e.message.slice(0, 300)));
let failed = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed++; };
const shot = async (name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png` }); };
const boot = async () => {
  await p.waitForFunction(() => self.photocraft && self.photocraft.request, null, { timeout: 300000 });
  for (let i = 0; i < 400; i++) { if (!(await req('engine.commands')).err) break; await p.waitForTimeout(250); }
};
const req = (m, pr = {}) => p.evaluate(([m, pr]) => self.photocraft.request(m, pr).then((r) => r, (e) => ({ err: String(e) })), [m, pr]);
const ex = async (c, pr = {}) => { const r = await req('engine.execute', { command: c, params: pr }); return r.result ?? r; };
// ui.dialog.set takes one field at a time.
const setFields = async (dialog, fields) => {
  for (const [field, value] of Object.entries(fields)) {
    const r = await req('ui.dialog.set', { dialog, field, value });
    check(!r?.err, `ui.dialog.set ${field}${r?.err ? ': ' + r.err : ''}`);
  }
};

await p.goto(url);
await boot();
await ex('prefs.set', { values: { 'interface.simpleUi': 'on', 'interface.language': 'es' } });
const css = ':root {\n  --base: studio;\n  --accent: #ff7a1a;\n  --card: #2a2035;\n  --radius: 10;\n}\nbutton:hover { --bg: #3b2d4a; }\n';
const saved = await ex('theme.custom.save', { name: 'Tinta naranja', css });
check(!!saved.id, `a custom theme is saved and used: ${JSON.stringify(saved)}`);
await p.waitForTimeout(1500);
const opened = await req('ui.menu.invoke', { id: 'edit.preferences.profile' });
const dialog = opened.result?.dialog ?? opened.dialog;
check(!!dialog, `Perfil y temas opens: ${JSON.stringify(opened).slice(0, 200)}`);
await p.waitForTimeout(1500);
await shot('01-temas');
await setFields(dialog, { edit: { id: saved.id, name: 'Tinta naranja', css, meta: { author: 'Equipo' }, icon: '', text: false } });
await p.waitForTimeout(1500);
await shot('02-editor');
await setFields(dialog, { edit: { id: saved.id, name: 'Tinta naranja', css, meta: {}, icon: '', text: true } });
await p.waitForTimeout(1200);
await shot('03-editar-como-texto');
await setFields(dialog, { edit: null, tab: 'export' });
await p.waitForTimeout(1200);
await shot('04-exportar');
await setFields(dialog, { tab: 'import' });
await p.waitForTimeout(1200);
await shot('05-importar');
// The theme in use comes back after a reload (preferences persistence).
await p.waitForTimeout(2500);
await p.reload();
await boot();
await p.waitForTimeout(2000);
const back = await ex('theme.custom.list');
check(back.active === saved.id, `the theme in use is restored after a reload: ${JSON.stringify(back).slice(0, 200)}`);
await shot('06-tras-recargar');
await b.close();
process.exit(failed ? 1 : 0);
