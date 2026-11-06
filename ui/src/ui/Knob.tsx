import { For, Show, createSignal, type JSX } from 'solid-js';
import { clamp } from '../lib/format';
import { dblCheck, track } from './drag';

const L36 = 61.26, C36 = 81.68;

export interface KnobProps {
  /** Normalised position 0..1. */
  t: () => number;
  /** Where the value arc starts (0 = left end, 0.5 = centre for bipolar params). */
  zero?: () => number;
  text: () => string;
  label: string;
  title?: string;
  key: string;
  onStart: () => void;
  onChange: (t: number) => void;
  onEnd: () => void;
  onReset: () => void;
  /** Wheel: a step of ±0.02 (fine: ±0.004) in normalised units. */
  onWheel: (t: number) => void;
  /** Right-click (automation / reset menu). */
  onMenu?: (x: number, y: number) => void;
  /** Labels under the two ends of the travel (e.g. "robot" / "natural"). */
  ends?: [string, string];
  /** Automated by a playlist clip: shows a marker; `t`/`text` then show the automated value. */
  automated?: () => boolean;
  /** Arc colour (defaults to the accent). */
  color?: string;
}

/** 36 px knob: drag ↕ (Shift = fine), wheel, double-click = default, right-click = menu. */
export function Knob(p: KnobProps) {
  const arc = () => {
    const t = clamp(p.t(), 0, 1), z = p.zero ? p.zero() : 0;
    return { dash: Math.abs(t - z) * L36 + 'px ' + C36 + 'px', off: -Math.min(t, z) * L36 + 'px', angle: -135 + t * 270 };
  };
  const down = (e: PointerEvent) => {
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); p.onMenu?.(e.clientX, e.clientY); return; }
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    if (dblCheck('knob' + p.key)) { p.onReset(); return; }
    let t = clamp(p.t(), 0, 1), ly = e.clientY;
    p.onStart();
    track(ev => {
      t = clamp(t + (ly - ev.clientY) / 150 * (ev.shiftKey ? 0.2 : 1), 0, 1); ly = ev.clientY;
      p.onChange(t);
    }, () => p.onEnd(), 'ns-resize');
  };
  const wheel = (e: WheelEvent) => {
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    p.onWheel(clamp(p.t() + (d < 0 ? 1 : -1) * 0.02 * (e.shiftKey ? 0.2 : 1), 0, 1));
  };
  return (
    <div class="knob" classList={{ auto: !!p.automated?.() }}>
      <div class="dial" onPointerDown={down} onWheel={wheel} onContextMenu={e => e.preventDefault()}
        title={p.title ?? 'Przeciągnij ↕ · Shift = precyzyjnie · 2× klik = domyślna · PPM = automatyzacja'}>
        <svg width="36" height="36" viewBox="0 0 36 36" style={{ position: 'absolute', inset: 0 }}>
          <circle cx="18" cy="18" r="13" fill="none" transform="rotate(135 18 18)" style={{ stroke: 'var(--knob-track)', 'stroke-width': '3px', 'stroke-linecap': 'round', 'stroke-dasharray': '61.26px 81.68px' }} />
          <circle cx="18" cy="18" r="13" fill="none" transform="rotate(135 18 18)" style={{ stroke: p.color ?? 'var(--blue)', 'stroke-width': '3px', 'stroke-linecap': 'round', 'stroke-dasharray': arc().dash, 'stroke-dashoffset': arc().off }} />
        </svg>
        <div class="cap" />
        <div class="ptr" style={{ transform: `rotate(${arc().angle}deg)` }} />
        <Show when={p.automated?.()}><span class="autodot" title="Automatyzowany na playliście" /></Show>
      </div>
      <Show when={p.ends}>{e => <div class="ends"><span>{e()[0]}</span><span>{e()[1]}</span></div>}</Show>
      <span class="val">{p.text()}</span>
      <span class="lab">{p.label}</span>
    </div>
  );
}

/** 22 px knob for dense rows (mixer slots, window headers): drag ↕, wheel, 2× click = default, right-click = menu. */
export function MiniKnob(p: { t: () => number; key: string; title: string; onStart: () => void; onChange: (t: number) => void; onEnd: () => void; onReset: () => void; onWheel: (t: number) => void; onMenu?: (x: number, y: number) => void; automated?: () => boolean }) {
  const R = 8, L = 2 * Math.PI * R * 0.75, C = 2 * Math.PI * R;
  const down = (e: PointerEvent) => {
    e.stopPropagation();
    if (e.button === 2) { e.preventDefault(); p.onMenu?.(e.clientX, e.clientY); return; }
    if (e.button !== 0) return;
    e.preventDefault();
    if (dblCheck('mk' + p.key)) { p.onReset(); return; }
    let t = clamp(p.t(), 0, 1), ly = e.clientY;
    p.onStart();
    track(ev => { t = clamp(t + (ly - ev.clientY) / 120 * (ev.shiftKey ? 0.2 : 1), 0, 1); ly = ev.clientY; p.onChange(t); }, () => p.onEnd(), 'ns-resize');
  };
  const wheel = (e: WheelEvent) => {
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    p.onWheel(clamp(p.t() + (d < 0 ? 1 : -1) * 0.03 * (e.shiftKey ? 0.2 : 1), 0, 1));
  };
  const t = () => clamp(p.t(), 0, 1);
  return (
    <span class="miniknob" classList={{ auto: !!p.automated?.() }} title={p.title} onPointerDown={down} onWheel={wheel} onContextMenu={e => e.preventDefault()} onClick={e => e.stopPropagation()}>
      <svg width="22" height="22" viewBox="0 0 22 22">
        <circle cx="11" cy="11" r={R} fill="none" transform="rotate(135 11 11)" style={{ stroke: 'var(--knob-track)', 'stroke-width': '2.5px', 'stroke-linecap': 'round', 'stroke-dasharray': `${L}px ${C}px` }} />
        <circle cx="11" cy="11" r={R} fill="none" transform="rotate(135 11 11)" style={{ stroke: 'var(--blue)', 'stroke-width': '2.5px', 'stroke-linecap': 'round', 'stroke-dasharray': `${t() * L}px ${C}px` }} />
        <line x1="11" y1="11" x2="11" y2="5" style={{ stroke: 'var(--fg-hi)', 'stroke-width': '1.6px', 'stroke-linecap': 'round' }} transform={`rotate(${-135 + t() * 270} 11 11)`} />
      </svg>
    </span>
  );
}

