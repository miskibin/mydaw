// UI profile against the mock backend and a generated long project.
//   node perf/ui_bench.mjs [--size large] [--out perf/results/ui.json] [--quick] [--no-build] [--headed]
// Builds the UI with VITE_PERF=1 (stores exposed as window.__perf; unminified so function names survive),
// serves it, drives scenarios in Chromium and records frame times, long tasks, CPU-profile hot
// functions, layout/style/script time, DOM mutations, canvas calls and mock-IPC volume.
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
const quick = !!arg('quick', false);
const out = resolve(root, arg('out', `perf/results/ui_${size}.json`));
const dist = join(root, 'perf/.cache/ui-dist');
const fixture = join(root, 'perf/.cache', size, 'project.json');
if (!existsSync(fixture)) { console.error(`missing ${fixture}; run: cargo run --release -p wave-perf -- gen ${size}`); process.exit(2); }

if (!arg('no-build', false)) {
  console.error('[ui] building with VITE_PERF=1');
  const r = spawnSync('npx', ['vite', 'build', '--outDir', dist, '--emptyOutDir', '--minify', 'false'], { cwd: join(root, 'ui'), env: { ...process.env, VITE_PERF: '1' }, shell: true, stdio: ['ignore', 'ignore', 'inherit'] });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

// ---------- static server
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.woff': 'font/woff', '.svg': 'image/svg+xml', '.json': 'application/json' };
const server = createServer((req, res) => {
  let p = decodeURIComponent((req.url || '/').split('?')[0]); if (p === '/') p = '/index.html';
  const f = join(dist, p);
  if (!f.startsWith(dist) || !existsSync(f) || statSync(f).isDirectory()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'content-type': mime[extname(f)] || 'application/octet-stream' }); res.end(readFileSync(f));
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const url = `http://127.0.0.1:${server.address().port}/`;

// ---------- browser
function findChrome() {
  if (process.env.PERF_CHROME) return process.env.PERF_CHROME;
  const base = join(process.env.LOCALAPPDATA || '', 'ms-playwright');
  if (existsSync(base)) for (const d of readdirSync(base).sort().reverse()) {
    if (!d.startsWith('chromium_headless_shell-')) continue;
    for (const sub of ['chrome-headless-shell-win64/chrome-headless-shell.exe', 'chrome-headless-shell-linux64/chrome-headless-shell']) {
      const p = join(base, d, sub); if (existsSync(p)) return p;
    }
  }
  for (const p of ['C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium']) if (existsSync(p)) return p;
  return undefined;
}
const browser = await chromium.launch({ executablePath: findChrome(), headless: !arg('headed', false), args: ['--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows', '--js-flags=--expose-gc'] });
const ctx = await browser.newContext({ viewport: { width: 1600, height: 900 }, deviceScaleFactor: 1 });
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
await cdp.send('Profiler.enable'); await cdp.send('Performance.enable');
await cdp.send('Profiler.setSamplingInterval', { interval: 250 });
const consoleErrors = [];
page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 200)); });
page.on('pageerror', e => consoleErrors.push('pageerror: ' + String(e).slice(0, 200)));

