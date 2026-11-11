// Print the hot functions of a ui_bench result: node perf/hot.mjs perf/results/ui.json [scenario,...]
import { readFileSync } from 'node:fs';
const r = JSON.parse(readFileSync(process.argv[2], 'utf8')), only = process.argv[3]?.split(',');
for (const [k, s] of Object.entries(r.scenarios)) {
  if (!s.frame_ms || (only && !only.includes(k))) continue;
  console.log(`\n== ${k}: ${s.fps} fps p95 ${s.frame_ms.p95} busy ${s.busy_pct}% script ${s.main_thread.script_ms} layout ${s.main_thread.layout_ms} style ${s.main_thread.style_ms} task ${s.main_thread.task_ms} mut ${s.dom_mutations} resizes ${s.canvas_resizes}`);
  console.log('  canvas', JSON.stringify(s.canvas_calls), 'full', JSON.stringify(s.canvas_full_paints), s.extra ? JSON.stringify(s.extra) : '');
  for (const h of s.hot_functions.slice(0, 9)) console.log('   ', h.self_ms, h.fn);
}
