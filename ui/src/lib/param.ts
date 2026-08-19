/**
 * Turning a `ParamDef` into a knob.
 *
 * The engine sends metadata rather than a picture: a range, a curve, a step and
 * a smoothing time. Everything a generic knob needs to lay itself out, show a
 * value and quantise a drag is derived here, in one place, so no view keeps a
 * second copy of an effect's taper.
 *
 * The stored value is **always the real value in the parameter's own unit**.
 * Normalised position is a rendering of it and never travels to the engine.
 */

import type { ParamDefDto } from '../api/types';

/** Sweep of a knob, in degrees, from full left to full right. */
export const KNOB_SWEEP_DEGREES = 270;

/**
 * A parameter with no meaningful in-between: a toggle or an enumeration.
 *
 * `smoothingMs: 0` is the engine's declaration of one -- there is no value
 * halfway between "peak" and "RMS", and ramping towards one would spend 10 ms
 * at a setting that does not exist. A UI must draw these as switches and must
 * not animate them.
 */
export function isSwitch(def: ParamDefDto): boolean {
  return def.smoothingMs === 0 && def.step !== null && def.step > 0;
}

/** How many positions a switch has. */
export function switchPositions(def: ParamDefDto): number {
  const step = def.step && def.step > 0 ? def.step : 1;
  return Math.round((def.max - def.min) / step) + 1;
}

/**
 * Value -> knob position, `0..1`.
 *
 * `log` is the taper that makes a pixel of drag a constant musical interval
 * everywhere; `db` is laid out **linearly**, because a linear slider in decibels
 * is already the logarithmic amplitude taper the ear wants and tapering it again
 * is the classic mistake.
 */
export function toNormalised(def: ParamDefDto, value: number): number {
  const span = def.max - def.min;
  if (!Number.isFinite(value) || span <= 0) return 0;
  if (def.curve === 'log' && def.min > 0 && def.max > 0) {
    const clamped = Math.min(def.max, Math.max(def.min, value));
    return Math.log(clamped / def.min) / Math.log(def.max / def.min);
  }
  return Math.min(1, Math.max(0, (value - def.min) / span));
}

/** Knob position -> value, snapped to the parameter's step grid. */
export function fromNormalised(def: ParamDefDto, t: number): number {
  const position = Math.min(1, Math.max(0, t));
  const raw =
    def.curve === 'log' && def.min > 0 && def.max > 0
      ? def.min * (def.max / def.min) ** position
      : def.min + position * (def.max - def.min);
  return snapToStep(def, raw);
}

/** Clamp into range and quantise onto the step grid, the way the engine does. */
export function snapToStep(def: ParamDefDto, value: number): number {
  const clamped = Math.min(def.max, Math.max(def.min, value));
  if (def.step === null || def.step <= 0) return clamped;
  const steps = Math.round((clamped - def.min) / def.step);
  return Math.min(def.max, def.min + steps * def.step);
}

/** Where a knob's pointer points, in degrees, `0` being straight up. */
export function knobAngle(t: number): number {
  return -KNOB_SWEEP_DEGREES / 2 + Math.min(1, Math.max(0, t)) * KNOB_SWEEP_DEGREES;
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/**
 * A value in its own unit, as the readout and the parameter toast write it.
 *
 * The unit strings are the engine's: `dB`, `Hz`, `ms`, `%`, `:1`, `samples`, or
 * empty. Anything else is printed after the number unchanged rather than
 * guessed at.
 */
export function formatValue(unit: string, value: number): string {
  if (!Number.isFinite(value)) return '--';
  switch (unit) {
    case 'dB':
      return `${value > 0 ? '+' : ''}${value.toFixed(1)} dB`;
    case 'Hz':
      return value >= 1000
        ? `${(value / 1000).toFixed(value >= 10_000 ? 1 : 2)} kHz`
        : `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} Hz`;
    case 'ms':
      return value >= 1000 ? `${(value / 1000).toFixed(2)} s` : `${value.toFixed(value < 10 ? 2 : 0)} ms`;
    case '%':
      return `${value.toFixed(0)} %`;
    case ':1':
      return `${value.toFixed(1)}:1`;
    case '':
      return value.toFixed(2);
    default:
      return `${value.toFixed(2)} ${unit}`;
  }
}

/**
 * The names a switch's positions go by.
 *
 * **A UI decision, and deliberately here rather than in the engine.** The engine
 * declares a parameter with two or three positions and refuses to name them,
 * because a name is a design choice and would end up in a file format. So the
 * table is ours: keyed on `typeId/paramId`, with an honest fallback for anything
 * it does not cover (`Off`/`On` for a toggle, the index otherwise), so a new
 * effect renders sensibly the day it lands rather than throwing.
 */
const SWITCH_LABELS: Readonly<Record<string, readonly string[]>> = {
  'effects.eq/hp.slope': ['12 dB/oct', '24 dB/oct'],
  'effects.compressor/detector': ['Peak', 'RMS'],
  'effects.delay/division': ['1/1', '1/2', '1/4', '1/8', '1/16', '1/32', '1/64'],
  'effects.delay/modifier': ['Straight', 'Dotted', 'Triplet'],
  'effects.delay/mode': ['Stereo', 'Ping-pong'],
  'effects.saturator/type': ['Tape', 'Tube', 'Clip'],
  'effects.saturator/oversample': ['Off', '2x', '4x'],
};

/** How a switch's current position reads. */
export function switchLabel(typeId: string, def: ParamDefDto, value: number): string {
  const index = Math.round((value - def.min) / (def.step && def.step > 0 ? def.step : 1));
  const named = SWITCH_LABELS[`${typeId}/${def.id}`]?.[index];
  if (named !== undefined) return named;
  if (switchPositions(def) === 2) return index === 0 ? 'Off' : 'On';
  return String(index);
}

/** How one parameter's value reads, switches included. */
export function formatParam(typeId: string, def: ParamDefDto, value: number): string {
  return isSwitch(def) ? switchLabel(typeId, def, value) : formatValue(def.unit, value);
}

/**
 * A short label for a knob's cap: the parameter's name with the band prefix
 * stripped, because a 4-across grid has room for `Freq` and not for
 * `Bell 1 Freq` -- and the card the knob sits in already says which band.
 */
export function shortParamName(name: string): string {
  return name.replace(/^(Bell [12]|Low|High|HP|SC)\s+/, '');
}