const project = JSON.parse(readFileSync(fixture, 'utf8'));
await page.addInitScript(p => { window.__waveFixture = p; }, project);
await page.addInitScript(() => {
  // frame sampler, long tasks, DOM mutations, canvas call counters
  const M = window.__m = { frames: [], long: [], mut: 0, canvas: {}, full: {}, resets: 0, running: false, last: 0 };
  try { new PerformanceObserver(l => { if (M.running) for (const e of l.getEntries()) M.long.push(e.duration); }).observe({ type: 'longtask', buffered: false }); } catch {}
  const cls = c => (c.className || c.id || 'canvas') + '';
  const P = CanvasRenderingContext2D.prototype;
  for (const k of ['fillRect', 'strokeRect', 'clearRect', 'drawImage', 'fill', 'stroke', 'fillText', 'strokeText', 'measureText', 'putImageData', 'getImageData', 'createLinearGradient', 'setLineDash']) {
    const o = P[k]; P[k] = function (...a) {
      if (M.running) {
        M.canvas[k] = (M.canvas[k] || 0) + 1;
        if ((k === 'clearRect' || k === 'fillRect' || k === 'drawImage') && this.canvas) {
          const c = this.canvas, w = k === 'drawImage' ? a[3] ?? c.width : a[2], h = k === 'drawImage' ? a[4] ?? c.height : a[3];
          if (c.width > 200 && Math.abs(w) >= c.width * 0.9 && Math.abs(h) >= c.height * 0.9) { const n = cls(c); M.full[n] = (M.full[n] || 0) + 1; }
        }
      }
      return o.apply(this, a);
    };
  }
  const wd = Object.getOwnPropertyDescriptor(HTMLCanvasElement.prototype, 'width');
  Object.defineProperty(HTMLCanvasElement.prototype, 'width', { get() { return wd.get.call(this); }, set(v) { if (M.running) M.resets++; wd.set.call(this, v); }, configurable: true });
  new MutationObserver(r => { if (M.running) M.mut += r.length; }).observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  M.start = () => { M.frames = []; M.long = []; M.mut = 0; M.canvas = {}; M.full = {}; M.resets = 0; M.running = true; M.last = performance.now(); const loop = t => { if (!M.running) return; M.frames.push(t - M.last); M.last = t; requestAnimationFrame(loop); }; requestAnimationFrame(loop); };
  M.stop = () => { M.running = false; return { frames: M.frames.slice(1), long: M.long, mut: M.mut, canvas: M.canvas, full: M.full, resets: M.resets }; };
});

// ---------- helpers
const pct = (a, q) => { if (!a.length) return 0; const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.round((s.length - 1) * q))]; };
const r1 = x => Math.round(x * 10) / 10;
const sum = a => a.reduce((x, y) => x + y, 0);
async function metrics() { const { metrics } = await cdp.send('Performance.getMetrics'); const o = {}; for (const m of metrics) o[m.name] = m.value; return o; }
function topSelf(profile, n = 14) {
  const byId = new Map(profile.nodes.map(x => [x.id, x]));
  const self = new Map();
  profile.samples.forEach((id, i) => self.set(id, (self.get(id) || 0) + (profile.timeDeltas[i] || 0)));
  const agg = new Map();
  for (const [id, us] of self) {
    const cf = byId.get(id).callFrame;
    if (cf.functionName === '(idle)' || cf.functionName === '(root)') continue;
    const file = (cf.url || '').split('/').pop() || '';
    const key = `${cf.functionName || '(anon)'} ${file}:${cf.lineNumber + 1}`;
    agg.set(key, (agg.get(key) || 0) + us);
  }
  return [...agg].sort((a, b) => b[1] - a[1]).slice(0, n).map(([fn, us]) => ({ fn, self_ms: r1(us / 1000) }));
}
const results = { meta: { size, url: 'mock backend, production-like unminified build', viewport: '1600x900', headless: !arg('headed', false), quick, chrome: await browser.version(), note: 'headless Chromium uses software rasterisation: absolute frame times are pessimistic for canvas work, relative comparisons are valid' }, scenarios: {} };

