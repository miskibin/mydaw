// Metric catalogue of the harness: where each number comes from, its unit, which direction is good,
// an absolute budget (the product goal) and how much worse than the baseline counts as a regression.
//
// To track a new number, add one line to `engineSpecs` / `uiSpecs`. A budget is a hard limit that should
// hold on any machine ("no audio block may miss its deadline"); the baseline tolerance catches slow drifts.

const get = (o, path) => path.split('.').reduce((a, k) => (a == null ? undefined : a[k]), o);

/** [key, path, unit, good, budget, desc]; good 'low' = smaller is better. budget null = report only. */
function engineSpecs(sizes) {
  const S = [];
  const add = (key, path, unit, good, budget, desc, fn) => S.push({ key, path, unit, good, budget, desc, fn });
  for (const s of sizes) {
    add(`rt.${s}.cpu_load_p99_pct`, `rt_project.${s}.from_middle_512.cpu_load_p99_pct`, '%', 'low', 60, `audio callback load (p99 block / 10.7 ms), ${s} project, 512 frames`);
    add(`rt.${s}.cpu_load_mean_pct`, `rt_project.${s}.from_middle_512.cpu_load_mean_pct`, '%', 'low', 50, `mean audio-thread load while playing, ${s}`);
    add(`rt.${s}.idle_cpu_load_mean_pct`, `rt_project.${s}.stopped_idle_512.cpu_load_mean_pct`, '%', 'low', 10, `audio-thread load with the transport STOPPED (effects on silence), ${s}`);
    add(`rt.${s}.tails_cpu_load_mean_pct`, `rt_project.${s}.tails_after_stop_512.cpu_load_mean_pct`, '%', 'low', 50, `mean audio-thread load in the seconds after Stop (effect tails decaying), ${s}`);
    add(`rt.${s}.tails_cpu_load_p99_pct`, `rt_project.${s}.tails_after_stop_512.cpu_load_p99_pct`, '%', 'low', 60, `p99 block in the seconds after Stop, ${s}`);
    add(`rt.${s}.cpu_load_max_pct`, `rt_project.${s}.from_middle_512.cpu_load_max_pct`, '%', 'low', 100, `worst block vs its deadline, ${s}`);
    add(`rt.${s}.late_blocks`, `rt_project.${s}.from_middle_512.late_blocks`, 'blocks', 'low', 0, `blocks that missed the deadline, ${s}`);
    add(`rt.${s}.audio_allocs`, `rt_project.${s}.from_middle_512.audio_allocs`, 'allocs', 'low', 0, `heap allocations on the audio thread, ${s}`);
    add(`rt.${s}.block64_cpu_load_p99_pct`, `rt_project.${s}.block_64.cpu_load_p99_pct`, '%', 'low', 80, `p99 load at 64-frame blocks, ${s}`);
    add(`ops.${s}.drag_clip_ms`, `ops.${s}.drag_clip_transient_ms.p50`, 'ms', 'low', 4, `engine apply of one clip-drag step, ${s}`);
    add(`ops.${s}.note_drag_ms`, `ops.${s}.note_drag_transient_ms.p50`, 'ms', 'low', 4, `engine apply of one piano-roll drag step (whole pattern, from JSON), ${s}`);
    add(`ops.${s}.fader_drag_ms`, `ops.${s}.fader_drag_transient_ms.p50`, 'ms', 'low', 4, `engine apply of one fader step, ${s}`);
    add(`ops.${s}.fader_drag_with_state_event_ms`, `ops.${s}.fader_drag_with_state_event_ms.p50`, 'ms', 'low', 4, `same, with the whole-project state event serialised, ${s}`);
    add(`ops.${s}.move_clip_ms`, `ops.${s}.move_clip_ms.p50`, 'ms', 'low', 16, `engine apply of a committed clip move, ${s}`);
    add(`ops.${s}.add_fx_ms`, `ops.${s}.add_fx_graph_rebuild_ms.p95`, 'ms', 'low', 50, `adding an effect (graph rebuild), p95, ${s}`);
    add(`ops.${s}.undo_ms`, `ops.${s}.undo_ms.p95`, 'ms', 'low', 50, `undo, p95, ${s}`);
    add(`ops.${s}.state_event_kib`, `ops.${s}.state_event_kib`, 'KiB', 'low', null, `payload of the state event (whole project), ${s}`);
    add(`ops.${s}.audio_thread_late_blocks`, `ops.${s}.audio_thread_during_ops.late_blocks`, 'blocks', 'low', 0, `audio blocks late while edits are applied, ${s}`);
    add(`io.${s}.open_rss_delta_mib`, `io.${s}.open_rss_delta_mib`, 'MiB', 'low', null, `memory added by opening the project, ${s}`);
    add(`io.${s}.open_ms`, `io.${s}.open_ms_with_device`, 'ms', 'low', null, `open a project with a device attached, ${s}`);
    add(`io.${s}.save_ms`, `io.${s}.save_ms`, 'ms', 'low', null, `save, ${s}`);
    add(`io.${s}.state_json_ms`, `io.${s}.state_json_ms`, 'ms', 'low', null, `serialise the state event, ${s}`);
    add(`export.${s}.rtf`, `export.${s}.rtf`, 'x realtime', 'high', 5, `offline export speed, ${s}`);
  }
  for (const k of ['eq', 'comp', 'fet', 'deess', 'sat', 'delay', 'reverb', 'limit', 'tune', 'denoise', 'voice']) {
    add(`fx.${k}.cpu_pct_per_instance`, `rt_fx.per_kind.${k}.cpu_pct_per_instance`, '%', 'low', null, `audio CPU (% of one core at 512/48k) per ${k} instance`);
  }
  add('fx.audio_allocs_total', 'rt_fx.per_kind', 'allocs', 'low', 0, 'heap allocations on the audio thread over all effect kinds', o => Object.values(o ?? {}).reduce((a, v) => a + (v.audio_allocs ?? 0), 0));
  add('analysis.pitch_rtf', 'analysis.pitch_analysis.rtf', 'x realtime', 'high', null, 'pitch analysis speed (YIN)');
  add('analysis.tune_render_rtf', 'analysis.tune_render_apply_ms.rtf', 'x realtime', 'high', null, 'tune render + analysis speed on edit');
  add('analysis.source_peaks_ms', 'analysis.source_peaks_ms.ms', 'ms', 'low', null, 'source peaks request');
  add('analysis.decode_mib_s', 'analysis.decode_wav.mib_per_s', 'MiB/s', 'high', null, 'WAV decode speed');
  add('analysis.flatten_comp_ms', 'analysis.flatten_comp_ms.ms', 'ms', 'low', null, 'flatten comp');
  add('analysis.audio_late_blocks', 'analysis.audio_thread_during_analysis.late_blocks', 'blocks', 'low', 0, 'audio blocks late during analysis/tune render/import');
  add('edit.control_late_blocks', 'rt_under_edit.audio_thread_control_no_edits.late_blocks', 'blocks', 'low', 0, 'audio blocks late in playback with NO edits (paced, 512 frames), large project');
  add('edit.audio_late_blocks', 'rt_under_edit.audio_thread.late_blocks', 'blocks', 'low', 0, 'audio blocks late while editing during playback');
  add('edit.audio_max_load_pct', 'rt_under_edit.audio_thread.cpu_load_max_pct', '%', 'low', 100, 'worst audio block while editing during playback');
  add('edit.audio_allocs', 'rt_under_edit.audio_thread.audio_allocs', 'allocs', 'low', 0, 'audio-thread allocations while editing during playback');
  add('edit.apply_clip_move_p95_ms', 'rt_under_edit.apply_clip_move_while_playing_ms.p95', 'ms', 'low', 16, 'clip move apply while playing, p95');
  add('telemetry.kib_per_frame_8_windows', 'telemetry.8_fx_windows.kib_per_frame', 'KiB', 'low', null, 'telemetry frame size with 8 effect windows open');
  add('telemetry.mib_per_s_8_windows', 'telemetry.8_fx_windows.mib_per_s', 'MiB/s', 'low', null, 'telemetry bandwidth with 8 effect windows open');
  add('undo.mib_per_step', 'undo_memory.mib_per_step', 'MiB', 'low', null, 'memory per undo step (a full project copy), large project');
  add('midi.note_us_idle_p99', 'midi.note_event_us_idle.p99', 'us', 'low', 1000, 'midi_event latency, idle, p99');
  add('midi.note_us_editing_p95', 'midi.note_event_us_while_editing.p95', 'us', 'low', 2000, 'midi_event latency while the UI edits (core lock), p95');
  add('midi.note_us_editing_max', 'midi.note_event_us_while_editing.max', 'us', 'low', 5000, 'midi_event latency while the UI edits (core lock), max');
  return S;
}

