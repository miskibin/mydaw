// Frame times of everyday actions, driven with the real mouse / keyboard / wheel against the mock
// backend and a generated long project (perf/.cache/<size>/project.json).
//   node perf/ui_actions.mjs [--size large] [--out perf/results/actions.json] [--no-build] [--only a,b] [--profile]
// Per action: frame time p50/p95/max, long tasks (n / total / max), and with --profile the top
// self-time functions. "open" actions also report the time from the action to the second frame.
// Headless Chromium rasterises in software: absolute numbers are pessimistic for canvas work,
// before/after comparisons on the same machine are what this is for.
import { chromium } from 'playwright-core';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const arg = (k, d) => { const i = process.argv.indexOf('--' + k); return i < 0 ? d : (process.argv[i + 1]?.startsWith('--') || process.argv[i + 1] == null ? true : process.argv[i + 1]); };
const size = arg('size', 'large');
const out = resolve(root, arg('out', `perf/results/actions_${size}.json`));
const only = arg('only', '') ? String(arg('only')).split(',') : null;
const profile = !!arg('profile', false);
const dist = join(root, 'perf/.cache/ui-dist-actions');
const fixture = join(root, 'perf/.cache', size, 'project.json');
if (!existsSync(fixture)) { console.error(`missing ${fixture}; run: cargo run --release -p wave-perf -- gen ${size}`); process.exit(2); }

if (!arg('no-build', false)) {
  console.error('[act] building with VITE_PERF=1');
  const r = spawnSync('npx', ['vite', 'build', '--outDir', dist, '--emptyOutDir', '--minify', 'false'], { cwd: join(root, 'ui'), env: { ...process.env, VITE_PERF: '1' }, shell: true, stdio: ['ignore', 'ignore', 'inherit'] });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.woff': 'font/woff', '.svg': 'image/svg+xml', '.json': 'application/json' };
const server = createServer((req, res) => {
  let p = decodeURIComponent((req.url || '/').split('?')[0]); if (p === '/') p = '/index.html';
  const f = join(dist, p);
  if (!f.startsWith(dist) || !existsSync(f) || statSync(f).isDirectory()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'content-type': mime[extname(f)] || 'application/octet-stream' }); res.end(readFileSync(f));
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const url = `http://127.0.0.1:${server.address().port}/`;

function findChrome() {
  if (process.env.PERF_CHROME) return process.env.PERF_CHROME;
  const base = join(process.env.LOCALAPPDATA || '', 'ms-playwright');
  if (existsSync(base)) for (const d of readdirSync(base).sort().reverse()) {
    if (!d.startsWith('chromium_headless_shell-')) continue;
    const p = join(base, d, 'chrome-headless-shell-win64/chrome-headless-shell.exe'); if (existsSync(p)) return p;
  }
  for (const p of ['C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium']) if (existsSync(p)) return p;
  return undefined;
}
const browser = await chromium.launch({ executablePath: findChrome(), headless: !arg('headed', false), args: ['--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows', '--js-flags=--expose-gc'] });
const ctx = await browser.newContext({ viewport: { width: 1600, height: 900 }, deviceScaleFactor: 1 });
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
await cdp.send('Profiler.enable');
await cdp.send('Profiler.setSamplingInterval', { interval: 200 });
const errors = [];
page.on('pageerror', e => errors.push(String(e).slice(0, 200)));

const project = JSON.parse(readFileSync(fixture, 'utf8'));
await page.addInitScript(p => { window.__waveFixture = p; }, project);
await page.addInitScript(() => {
  const M = window.__m = { frames: [], long: [], running: false, last: 0 };
  try { new PerformanceObserver(l => { if (M.running) for (const e of l.getEntries()) M.long.push(e.duration); }).observe({ type: 'longtask', buffered: false }); } catch {}
  M.start = () => { M.frames = []; M.long = []; M.running = true; M.last = performance.now(); const loop = t => { if (!M.running) return; M.frames.push(t - M.last); M.last = t; requestAnimationFrame(loop); }; requestAnimationFrame(loop); };
  M.stop = () => { M.running = false; return { frames: M.frames.slice(1), long: M.long }; };
});

const pct = (a, q) => { if (!a.length) return 0; const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.round((s.length - 1) * q))]; };
const r1 = x => Math.round(x * 10) / 10;
const sum = a => a.reduce((x, y) => x + y, 0);
function topSelf(prof, n = 12) {
  const byId = new Map(prof.nodes.map(x => [x.id, x])), self = new Map();
  prof.samples.forEach((id, i) => self.set(id, (self.get(id) || 0) + (prof.timeDeltas[i] || 0)));
  const agg = new Map();
  for (const [id, us] of self) {
    const cf = byId.get(id).callFrame;
    if (cf.functionName === '(idle)' || cf.functionName === '(root)' || cf.functionName === '(program)') continue;
    const key = `${cf.functionName || '(anon)'} ${(cf.url || '').split('/').pop()}:${cf.lineNumber + 1}`;
    agg.set(key, (agg.get(key) || 0) + us);
  }
  return [...agg].sort((a, b) => b[1] - a[1]).slice(0, n).map(([fn, us]) => `${r1(us / 1000)}ms ${fn}`);
}
const P = (f, a) => page.evaluate(f, a);
const frames = n => P(n => new Promise(res => { let i = 0; const l = () => (++i >= n ? res() : requestAnimationFrame(l)); requestAnimationFrame(l); }), n);
const now = () => P(() => performance.now());
const results = { meta: { size, chrome: await browser.version(), date: new Date().toISOString() }, actions: {} };