const only = arg('only', '') ? String(arg('only', '')).split(',') : null;
async function scenario(name, fn, opt = {}) {
  if (only && !only.includes(name)) { results.scenarios[name] = {}; return; }
  await page.waitForTimeout(opt.settle ?? 250);
  await page.evaluate(() => window.gc && window.gc());
  const m0 = await metrics();
  await page.evaluate(() => window.__m.start());
  await cdp.send('Profiler.start');
  const t0 = Date.now();
  let extra = null;
  try { extra = await fn(); } catch (e) { extra = { error: String(e).slice(0, 300) }; }
  const wall = Date.now() - t0;
  const { profile } = await cdp.send('Profiler.stop');
  const r = await page.evaluate(() => window.__m.stop());
  const m1 = await metrics();
  const d = k => (m1[k] - m0[k]) * 1000;
  const f = r.frames;
  const sc = {
    wall_ms: wall, frames: f.length, fps: r1(f.length / (wall / 1000)),
    frame_ms: { p50: r1(pct(f, 0.5)), p95: r1(pct(f, 0.95)), p99: r1(pct(f, 0.99)), max: r1(Math.max(0, ...f)) },
    jank_over_33ms: f.filter(x => x > 33.4).length, jank_over_50ms: f.filter(x => x > 50).length,
    long_tasks: { n: r.long.length, total_ms: r1(sum(r.long)), max_ms: r1(Math.max(0, ...r.long)) },
    main_thread: { script_ms: r1(d('ScriptDuration')), layout_ms: r1(d('LayoutDuration')), style_ms: r1(d('RecalcStyleDuration')), task_ms: r1(d('TaskDuration')), layouts: m1.LayoutCount - m0.LayoutCount, style_recalcs: m1.RecalcStyleCount - m0.RecalcStyleCount },
    busy_pct: r1(d('TaskDuration') / wall * 100),
    dom_mutations: r.mut, canvas_calls: r.canvas, canvas_full_paints: r.full, canvas_resizes: r.resets,
    dom_nodes: m1.Nodes, js_heap_mib: r1(m1.JSHeapUsedSize / 1048576),
    hot_functions: topSelf(profile),
  };
  if (extra) sc.extra = extra;
  results.scenarios[name] = sc;
  console.error(`[ui] ${name}: ${sc.fps} fps, p95 ${sc.frame_ms.p95} ms, long tasks ${sc.long_tasks.n}, busy ${sc.busy_pct}%`);
  return sc;
}
const P = (f, arg) => page.evaluate(f, arg);
const frames = n => P(n => new Promise(res => { let i = 0; const l = () => (++i >= n ? res() : requestAnimationFrame(l)); requestAnimationFrame(l); }), n);

// ---------- boot
const tNav = Date.now();
await page.goto(url, { waitUntil: 'load' });
await page.waitForSelector('canvas.gridCanvas', { timeout: 60000 });
await page.waitForFunction(() => window.__perf && window.__perf.S.ready && window.__waveMock, null, { timeout: 60000 });
const bootMs = Date.now() - tNav;
await frames(5);
const mb = await metrics();
const bootProf = await P(() => { const p = performance.getEntriesByType('navigation')[0]; return { dcl: p.domContentLoadedEventEnd, load: p.loadEventEnd }; });
results.boot = { ms_to_grid_canvas: bootMs, nav: bootProf, dom_nodes: mb.Nodes, js_heap_mib: r1(mb.JSHeapUsedSize / 1048576), tracks: project.tracks.length, clips: project.clips.length, project_json_kib: Math.round(JSON.stringify(project).length / 1024) };
console.error('[ui] boot', JSON.stringify(results.boot));

// mock-IPC counters (what the real app would move over Tauri)
await P(() => {
  const m = window.__waveMock, ipc = window.__ipc = { applyOps: 0, applyBytes: 0, stateEvents: 0, teleFrames: 0, teleBytes: 0, stateKiB: Math.round(JSON.stringify(m.dto()).length / 1024) };
  const ao = m.applyOps.bind(m); m.applyOps = (ops, o) => { ipc.applyOps++; ipc.applyBytes += JSON.stringify(ops).length; return ao(ops, o); };
  const em = m.emit.bind(m); m.emit = (ev, p) => { if (ev === 'state') ipc.stateEvents++; return em(ev, p); };
  const tc = m.teleCb; m.teleCb = b => { ipc.teleFrames++; ipc.teleBytes += b.byteLength; return tc(b); };
});
const ipcSnap = () => P(() => ({ ...window.__ipc }));
const ipcDiff = (a, b) => Object.fromEntries(Object.keys(b).map(k => [k, b[k] - a[k]]));

