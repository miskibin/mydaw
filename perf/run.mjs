// One command for the whole performance harness:
//   node perf/run.mjs [--sizes small,medium,large] [--quick] [--skip-engine] [--skip-ui] [--ui-size large]
//                     [--update-baseline] [--baseline perf/baseline.json] [--label name]
// Builds, generates fixtures (perf/.cache, git-ignored), runs the engine benchmarks (wave-perf) and the UI
// profile (Playwright), writes perf/results/{engine,ui,summary}.json and summary.md, compares against
// perf/baseline.json and the absolute budgets in perf/metrics.mjs. Exit code 1 on a regression vs the baseline (or, with --strict,
// on any metric over its budget); --no-fail only reports.
import { spawnSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SPECS, evaluate, markdown } from './metrics.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const arg = (k, d) => { const i = process.argv.indexOf('--' + k); if (i < 0) return d; const v = process.argv[i + 1]; return v == null || v.startsWith('--') ? true : v; };
const sizes = String(arg('sizes', 'small,medium,large'));
const uiSize = String(arg('ui-size', sizes.includes('large') ? 'large' : sizes.split(',').pop()));
const quick = !!arg('quick', false);
const resultsDir = join(root, 'perf/results'); mkdirSync(resultsDir, { recursive: true });
const cache = join(root, 'perf/.cache');
const exe = join(root, 'target/release', process.platform === 'win32' ? 'wave-perf.exe' : 'wave-perf');

function run(cmd, args, opts = {}) {
  console.error(`$ ${cmd} ${args.join(' ')}`);
  return new Promise((ok, fail) => {
    const p = spawn(cmd, args, { cwd: root, stdio: ['ignore', 'inherit', 'inherit'], shell: opts.shell ?? false, env: { ...process.env, ...(opts.env || {}) } });
    try { os.setPriority(p.pid, os.constants.priority.PRIORITY_BELOW_NORMAL); } catch {}
    p.on('exit', c => (c === 0 ? ok() : fail(new Error(`${cmd} exited ${c}`))));
    p.on('error', fail);
  });
}

if (!arg('skip-engine', false)) {
  await run('cargo', ['build', '--release', '-p', 'wave-perf', '-j', '4']);
  for (const s of new Set([...sizes.split(','), uiSize])) {
    if (!existsSync(join(cache, s, `${s}.wave`))) await run(exe, ['gen', s, '--cache', cache]);
  }
  await run(exe, ['bench', '--sizes', sizes, '--cache', cache, '--out', join(resultsDir, 'engine.json'), ...(quick ? ['--quick'] : [])]);
}
if (arg('vst3', false)) {
  // optional: hosted VST3 effects. Plugins on the owner's block list (licence dialogs) are not scanned.
  await run('cargo', ['build', '--release', '-p', 'wave-perf', '--features', 'vst3', '--bin', 'wave-perf-vst3', '-j', '4']);
  const r = spawnSync(join(root, 'target/release', process.platform === 'win32' ? 'wave-perf-vst3.exe' : 'wave-perf-vst3'), ['--max', '3'], { encoding: 'utf8', timeout: 180000 });
  if (r.stdout) writeFileSync(join(resultsDir, 'vst3.json'), r.stdout);
}
if (!arg('skip-ui', false)) {
  if (!existsSync(join(cache, uiSize, 'project.json'))) await run(exe, ['gen', uiSize, '--cache', cache]);
  if (!existsSync(join(here, 'node_modules'))) await run('npm', ['install', '--no-audit', '--no-fund'], { shell: true }).catch(() => {});
  await run('node', [join(here, 'ui_bench.mjs'), '--size', uiSize, '--out', join(resultsDir, 'ui.json'), ...(quick ? ['--quick'] : [])]);
}

const load = f => (existsSync(f) ? JSON.parse(readFileSync(f, 'utf8')) : null);
const engine = load(join(resultsDir, 'engine.json')), ui = load(join(resultsDir, 'ui.json'));
const baselineFile = resolve(root, String(arg('baseline', 'perf/baseline.json')));
const baseline = load(baselineFile);
const rows = evaluate({ engine, ui, uiSize }, baseline?.metrics);
const summary = {
  label: String(arg('label', 'run')), date: new Date().toISOString(), host: { cpus: os.cpus().length, cpu: os.cpus()[0]?.model, platform: os.platform(), totalMemGiB: Math.round(os.totalmem() / 2 ** 30) },
  quick, sizes, uiSize, engine_meta: engine?.meta, ui_meta: ui?.meta, metrics: Object.fromEntries(rows.map(r => [r.key, r.value])),
  over: rows.filter(r => r.status === 'OVER').map(r => r.key), regressions: rows.filter(r => r.status === 'REGRESSION').map(r => r.key),
};
writeFileSync(join(resultsDir, 'summary.json'), JSON.stringify(summary, null, 2));
writeFileSync(join(resultsDir, 'summary.md'), markdown(rows, summary, !!baseline));
if (arg('update-baseline', false)) { writeFileSync(baselineFile, JSON.stringify({ date: summary.date, host: summary.host, quick, sizes, uiSize, metrics: summary.metrics }, null, 2)); writeFileSync(baselineFile.replace(/\.json$/, '.md'), markdown(rows, summary, false)); console.error('baseline written', baselineFile); }
console.error(`\nsummary: ${join(resultsDir, 'summary.md')}  (${rows.length} metrics, ${summary.over.length} over budget, ${summary.regressions.length} regressions vs baseline)`);
if (!arg('no-fail', false) && (summary.regressions.length || (arg('strict', false) && summary.over.length))) process.exit(1);