async function act(name, fn) {
  if (only && !only.includes(name)) return;
  await page.waitForTimeout(200);
  await P(() => window.gc && window.gc());
  await P(() => window.__m.start());
  if (profile) await cdp.send('Profiler.start');
  let extra = null;
  const t0 = Date.now();
  try { extra = await fn(); } catch (e) { extra = { error: String(e).slice(0, 300) }; }
  const wall = Date.now() - t0;
  const prof = profile ? (await cdp.send('Profiler.stop')).profile : null;
  const r = await P(() => window.__m.stop());
  const f = r.frames;
  const a = { wall_ms: wall, frames: f.length, p50: r1(pct(f, 0.5)), p95: r1(pct(f, 0.95)), max: r1(Math.max(0, ...f)), long_n: r.long.length, long_ms: r1(sum(r.long)), long_max: r1(Math.max(0, ...r.long)) };
  if (extra) a.extra = extra;
  if (prof) a.hot = topSelf(prof);
  results.actions[name] = a;
  console.error(`[act] ${name.padEnd(26)} p50 ${a.p50} p95 ${a.p95} max ${a.max} long ${a.long_n}/${a.long_ms}ms ${extra ? JSON.stringify(extra) : ''}`);
  if (prof) for (const h of a.hot.slice(0, 8)) console.error('        ' + h);
}
/** Time from `f` (in page) to the second painted frame after it. */
const toFrame = f => P(async src => { const a = performance.now(); await (0, eval)(src)(); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); return +(performance.now() - a).toFixed(1); }, f.toString());

// ---------- boot
await page.goto(url, { waitUntil: 'load' });
await page.waitForSelector('canvas.gridCanvas', { timeout: 60000 });
await page.waitForFunction(() => window.__perf && window.__perf.S.ready && window.__waveMock, null, { timeout: 60000 });
await frames(10);
const D = 2000;
const grid = await P(() => { const r = window.__perf.geom.scroll.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height }; });
const wheel = (n, mk) => P(([n, mk, g]) => new Promise(res => { const el = window.__perf.geom.scroll; let i = 0; const f = (0, eval)(mk); const l = () => { el.dispatchEvent(new WheelEvent('wheel', { ...f(i), clientX: g.x + 600, clientY: g.y + 300, bubbles: true, cancelable: true })); ++i < n ? requestAnimationFrame(l) : res(); }; requestAnimationFrame(l); }), [n, mk.toString(), grid]);