// ---------- scenarios
const D = quick ? 1500 : 3000;
await scenario('idle', () => page.waitForTimeout(D));

await scenario('scroll_horizontal', () => P(ms => new Promise(res => { const el = window.__perf.geom.scroll; const t0 = performance.now(); el.scrollLeft = 0; const l = () => { const t = performance.now() - t0; el.scrollLeft = (t / ms) * (el.scrollWidth - el.clientWidth) * 0.9; t < ms ? requestAnimationFrame(l) : res({ scrollWidth: el.scrollWidth }); }; requestAnimationFrame(l); }), D));
await scenario('scroll_vertical', () => P(ms => new Promise(res => { const el = window.__perf.geom.scroll; const t0 = performance.now(); el.scrollTop = 0; const l = () => { const t = performance.now() - t0; el.scrollTop = (t / ms) * (el.scrollHeight - el.clientHeight); t < ms ? requestAnimationFrame(l) : res({ scrollHeight: el.scrollHeight }); }; requestAnimationFrame(l); }), D));
await P(() => { const el = window.__perf.geom.scroll; el.scrollLeft = 0; el.scrollTop = 0; });

await scenario('zoom_ctrl_wheel', () => P(n => new Promise(res => { const el = window.__perf.geom.scroll, r = el.getBoundingClientRect(); let i = 0; const l = () => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: i < n / 2 ? -100 : 100, ctrlKey: true, clientX: r.left + 600, clientY: r.top + 300, bubbles: true, cancelable: true })); ++i < n ? requestAnimationFrame(l) : res(); }; requestAnimationFrame(l); }), quick ? 40 : 90));

// clip drag with the real mouse
const pos = await P(() => {
  const { S, geom, U } = window.__perf;
  const c = S.project.clips.find(c => c.kind === 'audio' && c.track >= 8 && c.len * U.bw > 80) || S.project.clips[0];
  geom.scroll.scrollLeft = Math.max(0, c.start * U.bw - 150); geom.scroll.scrollTop = Math.max(0, geom.tops[c.track] - 60);
  return c.id;
});
await frames(4);
const pt = await P(id => {
  const { S, geom, U } = window.__perf; const c = S.project.clips.find(x => x.id === id), r = geom.grid.getBoundingClientRect();
  return { x: r.left + c.start * U.bw + 30, y: r.top + geom.tops[c.track] + 14 };
}, pos);
const ipc0 = await ipcSnap();
await scenario('clip_drag_mouse', async () => {
  await page.mouse.move(pt.x, pt.y); await page.mouse.down();
  const N = quick ? 40 : 100;
  for (let i = 1; i <= N; i++) await page.mouse.move(pt.x + i * 5, pt.y + Math.sin(i / 8) * 20);
  await page.mouse.up(); await page.waitForTimeout(300);
  return { moves: N };
});
const ipcDrag = ipcDiff(ipc0, await ipcSnap());
results.scenarios.clip_drag_mouse.ipc = ipcDrag;
await P(() => window.__perf.A.undo && window.__perf.A.undo());

// drawing a loop range on the ruler, then a Ctrl+drag marquee over the clips
// the ruler row is sticky at the top of the scroller, right of the lane headers
const rul = await P(() => { const { geom, U } = window.__perf, r = geom.scroll.getBoundingClientRect(); return { x: r.left + U.headW, y: r.top + 13 }; });
await scenario('loop_range_drag', async () => {
  const x0 = rul.x + 200; await page.mouse.move(x0, rul.y); await page.mouse.down();
  const N = quick ? 40 : 100;
  for (let i = 1; i <= N; i++) await page.mouse.move(x0 + i * 6, rul.y);
  await page.mouse.up(); await page.waitForTimeout(300);
  return { moves: N, loop: await P(() => window.__perf.S.project.loop) };
});
await P(() => window.__perf.A.undo && window.__perf.A.undo());
await scenario('marquee_select', async () => {
  await page.keyboard.down('Control');
  await page.mouse.move(pt.x - 40, pt.y - 40); await page.mouse.down();
  const N = quick ? 40 : 100;
  for (let i = 1; i <= N; i++) await page.mouse.move(pt.x - 40 + i * 12, pt.y - 40 + i * 6);
  await page.mouse.up(); await page.keyboard.up('Control'); await page.waitForTimeout(300);
  return { moves: N, selected: await P(() => window.__perf.U.sel.length) };
});
await P(() => window.__perf.setU('sel', []));

