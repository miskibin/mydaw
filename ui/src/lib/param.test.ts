import { describe, expect, it } from 'vitest';

import type { ParamDefDto } from '../api/types';
import { formatPan, formatValue, fromNormalised, toNormalised } from './param';

const PITCH: ParamDefDto = {
  id: 'pitch',
  name: 'Pitch',
  unit: 'ct',
  min: -1200,
  max: 1200,
  defaultValue: 0,
  curve: 'linear',
  step: 1,
  smoothingMs: 10,
};

describe('formatValue', () => {
  it('prints cents as a signed integer', () => {
    expect(formatValue('ct', 0)).toBe('0 ct');
    expect(formatValue('ct', 120)).toBe('+120 ct');
    expect(formatValue('ct', -50.4)).toBe('-50 ct');
  });
});

describe('formatPan', () => {
  it('reads as C / L / R rather than a raw number', () => {
    expect(formatPan(0)).toBe('C');
    expect(formatPan(-0.4)).toBe('L40');
    expect(formatPan(1)).toBe('R100');
  });
});

describe('pitch slider mapping', () => {
  it('puts the centre of a bipolar pitch range at 0 ct', () => {
    expect(fromNormalised(PITCH, 0.5)).toBe(0);
    expect(toNormalised(PITCH, 0)).toBe(0.5);
  });

  it('maps the right edge to +1200 ct', () => {
    expect(fromNormalised(PITCH, 1)).toBe(1200);
    expect(toNormalised(PITCH, 1200)).toBe(1);
    expect(fromNormalised(PITCH, 0)).toBe(-1200);
  });
});