// ---------- playlist
await act('wheel_scroll_mouse', () => wheel(120, i => ({ deltaY: i % 60 < 30 ? 100 : -100, deltaMode: 0 })));
await act('wheel_scroll_h_mouse', () => wheel(120, i => ({ deltaY: i % 60 < 30 ? 100 : -100, shiftKey: true })));
await act('touchpad_pan', () => wheel(180, i => ({ deltaX: Math.round(6 * Math.sin(i / 20)), deltaY: Math.round(4 * Math.cos(i / 15)) })));
await act('touchpad_pinch_zoom', () => wheel(180, i => ({ deltaY: (i % 90 < 45 ? -1 : 1) * 3, ctrlKey: true })));
await act('ctrl_wheel_zoom', () => wheel(80, i => ({ deltaY: i % 40 < 20 ? -100 : 100, ctrlKey: true })));
await P(() => { const el = window.__perf.geom.scroll; el.scrollLeft = 0; el.scrollTop = 0; });
await frames(4);

const clipPt = async () => {
  const id = await P(() => {
    const { S, geom, U } = window.__perf;
    const c = S.project.clips.find(c => c.kind === 'audio' && c.track >= 4 && c.track < 10 && c.len * U.bw > 120) || S.project.clips[0];
    geom.scroll.scrollLeft = Math.max(0, c.start * U.bw - 150); geom.scroll.scrollTop = Math.max(0, geom.tops[c.track] - 60);
    return c.id;
  });
  await frames(4);
  return P(id => { const { S, geom, U } = window.__perf; const c = S.project.clips.find(x => x.id === id), r = geom.grid.getBoundingClientRect(); return { id, x: r.left + c.start * U.bw, y: r.top + geom.tops[c.track] + 14, w: c.len * U.bw }; }, id);
};
let cp = await clipPt();
await act('clip_drag', async () => {
  await page.mouse.move(cp.x + 40, cp.y); await page.mouse.down();
  for (let i = 1; i <= 80; i++) await page.mouse.move(cp.x + 40 + i * 4, cp.y + Math.sin(i / 8) * 20);
  await page.mouse.up(); await page.waitForTimeout(200);
});
await P(() => window.__perf.A.undo());
await page.waitForTimeout(200);
cp = await clipPt();
await act('clip_resize', async () => {
  const x = cp.x + cp.w - 3;
  await page.mouse.move(x, cp.y); await page.mouse.down();
  for (let i = 1; i <= 80; i++) await page.mouse.move(x - i * 1.5, cp.y);
  await page.mouse.up(); await page.waitForTimeout(200);
});
await P(() => window.__perf.A.undo());
await page.waitForTimeout(200);
await P(() => { const el = window.__perf.geom.scroll; el.scrollLeft = 0; el.scrollTop = 0; }); await frames(3);
await act('marquee_select', async () => {
  const g = await P(() => { const r = window.__perf.geom.grid.getBoundingClientRect(); return { x: r.left, y: r.top }; });
  const x = g.x + 40, y = g.y + 10;
  await page.keyboard.down('Control');
  await page.mouse.move(x, y); await page.mouse.down();
  for (let i = 1; i <= 80; i++) await page.mouse.move(x + i * 6, y + i * 4);
  await page.mouse.up(); await page.keyboard.up('Control'); await page.waitForTimeout(200);
  return { selected: await P(() => window.__perf.U.sel.length) };
});
await P(() => window.__perf.setU('sel', []));
await act('playback_follow', async () => {
  await P(() => { window.__perf.setU('follow', true); window.__perf.A.seek(8); return window.__perf.A.togglePlay(); });
  await page.waitForTimeout(D + 1000);
  await P(() => window.__perf.A.togglePlay());
});
await P(() => { window.__perf.setU('follow', false); window.__perf.A.seek(0); });