function uiSpecs() {
  const S = [];
  const add = (key, path, unit, good, budget, desc, fn) => S.push({ key, path, unit, good, budget, desc, fn });
  for (const sc of ['idle', 'scroll_horizontal', 'scroll_vertical', 'zoom_ctrl_wheel', 'clip_drag_mouse', 'mixer_resize', 'mixer_scroll', 'playback_meters_mixer_open', 'playback_mixer_closed', 'fxwin_open_1_eq', 'fxwin_open_4', 'fxwin_open_8', 'pianoroll_open', 'pianoroll_wheel_scroll_zoom', 'pianoroll_add_move_notes_mouse', 'rack_open', 'rack_click_steps']) {
    add(`ui.${sc}.frame_p95_ms`, `scenarios.${sc}.frame_ms.p95`, 'ms', 'low', 34, `p95 frame interval, ${sc}`);
    add(`ui.${sc}.busy_pct`, `scenarios.${sc}.busy_pct`, '%', 'low', null, `main-thread busy share, ${sc}`);
    add(`ui.${sc}.long_task_ms`, `scenarios.${sc}.long_tasks.total_ms`, 'ms', 'low', null, `time in long tasks (>50 ms), ${sc}`);
  }
  for (const k of ['applyLocal_move_clip_ms', 'applyLocal_track_vol_ms', 'applyLocal_fx_param_ms', 'commit_move_clip_sync_ms', 'json_parse_state_ms', 'replaceState_unchanged_ms', 'replaceState_one_clip_changed_ms']) {
    add(`ui.op.${k}`, `op_cost.${k}.p50`, 'ms', 'low', k.startsWith('applyLocal') || k.startsWith('commit') ? 4 : null, `UI reactive cost: ${k}`);
  }
  add('ui.op.edit_to_second_frame_p50_ms', 'op_cost.edit_to_second_frame_ms.p50', 'ms', 'low', 50, 'local edit until the second following frame');
  add('ui.op.state_payload_kib', 'op_cost.state_payload_kib', 'KiB', 'low', null, 'state event payload');
  add('ui.boot_ms', 'boot.ms_to_grid_canvas', 'ms', 'low', null, 'page load until the playlist canvas exists (mock backend)');
  add('ui.dom_nodes', 'boot.dom_nodes', 'nodes', 'low', null, 'DOM nodes after boot');
  add('ui.pianoroll_commit_to_frame_p50_ms', 'pianoroll_op_cost.commit_to_second_frame_ms', 'ms', 'low', 50, 'putPattern commit of the 2000-note pattern until the 2nd frame', a => (a ? a.slice().sort((x, y) => x - y)[a.length >> 1] : undefined));
  return S;
}

