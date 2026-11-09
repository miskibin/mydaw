// Perf-harness hook: only bundled when built with VITE_PERF=1 (see perf/ui_bench.mjs).
// Exposes the stores and a few actions so Playwright can drive scenarios deterministically.
import * as A from './state/actions';
import * as E from './state/edit';
import * as P from './state/pattern';
import { applyLocal, replaceState, S, setS } from './state/store';
import { setU, U } from './state/ui';
import { geom } from './views/Playlist';
import { tele } from './state/telemetry';
import { be } from './api/backend';

(window as unknown as { __perf: unknown }).__perf = { S, setS, U, setU, A, E, P, geom, replaceState, applyLocal, tele, be };