// ---------- mixer
await act('mixer_open_close', async () => {
  const lat = [];
  for (let i = 0; i < 6; i++) lat.push(await toFrame(() => window.__perf.setU('mixOpen', !window.__perf.U.mixOpen)));
  return { to_2nd_frame_ms: lat };
});
await P(() => window.__perf.setU('mixOpen', true)); await frames(5);
await act('fader_drag', async () => {
  const r = await P(() => { const f = document.querySelectorAll('.mixer .fader')[3]; const b = f.getBoundingClientRect(); return { x: b.left + b.width / 2, y: b.top + b.height / 2 }; });
  await page.mouse.move(r.x, r.y); await page.mouse.down();
  for (let i = 1; i <= 80; i++) await page.mouse.move(r.x, r.y + Math.sin(i / 10) * 40);
  await page.mouse.up(); await page.waitForTimeout(200);
});
await act('mixer_knob_drag', async () => {
  const r = await P(() => { const f = document.querySelectorAll('.mixer .pandial')[3]; const b = f.getBoundingClientRect(); return { x: b.left + b.width / 2, y: b.top + b.height / 2 }; });
  await page.mouse.move(r.x, r.y); await page.mouse.down();
  for (let i = 1; i <= 80; i++) await page.mouse.move(r.x, r.y + Math.sin(i / 10) * 40);
  await page.mouse.up(); await page.waitForTimeout(200);
});
await P(() => window.__perf.setU('mixOpen', false)); await frames(5);

// ---------- effect windows
const fxOf = await P(() => { const { S } = window.__perf; const o = {}; for (const m of S.project.inserts) for (const f of m.fx) if (!o[f.type]) o[f.type] = f.id; return o; });
await act('fx_window_open_each', async () => {
  const lat = {};
  for (const [type, id] of Object.entries(fxOf)) {
    if (type === 'vst3') continue;
    lat[type] = await P(async id => { const a = performance.now(); window.__perf.A.openPlug(id); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); const t = +(performance.now() - a).toFixed(1); window.__perf.A.closePlug(id); await new Promise(r => requestAnimationFrame(r)); return t; }, id);
  }
  return lat;
});
await act('fx_add_each_type', async () => {
  const lat = {};
  for (const type of ['voice', 'tune', 'eq', 'comp', 'sat', 'delay', 'reverb', 'limit', 'fet', 'deess', 'denoise', 'rider']) {
    lat[type] = await P(async type => { const { A, S } = window.__perf; const ins = S.project.tracks[2].insert; const a = performance.now(); A.addFx(ins, type); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); const t = +(performance.now() - a).toFixed(1); const m = S.project.inserts.find(x => x.id === ins); const f = m.fx[m.fx.length - 1]; A.removeFx(f.id); await new Promise(r => setTimeout(r, 50)); return t; }, type);
  }
  return lat;
});
await act('fx_bypass_toggle', async () => {
  const lat = [];
  for (let i = 0; i < 8; i++) lat.push(await P(async id => { const a = performance.now(); window.__perf.A.toggleBypass(id); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); return +(performance.now() - a).toFixed(1); }, fxOf.comp));
  return { to_2nd_frame_ms: lat };
});
if (fxOf.comp) {
  await P(id => window.__perf.A.openPlug(id), fxOf.comp); await frames(5);
  await act('fx_knob_drag', async () => {
    const r = await P(() => { const f = document.querySelector('.fxwin .knob .dial, .win .knob .dial'); if (!f) return null; const b = f.getBoundingClientRect(); return { x: b.left + b.width / 2, y: b.top + b.height / 2 }; });
    if (!r) return { error: 'no knob' };
    await page.mouse.move(r.x, r.y); await page.mouse.down();
    for (let i = 1; i <= 80; i++) await page.mouse.move(r.x, r.y + Math.sin(i / 10) * 40);
    await page.mouse.up(); await page.waitForTimeout(200);
  });
  await P(id => window.__perf.A.closePlug(id), fxOf.comp); await frames(3);
}

