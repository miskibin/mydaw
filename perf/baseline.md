# WAVE performance summary

Run 2026-11-09T11:05:53.761Z on 12th Gen Intel(R) Core(TM) i5-12400F (12 threads, 32 GiB). Sizes: small,medium,large; UI fixture: large.

Over budget (targets not met): **37**, regressions vs baseline: **n/a (no baseline)**.

## rt

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| rt.large.cpu_load_p99_pct | 138 | % | <= 60 |  |  | OVER |
| rt.large.cpu_load_mean_pct | 72.31 | % | <= 50 |  |  | OVER |
| rt.large.idle_cpu_load_mean_pct | 50.17 | % | <= 10 |  |  | OVER |
| rt.large.cpu_load_max_pct | 205 | % | <= 100 |  |  | OVER |
| rt.large.late_blocks | 111 | blocks | <= 0 |  |  | OVER |
| rt.large.audio_allocs | 0 | allocs | <= 0 |  |  | OK |
| rt.large.block64_cpu_load_p99_pct | 181 | % | <= 80 |  |  | OVER |
| rt.medium.cpu_load_p99_pct | 85.92 | % | <= 60 |  |  | OVER |
| rt.medium.cpu_load_mean_pct | 55.41 | % | <= 50 |  |  | OVER |
| rt.medium.idle_cpu_load_mean_pct | 23.03 | % | <= 10 |  |  | OVER |
| rt.medium.cpu_load_max_pct | 120 | % | <= 100 |  |  | OVER |
| rt.medium.late_blocks | 1 | blocks | <= 0 |  |  | OVER |
| rt.medium.audio_allocs | 0 | allocs | <= 0 |  |  | OK |
| rt.medium.block64_cpu_load_p99_pct | 80.62 | % | <= 80 |  |  | OVER |
| rt.small.cpu_load_p99_pct | 23.02 | % | <= 60 |  |  | OK |
| rt.small.cpu_load_mean_pct | 13.32 | % | <= 50 |  |  | OK |
| rt.small.idle_cpu_load_mean_pct | 7.03 | % | <= 10 |  |  | OK |
| rt.small.cpu_load_max_pct | 331 | % | <= 100 |  |  | OVER |
| rt.small.late_blocks | 1 | blocks | <= 0 |  |  | OVER |
| rt.small.audio_allocs | 0 | allocs | <= 0 |  |  | OK |
| rt.small.block64_cpu_load_p99_pct | 173 | % | <= 80 |  |  | OVER |

## ops

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| ops.large.drag_clip_ms | 2.28 | ms | <= 4 |  |  | OK |
| ops.large.fader_drag_ms | 2.77 | ms | <= 4 |  |  | OK |
| ops.large.fader_drag_with_state_event_ms | 5.18 | ms | <= 4 |  |  | OVER |
| ops.large.move_clip_ms | 2.09 | ms | <= 16 |  |  | OK |
| ops.large.add_fx_ms | 10.92 | ms | <= 50 |  |  | OK |
| ops.large.undo_ms | 8.7 | ms | <= 50 |  |  | OK |
| ops.large.state_event_kib | 972 | KiB |  |  |  | OK |
| ops.large.audio_thread_late_blocks | 70 | blocks | <= 0 |  |  | OVER |
| ops.medium.drag_clip_ms | 0.64 | ms | <= 4 |  |  | OK |
| ops.medium.fader_drag_ms | 0.59 | ms | <= 4 |  |  | OK |
| ops.medium.fader_drag_with_state_event_ms | 1.84 | ms | <= 4 |  |  | OK |
| ops.medium.move_clip_ms | 0.81 | ms | <= 16 |  |  | OK |
| ops.medium.add_fx_ms | 3.54 | ms | <= 50 |  |  | OK |
| ops.medium.undo_ms | 3.84 | ms | <= 50 |  |  | OK |
| ops.medium.state_event_kib | 368 | KiB |  |  |  | OK |
| ops.medium.audio_thread_late_blocks | 8 | blocks | <= 0 |  |  | OVER |
| ops.small.drag_clip_ms | 0.19 | ms | <= 4 |  |  | OK |
| ops.small.fader_drag_ms | 0.19 | ms | <= 4 |  |  | OK |
| ops.small.fader_drag_with_state_event_ms | 0.32 | ms | <= 4 |  |  | OK |
| ops.small.move_clip_ms | 0.22 | ms | <= 16 |  |  | OK |
| ops.small.add_fx_ms | 1.27 | ms | <= 50 |  |  | OK |
| ops.small.undo_ms | 0.69 | ms | <= 50 |  |  | OK |
| ops.small.state_event_kib | 131 | KiB |  |  |  | OK |
| ops.small.audio_thread_late_blocks | 0 | blocks | <= 0 |  |  | OK |