/** Segmented choice (catalog `options`): one button per option. */
export function Segmented(p: { options: string[]; value: () => number; pick: (i: number) => void; title?: string; label?: string; danger?: number; small?: boolean; color?: string; onMenu?: (x: number, y: number) => void }) {
  return (
    <div class="segp" title={p.title} onContextMenu={e => { e.preventDefault(); p.onMenu?.(e.clientX, e.clientY); }}>
      <Show when={p.label}><span class="lab">{p.label}</span></Show>
      <div class="seg2" classList={{ small: !!p.small }}>
        <For each={p.options}>{(o, i) => (
          <button classList={{ on: Math.round(p.value()) === i(), danger: p.danger === i() }}
            style={Math.round(p.value()) === i() && p.color && p.danger !== i() ? { background: p.color + '33', color: p.color } : {}}
            onClick={() => p.pick(i())}>{o}</button>
        )}</For>
      </div>
    </div>
  );
}

/**
 * A number you can drag (↕, Shift = fine), wheel, or double-click to type. `drag` maps a start
 * value and pixel delta to a value; `apply` receives begin/move/end for drags and 'set' for typed values.
 */
export function NumField(p: {
  label: string; value: () => number; text: (v: number) => string; drag: (v0: number, dpx: number) => number;
  parse: (s: string) => number | null; apply: (v: number, mode: 'begin' | 'move' | 'end' | 'set' | 'wheel') => void;
  disabled?: () => boolean; title?: string; color?: string; onMenu?: (x: number, y: number) => void; width?: number;
}): JSX.Element {
  const [edit, setEdit] = createSignal(false);
  let input!: HTMLInputElement;
  const down = (e: PointerEvent) => {
    if (p.disabled?.()) return;
    if (e.button === 2) { e.preventDefault(); e.stopPropagation(); p.onMenu?.(e.clientX, e.clientY); return; }
    if (e.button !== 0 || edit()) return;
    e.preventDefault(); e.stopPropagation();
    if (dblCheck('nf' + p.label)) { setEdit(true); setTimeout(() => { input?.focus(); input?.select(); }, 0); return; }
    const v0 = p.value(), sy = e.clientY, sx = e.clientX;
    let started = false;
    track(ev => {
      const d = ((ev.clientX - sx) + (sy - ev.clientY)) * (ev.shiftKey ? 0.2 : 1);
      if (!started) { if (Math.abs(d) < 2) return; started = true; p.apply(v0, 'begin'); }
      p.apply(p.drag(v0, d), 'move');
    }, () => { if (started) p.apply(p.value(), 'end'); }, 'ns-resize');
  };
  const wheel = (e: WheelEvent) => {
    if (p.disabled?.()) return;
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault(); e.stopPropagation();
    p.apply(p.drag(p.value(), (d < 0 ? 1 : -1) * (e.shiftKey ? 1 : 5)), 'wheel');
  };
  const done = (ok: boolean) => {
    if (ok) { const v = p.parse(input.value); if (v != null && isFinite(v)) p.apply(v, 'set'); }
    setEdit(false);
  };
  return (
    <div class="numf" classList={{ dis: !!p.disabled?.() }} title={p.title ?? 'Przeciągnij · kółko · 2× klik = wpisz'} style={p.width ? { width: p.width + 'px' } : {}}
      onPointerDown={down} onWheel={wheel} onContextMenu={e => e.preventDefault()}>
      <span class="lab">{p.label}</span>
      <Show when={edit()} fallback={<span class="v" style={p.color ? { color: p.color } : {}}>{p.disabled?.() ? '—' : p.text(p.value())}</span>}>
        <input ref={input} value={p.text(p.value()).replace(/[^\d.,+\-k]/gi, '')} onKeyDown={e => { e.stopPropagation(); if (e.key === 'Enter') done(true); if (e.key === 'Escape') done(false); }}
          onBlur={() => done(true)} onPointerDown={e => e.stopPropagation()} />
      </Show>
    </div>
  );
}

/** "1.2k", "1200 Hz", "-3,5" → number. */
export function parseNum(s: string): number | null {
  const m = s.trim().replace(',', '.').match(/^([+-]?\d*\.?\d+)\s*(k)?/i);
  if (!m) return null;
  return parseFloat(m[1]) * (m[2] ? 1000 : 1);
}