// ---------- pattern editor / piano roll
const stress = await P(() => { const { S } = window.__perf; const p = S.project.patterns.find(x => x.name.startsWith('Piano roll stress')) || S.project.patterns[0]; return { id: p.id, ch: p.notes[0]?.ch, notes: p.notes.length, first: S.project.patterns[0].id }; });
await act('pattern_editor_open', async () => {
  const lat = [];
  for (let i = 0; i < 4; i++) { lat.push(await P(async s => { const a = performance.now(); window.__perf.P.openPattern(s.first, 'rack'); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); const t = +(performance.now() - a).toFixed(1); window.__perf.P.closePattern(); await new Promise(r => requestAnimationFrame(r)); return t; }, stress)); }
  return { to_2nd_frame_ms: lat };
});
await act('pianoroll_open', async () => {
  const lat = [];
  for (let i = 0; i < 4; i++) { lat.push(await P(async s => { const a = performance.now(); window.__perf.P.openPattern(s.id, 'roll', s.ch); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); const t = +(performance.now() - a).toFixed(1); window.__perf.P.closePattern(); await new Promise(r => requestAnimationFrame(r)); return t; }, stress)); }
  return { to_2nd_frame_ms: lat, notes: stress.notes };
});
await P(s => window.__perf.P.openPattern(s.id, 'roll', s.ch), stress); await frames(5);
const roll = await P(() => { const cs = [...document.querySelectorAll('.win canvas')].filter(c => c.getBoundingClientRect().width > 400); const c = cs[0]; if (!c) return null; const r = c.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height }; });
if (roll) {
  await act('pianoroll_edit_notes', async () => {
    const n0 = await P(s => window.__perf.S.project.patterns.find(p => p.id === s.id).notes.length, stress);
    for (let i = 0; i < 10; i++) { await page.mouse.click(roll.x + 120 + i * 37, roll.y + 60 + (i % 5) * 18); await page.waitForTimeout(30); }
    await page.mouse.move(roll.x + 120, roll.y + 60); await page.mouse.down();
    for (let i = 1; i <= 50; i++) await page.mouse.move(roll.x + 120 + i * 4, roll.y + 60 + Math.round(i / 6) * 4);
    await page.mouse.up(); await page.waitForTimeout(200);
    return { notes_added: (await P(s => window.__perf.S.project.patterns.find(p => p.id === s.id).notes.length, stress)) - n0 };
  });
  await act('pianoroll_wheel', () => P(n => new Promise(res => { const c = [...document.querySelectorAll('.win canvas')].filter(c => c.getBoundingClientRect().width > 400)[0], r = c.getBoundingClientRect(); let i = 0; const l = () => { c.dispatchEvent(new WheelEvent('wheel', { deltaY: i % 4 < 2 ? 40 : -40, shiftKey: i % 8 > 3, clientX: r.left + 300, clientY: r.top + 200, bubbles: true, cancelable: true })); ++i < n ? requestAnimationFrame(l) : res(); }; requestAnimationFrame(l); }), 100));
}
await P(() => window.__perf.P.closePattern()); await frames(3);

// ---------- notebook
await act('notebook_typing', async () => {
  await page.keyboard.press('F8'); await frames(3);
  const ok = await P(() => { const t = document.querySelector('textarea.nb-ta'); if (!t) return false; t.focus(); t.selectionStart = t.selectionEnd = t.value.length; return true; });
  if (!ok) return { error: 'no notebook' };
  await page.keyboard.type('la la la tekst piosenki który się pisze szybko\n'.repeat(3), { delay: 15 });
  await page.waitForTimeout(200);
  await P(() => document.activeElement && document.activeElement.blur());
  await page.keyboard.press('F8'); await frames(3);
});

// ---------- menus / settings
await act('context_menu_open_close', async () => {
  const lat = [];
  for (let i = 0; i < 6; i++) {
    const a = await now(); await page.mouse.click(grid.x + 500, grid.y + 80 + i * 10, { button: 'right' }); await frames(2); lat.push(r1((await now()) - a));
    await page.keyboard.press('Escape'); await frames(2);
  }
  return { to_2nd_frame_ms: lat };
});
await act('settings_open_close', async () => {
  const lat = [];
  for (let i = 0; i < 6; i++) lat.push(await toFrame(() => window.__perf.setU('settings', !window.__perf.U.settings)));
  return { to_2nd_frame_ms: lat };
});
await P(() => window.__perf.setU('settings', false));

results.errors = errors.slice(0, 10);
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, JSON.stringify(results, null, 2));
console.error('[act] wrote', out, errors.length ? 'errors: ' + errors.slice(0, 3).join(' | ') : '');
await browser.close(); server.close();