## io

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| io.large.open_rss_delta_mib | 234 | MiB |  |  |  | OK |
| io.large.open_ms | 2601 | ms |  |  |  | OK |
| io.large.save_ms | 31.53 | ms |  |  |  | OK |
| io.large.state_json_ms | 3.23 | ms |  |  |  | OK |
| io.medium.open_rss_delta_mib | 82.77 | MiB |  |  |  | OK |
| io.medium.open_ms | 1006 | ms |  |  |  | OK |
| io.medium.save_ms | 14.07 | ms |  |  |  | OK |
| io.medium.state_json_ms | 1.41 | ms |  |  |  | OK |
| io.small.open_rss_delta_mib | 27.94 | MiB |  |  |  | OK |
| io.small.open_ms | 413 | ms |  |  |  | OK |
| io.small.save_ms | 16.11 | ms |  |  |  | OK |
| io.small.state_json_ms | 0.5 | ms |  |  |  | OK |

## export

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| export.large.rtf | 0.95 | x realtime | >= 5 |  |  | OVER |
| export.medium.rtf | 1.56 | x realtime | >= 5 |  |  | OVER |
| export.small.rtf | 7.12 | x realtime | >= 5 |  |  | OK |

## fx

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| fx.eq.cpu_pct_per_instance | 0.22 | % |  |  |  | OK |
| fx.comp.cpu_pct_per_instance | 0.13 | % |  |  |  | OK |
| fx.fet.cpu_pct_per_instance | 0.19 | % |  |  |  | OK |
| fx.deess.cpu_pct_per_instance | 0.3 | % |  |  |  | OK |
| fx.sat.cpu_pct_per_instance | 0.1 | % |  |  |  | OK |
| fx.delay.cpu_pct_per_instance | 0.09 | % |  |  |  | OK |
| fx.reverb.cpu_pct_per_instance | 0.28 | % |  |  |  | OK |
| fx.limit.cpu_pct_per_instance | 0.13 | % |  |  |  | OK |
| fx.tune.cpu_pct_per_instance | 2.16 | % |  |  |  | OK |
| fx.denoise.cpu_pct_per_instance | 0.54 | % |  |  |  | OK |
| fx.voice.cpu_pct_per_instance | 2.2 | % |  |  |  | OK |
| fx.audio_allocs_total | 0 | allocs | <= 0 |  |  | OK |

## analysis

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| analysis.pitch_rtf | 46.15 | x realtime |  |  |  | OK |
| analysis.tune_render_rtf | 126 | x realtime |  |  |  | OK |
| analysis.source_peaks_ms | 0.01 | ms |  |  |  | OK |
| analysis.decode_mib_s | 235 | MiB/s |  |  |  | OK |
| analysis.flatten_comp_ms | 25 | ms |  |  |  | OK |
| analysis.audio_late_blocks | 0 | blocks | <= 0 |  |  | OK |

## edit

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| edit.control_late_blocks | 53 | blocks | <= 0 |  |  | OVER |
| edit.audio_late_blocks | 37 | blocks | <= 0 |  |  | OVER |
| edit.audio_max_load_pct | 523 | % | <= 100 |  |  | OVER |
| edit.audio_allocs | 0 | allocs | <= 0 |  |  | OK |
| edit.apply_clip_move_p95_ms | 15.92 | ms | <= 16 |  |  | OK |

## telemetry

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| telemetry.kib_per_frame_8_windows | 25.94 | KiB |  |  |  | OK |
| telemetry.mib_per_s_8_windows | 1.51 | MiB/s |  |  |  | OK |

## undo

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| undo.mib_per_step | 0.37 | MiB |  |  |  | OK |

## midi

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| midi.note_us_idle_p99 | 16.3 | us | <= 1000 |  |  | OK |
| midi.note_us_editing_p95 | 7494 | us | <= 2000 |  |  | OVER |
| midi.note_us_editing_max | 53862 | us | <= 5000 |  |  | OVER |

## ui

