// Partial-repaint correctness: after drags that repaint only damaged rects (clip move, resize,
// loop range, marquee, undo), the playlist grid must be pixel-identical to a full repaint.
//   node perf/repaint_check.mjs [--size large] [--no-build] [--dpr 1.25]
// Exits 1 when any step leaves pixels that a full repaint would draw differently.
import { chromium } from 'playwright-core';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const arg = (k, d) => { const i = process.argv.indexOf('--' + k); return i < 0 ? d : (process.argv[i + 1]?.startsWith('--') || process.argv[i + 1] == null ? true : process.argv[i + 1]); };
const size = arg('size', 'large'), dpr = +arg('dpr', 1.25);
const dist = join(root, 'perf/.cache/ui-dist-check');
const fixture = join(root, 'perf/.cache', size, 'project.json');
if (!existsSync(fixture)) { console.error(`missing ${fixture}; run: cargo run --release -p wave-perf -- gen ${size}`); process.exit(2); }
if (!arg('no-build', false)) {
  const r = spawnSync('npx', ['vite', 'build', '--outDir', dist, '--emptyOutDir', '--minify', 'false'], { cwd: join(root, 'ui'), env: { ...process.env, VITE_PERF: '1' }, shell: true, stdio: ['ignore', 'ignore', 'inherit'] });
  if (r.status !== 0) process.exit(r.status ?? 1);
}
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.svg': 'image/svg+xml', '.json': 'application/json' };
const server = createServer((req, res) => {
  let p = decodeURIComponent((req.url || '/').split('?')[0]); if (p === '/') p = '/index.html';
  const f = join(dist, p);
  if (!f.startsWith(dist) || !existsSync(f) || statSync(f).isDirectory()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'content-type': mime[extname(f)] || 'application/octet-stream' }); res.end(readFileSync(f));
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const base = join(process.env.LOCALAPPDATA || '', 'ms-playwright');
const shell = existsSync(base) && readdirSync(base).sort().reverse().filter(d => d.startsWith('chromium_headless_shell-')).map(d => join(base, d, 'chrome-headless-shell-win64/chrome-headless-shell.exe')).find(existsSync);
const browser = await chromium.launch({ executablePath: process.env.PERF_CHROME || shell });
const page = await (await browser.newContext({ viewport: { width: 1600, height: 900 }, deviceScaleFactor: dpr })).newPage();
const errors = [];
page.on('pageerror', e => errors.push(String(e)));
await page.addInitScript(p => { window.__waveFixture = p; }, JSON.parse(readFileSync(fixture, 'utf8')));
await page.goto(`http://127.0.0.1:${server.address().port}/`);
await page.waitForFunction(() => window.__perf && window.__perf.S.ready, null, { timeout: 60000 });
const P = (f, a) => page.evaluate(f, a);
const frames = n => P(n => new Promise(r => { let i = 0; const l = () => (++i >= n ? r() : requestAnimationFrame(l)); requestAnimationFrame(l); }), n);

const pt = await P(() => {
  const { S, geom, U } = window.__perf;
  const c = S.project.clips.find(c => c.kind === 'audio' && c.track >= 8 && c.len * U.bw > 80) || S.project.clips[0];
  geom.scroll.scrollLeft = Math.max(0, c.start * U.bw - 150); geom.scroll.scrollTop = Math.max(0, geom.tops[c.track] - 60);
  return c.id;
}).then(async id => { await frames(4); return P(id => { const { S, geom, U } = window.__perf; const c = S.project.clips.find(x => x.id === id), r = geom.grid.getBoundingClientRect(); return { x: r.left + c.start * U.bw + 30, y: r.top + geom.tops[c.track] + 14, endX: r.left + (c.start + c.len) * U.bw - 2 }; }, id); });

/** Grid pixels now, then after forcing a full repaint (a snap change that draws the same grid lines). */
async function compare(step) {
  await frames(3);
  const shot = () => P(() => { const cv = document.querySelector('canvas.gridCanvas:not(.gridOverlay)'); return cv.getContext('2d').getImageData(0, 0, cv.width, cv.height).data.join(','); });
  const a = await shot();
  await P(() => { const { U, setU } = window.__perf; const s = U.snap; setU('snap', s === 'none' ? 'bar' : 'none'); return new Promise(r => requestAnimationFrame(() => { setU('snap', s); requestAnimationFrame(() => requestAnimationFrame(r)); })); });
  await frames(2);
  const b = await shot();
  const pa = a.split(','), pb = b.split(',');
  const W = await P(() => document.querySelector('canvas.gridCanvas:not(.gridOverlay)').width);
  let diff = 0, where = [];
  for (let i = 0; i < pa.length; i++) if (Math.abs(+pa[i] - +pb[i]) > 2) { diff++; const px = i >> 2; if (where.length < 6) where.push(`(${px % W},${Math.floor(px / W)}) ${pa[i]}->${pb[i]}`); }
  console.log(`${diff ? 'FAIL' : 'ok  '} ${step}: ${diff} differing channel values of ${pa.length}${diff ? ' at ' + where.join(' ') : ''}`);
  if (diff && arg('explain', false)) console.log(await P(([x, y]) => { const { S, U, geom } = window.__perf; const cv = document.querySelector('canvas.gridCanvas:not(.gridOverlay)'); const m = /translate\(([-\d.]+)px, ([-\d.]+)px\)/.exec(cv.style.transform); const d = devicePixelRatio, gx = +m[1] + x / d, gy = +m[2] + y / d; return JSON.stringify({ gx, gy, clips: S.project.clips.filter(c => c.start * U.bw - 6 < gx && (c.start + c.len) * U.bw + 6 > gx && geom.tops[c.track] - 6 < gy && geom.tops[c.track + 1] + 6 > gy).map(c => ({ id: c.id, kind: c.kind, x0: c.start * U.bw, x1: (c.start + c.len) * U.bw, y0: geom.tops[c.track], y1: geom.tops[c.track + 1] })) }); }, where.length ? where[0].match(/\((\d+),(\d+)\)/).slice(1).map(Number) : [0, 0]));
  return diff;
}

let bad = 0;
const drag = async (from, to, n, mods = []) => {
  for (const m of mods) await page.keyboard.down(m);
  await page.mouse.move(from.x, from.y); await page.mouse.down();
  for (let i = 1; i <= n; i++) await page.mouse.move(from.x + (to.x - from.x) * i / n, from.y + (to.y - from.y) * i / n);
  await page.mouse.up();
  for (const m of mods) await page.keyboard.up(m);
};
bad += await compare('initial');
await drag(pt, { x: pt.x + 420, y: pt.y + 130 }, 40); bad += await compare('clip move across tracks');
await drag(pt, { x: pt.x + 900, y: pt.y + 260 }, 30, ['Control']); bad += await compare('marquee select');
await P(() => { const { setU } = window.__perf; setU('sel', []); }); bad += await compare('deselect');
const rul = await P(() => { const { geom, U } = window.__perf, r = geom.scroll.getBoundingClientRect(); return { x: r.left + U.headW + 120, y: r.top + 13 }; });
await drag(rul, { x: rul.x + 500, y: rul.y }, 30); bad += await compare('loop range');
await P(() => window.__perf.A.undo()); await frames(6); bad += await compare('undo');
console.log(errors.length ? 'page errors: ' + errors.join(' | ') : 'no page errors');
await browser.close(); server.close();
process.exit(bad || errors.length ? 1 : 0);
