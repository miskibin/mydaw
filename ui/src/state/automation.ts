// Automation in the UI: the right-click menu every automatable control shares, creating an
// automation clip, and the live (automated) value a control shows while the song plays.
import { createMemo, createRoot, createSignal } from 'solid-js';
import type { AutoTarget } from '../api/types';
import { automatedKeys, automatedValue, createAutomationOps, denormalize, targetKey, targetLabel } from '../lib/automation';
import { fmtPos } from '../lib/format';
import { addFrameTask } from '../ui/frameLoop';
import { commit } from './edit';
import { allocId, S } from './store';
import { curBeat } from './telemetry';
import { openMenu, say, setU, U } from './ui';

const [beatSig, setBeatSig] = createSignal(0);
const keys = createRoot(() => createMemo(() => { S.project.clips.length; S.project.automations?.length; return automatedKeys(S.project); }));

// While playing, publish the playhead ~30×/s — only controls that are automated subscribe to it.
let lastSet = 0;
addFrameTask(now => {
  if (!U.playing || !keys().size) return;
  if (now - lastSet < 30) return;
  lastSet = now; setBeatSig(curBeat());
});

/** Is this control driven by an automation clip? */
export const isAutomated = (t: AutoTarget) => keys().has(targetKey(t));

/**
 * The value (plain units) a control should display: the automated value at the playhead while
 * playing (or at the start marker when stopped), or null when not automated.
 */
export function autoDisplay(t: AutoTarget): number | null {
  if (!isAutomated(t)) return null;
  const beat = U.playing ? beatSig() : U.start;
  const v = automatedValue(S.project, t, beat);
  return v == null ? null : denormalize(S.project, S.catalog, t, v);
}

/** Create an automation clip for `target` at the start marker (or over the loop), 4 bars long. */
export function automate(target: AutoTarget, color = '#e0a44a') {
  const lp = S.project.loop;
  const start = lp ? lp.a : Math.max(0, U.start), len = lp ? lp.b - lp.a : 16;
  const name = targetLabel(S.project, S.catalog, target);
  const r = createAutomationOps(S.project, S.catalog, target, name, color, start, len, allocId);
  commit(r.ops, 'Automatyzacja');
  setU('sel', [r.clipId]);
  say('Automatyzacja: ' + name + ' · ścieżka ' + (r.track + 1) + ' · ' + fmtPos(start) + ' — 2× klik na klipie = edycja punktów');
}

/** The shared right-click menu of an automatable control. */
export function paramMenu(x: number, y: number, target: AutoTarget, reset: (() => void) | null, color?: string) {
  const auto = isAutomated(target);
  const clips = () => S.project.clips.filter(c => c.kind === 'auto' && S.project.automations?.find(a => a.id === c.ref && targetKey(a.target) === targetKey(target)));
  openMenu(x, y, [
    { t: 'title', label: targetLabel(S.project, S.catalog, target) },
    { t: 'item', label: 'Automatyzuj na playliście', kbd: S.project.loop ? 'pętla' : '4 takty', act: () => automate(target, color) },
    ...(auto ? [{ t: 'item' as const, label: 'Zaznacz klipy automatyzacji', act: () => setU('sel', clips().map(c => c.id)) }] : []),
    ...(reset ? [{ t: 'sep' as const }, { t: 'item' as const, label: 'Reset do domyślnej', kbd: '2× klik', act: reset }] : []),
  ], 140);
}