// per-op reactive cost, synchronous parts, in page (scales with project size)
results.op_cost = await P(async () => {
  const { S, E, A, applyLocal, replaceState } = window.__perf;
  const time = (n, f) => { const t = []; for (let i = 0; i < n; i++) { const a = performance.now(); f(i); t.push(performance.now() - a); } t.sort((x, y) => x - y); return { p50: +t[t.length >> 1].toFixed(3), max: +t[t.length - 1].toFixed(3) }; };
  const tick = () => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
  const out = {};
  const c0 = S.project.clips.find(c => c.kind === 'audio');
  const tr = S.project.tracks[3];
  const fx = S.project.inserts.find(m => m.fx.some(f => f.type === 'comp'))?.fx.find(f => f.type === 'comp');
  out.applyLocal_move_clip_ms = time(30, i => applyLocal([{ op: 'putClips', clips: [{ ...JSON.parse(JSON.stringify(c0)), start: 10 + (i % 5) }] }]));
  await tick();
  out.applyLocal_track_vol_ms = time(30, i => applyLocal([{ op: 'putTrack', track: { ...JSON.parse(JSON.stringify(tr)), vol: 0.5 + (i % 10) * 0.01 } }]));
  await tick();
  if (fx) { out.applyLocal_fx_param_ms = time(30, i => applyLocal([{ op: 'putFx', insertId: 0, fx: { ...JSON.parse(JSON.stringify(fx)), params: { ...fx.params, thr: -30 + (i % 10) } } }])); await tick(); }
  out.commit_move_clip_sync_ms = time(20, i => E.commit([{ op: 'putClips', clips: [{ ...JSON.parse(JSON.stringify(c0)), start: 12 + (i % 5) }] }], 'perf'));
  await new Promise(r => setTimeout(r, 400)); await tick();
  // the "state" event: parse + reconcile (what every committed op, undo and CLI change costs the UI)
  const dto = { project: JSON.parse(JSON.stringify(S.project)), revision: S.revision + 1000, canUndo: true, canRedo: false, transport: JSON.parse(JSON.stringify(S.transport)), path: null, dirty: true, audition: {} };
  const text = JSON.stringify(dto);
  out.state_payload_kib = Math.round(text.length / 1024);
  out.json_parse_state_ms = time(10, () => JSON.parse(text));
  out.structuredClone_state_ms = time(10, () => structuredClone(dto));
  out.replaceState_unchanged_ms = time(10, i => { dto.revision += 1; replaceState(dto); });
  await tick();
  out.replaceState_one_clip_changed_ms = time(10, i => { dto.revision += 1; dto.project.clips[5].start = 20 + i; replaceState(dto); });
  await tick();
  // time from a state change to the next painted frame (effects + canvas redraw)
  const lat = [];
  for (let i = 0; i < 10; i++) { const a = performance.now(); applyLocal([{ op: 'putClips', clips: [{ ...JSON.parse(JSON.stringify(c0)), start: 15 + i * 0.25 }] }]); await tick(); lat.push(performance.now() - a); }
  lat.sort((x, y) => x - y); out.edit_to_second_frame_ms = { p50: +lat[5].toFixed(1), max: +lat[9].toFixed(1) };
  return out;
});
console.error('[ui] op_cost', JSON.stringify(results.op_cost));