function value(spec, root) {
  const raw = get(root, spec.path);
  const v = spec.fn ? spec.fn(raw) : raw;
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

export const SPECS = { engineSpecs, uiSpecs };

/** Relative tolerance before a number counts as regressed, plus an absolute floor so tiny numbers do not flap. */
const REL = 0.3;
const FLOOR = { '%': 3, ms: 0.3, us: 30, 'x realtime': 1, KiB: 8, 'MiB/s': 5, MiB: 5, nodes: 200, allocs: 0, blocks: 5 };

export function evaluate({ engine, ui, uiSize }, baseline) {
  const rows = [];
  const push = (specs, root) => {
    for (const s of specs) {
      const v = value(s, root);
      if (v === undefined) continue;
      const b = baseline?.[s.key];
      let status = 'OK';
      if (s.budget != null && (s.good === 'low' ? v > s.budget : v < s.budget)) status = 'OVER';
      let delta;
      if (b != null && b !== 0) delta = (v - b) / Math.abs(b);
      if (status === 'OK' && b != null) {
        // worst-case counters (max block, late blocks) are scheduler noise on a shared desktop: wide tolerance
        const rel = /(max_pct|late_blocks|_max$|_max_|audio_late)/.test(s.key) ? 1.5 : REL;
        const worse = s.good === 'low' ? v - b : b - v;
        if (worse > Math.max(Math.abs(b) * rel, FLOOR[s.unit] ?? 0)) status = 'REGRESSION';
        else if (-worse > Math.max(Math.abs(b) * rel, FLOOR[s.unit] ?? 0)) status = 'IMPROVED';
      }
      rows.push({ key: s.key, value: Math.round(v * 1000) / 1000, unit: s.unit, good: s.good, budget: s.budget, base: b, delta, status, desc: s.desc });
    }
  };
  if (engine) {
    const sizes = Object.keys(engine.rt_project ?? {});
    const specs = engineSpecs(sizes);
    push(specs, engine);
  }
  if (ui) push(uiSpecs(), ui);
  return rows;
}

export function markdown(rows, summary, hasBaseline) {
  const f = x => (x == null ? '' : Math.abs(x) >= 100 ? Math.round(x).toString() : String(Math.round(x * 100) / 100));
  const L = [];
  L.push(`# WAVE performance summary`, '', `Run ${summary.date} on ${summary.host.cpu ?? '?'} (${summary.host.cpus} threads, ${summary.host.totalMemGiB} GiB). Sizes: ${summary.sizes}; UI fixture: ${summary.uiSize}${summary.quick ? ' (quick)' : ''}.`, '');
  L.push(`Over budget (targets not met): **${summary.over.length}**, regressions vs baseline: **${hasBaseline ? summary.regressions.length : 'n/a (no baseline)'}**.`, '');
  const groups = new Map();
  for (const r of rows) { const g = r.key.split('.')[0] + (r.key.startsWith('ui.') ? '' : ''); (groups.get(g) ?? groups.set(g, []).get(g)).push(r); }
  for (const [g, rs] of groups) {
    L.push(`## ${g}`, '', '| metric | value | unit | budget | baseline | delta | status |', '|---|---:|---|---:|---:|---:|---|');
    for (const r of rs) L.push(`| ${r.key} | ${f(r.value)} | ${r.unit} | ${r.budget == null ? '' : (r.good === 'low' ? '<= ' : '>= ') + r.budget} | ${f(r.base)} | ${r.delta == null ? '' : (r.delta >= 0 ? '+' : '') + Math.round(r.delta * 100) + '%'} | ${r.status} |`);
    L.push('');
  }
  return L.join('\n');
}