| metric | value | unit | budget | baseline | delta | status |
|---|---:|---|---:|---:|---:|---|
| ui.idle.frame_p95_ms | 16.8 | ms | <= 34 |  |  | OK |
| ui.idle.busy_pct | 11.2 | % |  |  |  | OK |
| ui.idle.long_task_ms | 0 | ms |  |  |  | OK |
| ui.scroll_horizontal.frame_p95_ms | 50 | ms | <= 34 |  |  | OVER |
| ui.scroll_horizontal.busy_pct | 100 | % |  |  |  | OK |
| ui.scroll_horizontal.long_task_ms | 59 | ms |  |  |  | OK |
| ui.scroll_vertical.frame_p95_ms | 50.1 | ms | <= 34 |  |  | OVER |
| ui.scroll_vertical.busy_pct | 101 | % |  |  |  | OK |
| ui.scroll_vertical.long_task_ms | 208 | ms |  |  |  | OK |
| ui.zoom_ctrl_wheel.frame_p95_ms | 66.7 | ms | <= 34 |  |  | OVER |
| ui.zoom_ctrl_wheel.busy_pct | 72.4 | % |  |  |  | OK |
| ui.zoom_ctrl_wheel.long_task_ms | 974 | ms |  |  |  | OK |
| ui.clip_drag_mouse.frame_p95_ms | 33.4 | ms | <= 34 |  |  | OK |
| ui.clip_drag_mouse.busy_pct | 82.4 | % |  |  |  | OK |
| ui.clip_drag_mouse.long_task_ms | 601 | ms |  |  |  | OK |
| ui.mixer_resize.frame_p95_ms | 83.4 | ms | <= 34 |  |  | OVER |
| ui.mixer_resize.busy_pct | 103 | % |  |  |  | OK |
| ui.mixer_resize.long_task_ms | 7166 | ms |  |  |  | OK |
| ui.mixer_scroll.frame_p95_ms | 33.4 | ms | <= 34 |  |  | OK |
| ui.mixer_scroll.busy_pct | 105 | % |  |  |  | OK |
| ui.mixer_scroll.long_task_ms | 0 | ms |  |  |  | OK |
| ui.playback_meters_mixer_open.frame_p95_ms | 83.4 | ms | <= 34 |  |  | OVER |
| ui.playback_meters_mixer_open.busy_pct | 105 | % |  |  |  | OK |
| ui.playback_meters_mixer_open.long_task_ms | 2892 | ms |  |  |  | OK |
| ui.playback_mixer_closed.frame_p95_ms | 16.7 | ms | <= 34 |  |  | OK |
| ui.playback_mixer_closed.busy_pct | 33.3 | % |  |  |  | OK |
| ui.playback_mixer_closed.long_task_ms | 0 | ms |  |  |  | OK |
| ui.fxwin_open_1_eq.frame_p95_ms | 16.7 | ms | <= 34 |  |  | OK |
| ui.fxwin_open_1_eq.busy_pct | 51.7 | % |  |  |  | OK |
| ui.fxwin_open_1_eq.long_task_ms | 0 | ms |  |  |  | OK |
| ui.fxwin_open_4.frame_p95_ms | 16.8 | ms | <= 34 |  |  | OK |
| ui.fxwin_open_4.busy_pct | 90 | % |  |  |  | OK |
| ui.fxwin_open_4.long_task_ms | 0 | ms |  |  |  | OK |
| ui.fxwin_open_8.frame_p95_ms | 33.3 | ms | <= 34 |  |  | OK |
| ui.fxwin_open_8.busy_pct | 105 | % |  |  |  | OK |
| ui.fxwin_open_8.long_task_ms | 0 | ms |  |  |  | OK |
| ui.pianoroll_open.frame_p95_ms | 183 | ms | <= 34 |  |  | OVER |
| ui.pianoroll_open.busy_pct | 154 | % |  |  |  | OK |
| ui.pianoroll_open.long_task_ms | 203 | ms |  |  |  | OK |
| ui.pianoroll_wheel_scroll_zoom.frame_p95_ms | 50.1 | ms | <= 34 |  |  | OVER |
| ui.pianoroll_wheel_scroll_zoom.busy_pct | 104 | % |  |  |  | OK |
| ui.pianoroll_wheel_scroll_zoom.long_task_ms | 105 | ms |  |  |  | OK |
| ui.pianoroll_add_move_notes_mouse.frame_p95_ms | 250 | ms | <= 34 |  |  | OVER |
| ui.pianoroll_add_move_notes_mouse.busy_pct | 96.8 | % |  |  |  | OK |
| ui.pianoroll_add_move_notes_mouse.long_task_ms | 13583 | ms |  |  |  | OK |
| ui.op.applyLocal_move_clip_ms | 6.2 | ms | <= 4 |  |  | OVER |
| ui.op.applyLocal_track_vol_ms | 0.1 | ms | <= 4 |  |  | OK |
| ui.op.applyLocal_fx_param_ms | 0.3 | ms | <= 4 |  |  | OK |
| ui.op.commit_move_clip_sync_ms | 6.9 | ms | <= 4 |  |  | OVER |
| ui.op.json_parse_state_ms | 4.9 | ms |  |  |  | OK |
| ui.op.replaceState_unchanged_ms | 31.4 | ms |  |  |  | OK |
| ui.op.replaceState_one_clip_changed_ms | 33.4 | ms |  |  |  | OK |
| ui.op.edit_to_second_frame_p50_ms | 62.2 | ms | <= 50 |  |  | OVER |
| ui.op.state_payload_kib | 923 | KiB |  |  |  | OK |
| ui.boot_ms | 1167 | ms |  |  |  | OK |
| ui.dom_nodes | 5322 | nodes |  |  |  | OK |
| ui.pianoroll_commit_to_frame_p50_ms | 163 | ms | <= 50 |  |  | OVER |
