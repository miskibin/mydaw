/**
 * The navbar master volume, the same control FL Studio keeps next to the hint
 * panel: a small knob for the final output, always on screen.
 *
 * It is the master fader under a different shape. Drag travel is the mixer's
 * fader curve, not a linear-in-dB sweep, so a turn here and a pull on the
 * mixer land on the same loudness. The value itself lives on app state so the
 * two stay in step while a gesture is in flight.
 */

import { onCleanup, onMount, type JSX } from 'solid-js';

import { clampDb, faderFraction, faderToDb, formatDb, MAX_DB, MIN_DB } from '../panels/mixer/scale';
import { knobAngle } from '../lib/param';
import { useAppState } from '../state';
import {
  bindWheel,
  FINE_FACTOR,
  KNOB_TRAVEL_PX,
  startDrag,
} from '../ui/drag';

/** Decibels per wheel notch, matching the mixer fader. */
const WHEEL_DB = 0.5;

export function MasterVolume(): JSX.Element {
  const app = useAppState();

  const gainDb = () => app.masterGainDb();
  const floor = () => app.mixer()?.minGainDb ?? MIN_DB;
  const ceiling = () => app.mixer()?.maxGainDb ?? MAX_DB;
  const ready = () => app.mixer() !== undefined;

  function setGain(db: number): void {
    const next = clampDb(db, floor(), ceiling());
    if (next === gainDb()) return;
    app.setMasterGain(next);
  }

  function onPointerDown(event: PointerEvent): void {
    if (!ready() || event.button !== 0) return;
    startDrag(event, {
      from: faderFraction(gainDb()),
      perPixel: 1 / KNOB_TRAVEL_PX,
      min: 0,
      max: 1,
      onChange: (fraction) => setGain(faderToDb(fraction)),
    });
  }

  function onDoubleClick(event: MouseEvent): void {
    if (!ready()) return;
    event.preventDefault();
    event.stopPropagation();
    setGain(0);
  }

  function onWheel(notches: number, event: WheelEvent): void {
    if (!ready()) return;
    const step = WHEEL_DB * (event.shiftKey ? FINE_FACTOR : 1);
    setGain(gainDb() + notches * step);
  }

  let root: HTMLDivElement | undefined;
  let unbindWheel: (() => void) | undefined;
  onMount(() => {
    if (root) unbindWheel = bindWheel(root, onWheel);
  });
  onCleanup(() => unbindWheel?.());

  return (
    <div
      ref={root}
      class="wv-vol"
      classList={{ 'wv-disabled': !ready() }}
      role="slider"
      tabindex="0"
      aria-label="Master volume"
      aria-valuemin={floor()}
      aria-valuemax={ceiling()}
      aria-valuenow={Number(gainDb().toFixed(1))}
      aria-valuetext={`${formatDb(gainDb())} dB`}
      title={`Master volume ${formatDb(gainDb())} dB — drag or scroll, shift for fine, double-click to reset`}
      onPointerDown={onPointerDown}
      onDblClick={onDoubleClick}
      onKeyDown={(event) => {
        if (!ready()) return;
        const step = event.shiftKey ? 0.1 : 1;
        if (event.key === 'ArrowUp') setGain(gainDb() + step);
        else if (event.key === 'ArrowDown') setGain(gainDb() - step);
        else if (event.key === 'Home') setGain(ceiling());
        else if (event.key === 'End') setGain(floor());
        else return;
        event.preventDefault();
      }}
    >
      <span class="wv-knob wv-vol__knob">
        <span
          class="wv-knob__pointer"
          style={{
            height: '8px',
            background: 'var(--acc)',
            transform: `translateX(-50%) rotate(${knobAngle(faderFraction(gainDb()))}deg)`,
          }}
        />
      </span>
      <span class="wv-vol__tag">VOL</span>
    </div>
  );
}