// mixer
await scenario('mixer_toggle', () => P(async () => { const { setU, U } = window.__perf; const lat = []; for (let i = 0; i < 6; i++) { const a = performance.now(); setU('mixOpen', !U.mixOpen); await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); lat.push(+(performance.now() - a).toFixed(1)); } if (!U.mixOpen) setU('mixOpen', true); return { toggle_to_frame_ms: lat, strips: document.querySelectorAll('.strip').length }; }));
await P(() => window.__perf.setU('mixOpen', true)); await frames(5);
const mixInfo = await P(() => { const m = document.querySelector('.mixer'); return { mixerNodes: m ? m.querySelectorAll('*').length : -1, mixH: window.__perf.U.mixH }; });
await scenario('mixer_resize', () => P(n => new Promise(res => { const { setU } = window.__perf; let i = 0; const l = () => { setU('mixH', 200 + Math.round(150 * (1 + Math.sin(i / 8)))); ++i < n ? requestAnimationFrame(l) : res(); }; requestAnimationFrame(l); }), quick ? 40 : 120));
results.scenarios.mixer_resize.extra = mixInfo;
await P(() => window.__perf.setU('mixH', 300));
// mixer scroll horizontally (many strips)
await scenario('mixer_scroll', () => P(ms => new Promise(res => { const sc = document.querySelector('.mixer .strips'); if (!sc) return res({ error: 'no mixer scroller' }); const t0 = performance.now(); const l = () => { const t = performance.now() - t0; sc.scrollLeft = (t / ms) * (sc.scrollWidth - sc.clientWidth); t < ms ? requestAnimationFrame(l) : res({ scrollWidth: sc.scrollWidth }); }; requestAnimationFrame(l); }), D));

// playback with meters (mixer open: 64 strips)
const ipc1 = await ipcSnap();
await scenario('playback_meters_mixer_open', async () => { await P(() => { window.__perf.A.seek(40); return window.__perf.A.togglePlay(); }); await page.waitForTimeout(D + 1000); await P(() => window.__perf.A.togglePlay()); }, { settle: 300 });
results.scenarios.playback_meters_mixer_open.ipc = ipcDiff(ipc1, await ipcSnap());
await P(() => window.__perf.setU('mixOpen', false)); await frames(5);
await scenario('playback_mixer_closed', async () => { await P(() => { window.__perf.A.seek(40); return window.__perf.A.togglePlay(); }); await page.waitForTimeout(D + 500); await P(() => window.__perf.A.togglePlay()); });
// the mixer stays closed for the effect-window and piano-roll scenarios so their numbers are their own

// FX windows: open 1 / 4 / 8 and play
const fxIds = await P(() => { const { S } = window.__perf; const ids = []; for (const t of S.project.tracks.slice(0, 8)) { const m = S.project.inserts.find(i => i.id === t.insert); for (const f of m.fx) if (f.type !== 'vst3') ids.push({ id: f.id, type: f.type }); } return ids; });
const pick = (types, n) => { const out = []; for (const t of types) for (const f of fxIds) if (f.type === t && !out.includes(f.id) && out.length < n) { out.push(f.id); break; } return out; };
const open = ids => P(async ids => { for (const id of ids) { window.__perf.A.openPlug(id); await new Promise(r => requestAnimationFrame(r)); } }, ids);
const winSteps = [['fxwin_open_1_eq', ['eq'], 1], ['fxwin_open_4', ['comp', 'tune', 'reverb'], 3], ['fxwin_open_8', ['denoise', 'fet', 'deess', 'sat'], 4]];
for (const [name, types, n] of winSteps) {
  const ids = pick(types, n);
  if (name === 'fxwin_open_1_eq') ids.length = 0, ids.push(...pick(['eq'], 1));
  const ipcA = await ipcSnap();
  await scenario(name, async () => {
    const t0 = await P(() => performance.now()); await open(ids); await frames(2);
    const openMs = (await P(() => performance.now())) - t0;
    await P(() => { window.__perf.A.seek(40); return window.__perf.A.togglePlay(); }); await page.waitForTimeout(D); await P(() => window.__perf.A.togglePlay());
    return { opened: ids.length, open_ms_incl_2_frames: r1(openMs), windows: await P(() => window.__perf.U.plugs.length) };
  });
  results.scenarios[name].ipc = ipcDiff(ipcA, await ipcSnap());
}
await P(() => { const { U, A } = window.__perf; for (const p of [...U.plugs]) A.closePlug(p.fxId); });
await frames(3);

