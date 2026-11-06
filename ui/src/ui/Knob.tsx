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
}

/** 36 px knob: drag ↕ (Shift = fine), wheel, double-click = default. */
export function Knob(p: KnobProps) {
  const arc = () => {
    const t = clamp(p.t(), 0, 1), z = p.zero ? p.zero() : 0;
    return { dash: Math.abs(t - z) * L36 + 'px ' + C36 + 'px', off: -Math.min(t, z) * L36 + 'px', angle: -135 + t * 270 };
  };
  const down = (e: PointerEvent) => {
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
    <div class="knob">
      <div class="dial" onPointerDown={down} onWheel={wheel} title={p.title ?? 'Przeciągnij ↕ · Shift = precyzyjnie · 2× klik = domyślna'}>
        <svg width="36" height="36" viewBox="0 0 36 36" style={{ position: 'absolute', inset: 0 }}>
          <circle cx="18" cy="18" r="13" fill="none" transform="rotate(135 18 18)" style={{ stroke: '#2c333d', 'stroke-width': '3px', 'stroke-linecap': 'round', 'stroke-dasharray': '61.26px 81.68px' }} />
          <circle cx="18" cy="18" r="13" fill="none" transform="rotate(135 18 18)" style={{ stroke: '#4f8ef7', 'stroke-width': '3px', 'stroke-linecap': 'round', 'stroke-dasharray': arc().dash, 'stroke-dashoffset': arc().off }} />
        </svg>
        <div style={{ position: 'absolute', left: '9px', top: '9px', width: '18px', height: '18px', 'border-radius': '50%', background: '#2c333d', 'box-shadow': '0 1px 3px rgba(0,0,0,.4)' }} />
        <div style={{ position: 'absolute', left: '17px', top: '9px', width: '2px', height: '7px', 'border-radius': '1px', background: '#f2f4f7', 'transform-origin': '1px 9px', transform: `rotate(${arc().angle}deg)` }} />
      </div>
      <span class="val">{p.text()}</span>
      <span class="lab">{p.label}</span>
    </div>
  );
}