// piano roll on the 2000-note pattern
const stress = await P(() => { const { S } = window.__perf; const p = S.project.patterns.find(x => x.name.startsWith('Piano roll stress')); const ch = p.notes[0].ch; return { id: p.id, ch, notes: p.notes.length }; });
await scenario('pianoroll_open', async () => { const a = await P(() => performance.now()); await P(s => { window.__perf.P.openPattern(s.id, 'roll', s.ch); }, stress); await frames(3); return { open_ms: r1((await P(() => performance.now())) - a), notes: stress.notes }; });
const roll = await P(() => { const cs = [...document.querySelectorAll('.win canvas')].filter(c => c.getBoundingClientRect().width > 400); const c = cs[0]; if (!c) return null; const r = c.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height, n: cs.length }; });
results.pianoroll_canvas = roll;
if (roll) {
  await scenario('pianoroll_wheel_scroll_zoom', () => P(n => new Promise(res => { const cs = [...document.querySelectorAll('.win canvas')].filter(c => c.getBoundingClientRect().width > 400); const c = cs[0], r = c.getBoundingClientRect(); let i = 0; const l = () => { c.dispatchEvent(new WheelEvent('wheel', { deltaY: i % 4 < 2 ? 100 : -100, shiftKey: i % 8 > 3, ctrlKey: i % 16 > 7, clientX: r.left + 300, clientY: r.top + 200, bubbles: true, cancelable: true })); ++i < n ? requestAnimationFrame(l) : res(); }; requestAnimationFrame(l); }), quick ? 40 : 100));
  const ipcR = await ipcSnap();
  await scenario('pianoroll_add_move_notes_mouse', async () => {
    // draw notes with clicks, then drag a note
    for (let i = 0; i < (quick ? 6 : 14); i++) { await page.mouse.click(roll.x + 120 + i * 37, roll.y + 60 + (i % 5) * 18); await page.waitForTimeout(40); }
    await page.mouse.move(roll.x + 120, roll.y + 60); await page.mouse.down();
    for (let i = 1; i <= (quick ? 20 : 50); i++) await page.mouse.move(roll.x + 120 + i * 4, roll.y + 60 + Math.round(i / 6) * 4);
    await page.mouse.up(); await page.waitForTimeout(300);
  });
  results.scenarios.pianoroll_add_move_notes_mouse.ipc = ipcDiff(ipcR, await ipcSnap());
  // reactive cost of one edit of the 2000-note pattern
  results.pianoroll_op_cost = await P(async s => {
    const { S, E } = window.__perf; const tick = () => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
    const p = S.project.patterns.find(x => x.id === s.id); const res = [];
    for (let i = 0; i < 10; i++) { const pat = JSON.parse(JSON.stringify(p)); pat.notes[0].step = i; const a = performance.now(); E.commit([{ op: 'putPattern', pattern: pat }], 'perf'); const b = performance.now(); await tick(); res.push([+(b - a).toFixed(2), +(performance.now() - a).toFixed(1)]); }
    return { notes: p.notes.length, commit_sync_ms: res.map(r => r[0]), commit_to_second_frame_ms: res.map(r => r[1]), op_payload_kib: Math.round(JSON.stringify({ op: 'putPattern', pattern: p }).length / 1024) };
  }, stress);
  console.error('[ui] pianoroll_op_cost', JSON.stringify(results.pianoroll_op_cost));
}
await P(() => window.__perf.P.closePattern());

results.console_errors = consoleErrors.slice(0, 10);
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, JSON.stringify(results, null, 2));
console.error('[ui] wrote', out);
await browser.close(); server.close();
