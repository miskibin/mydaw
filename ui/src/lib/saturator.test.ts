import { describe, expect, it } from 'vitest';
import { migrateSatParams, satDriveDb } from './saturator';

describe('saturator params', () => {
  it('migrates the old drive in dB to the same gain on the new knob, once', () => {
    const p: Record<string, number> = { drive: 9, mix: 80, out: -3 };
    expect(migrateSatParams(p)).toBe(true);
    expect(satDriveDb(p)).toBeCloseTo(9, 6);
    expect(p).toMatchObject({ style: 0, auto: 0, mix: 80, out: -7.5 });
    expect(migrateSatParams(p)).toBe(false);
  });
  it('punish adds 20 dB', () => {
    expect(satDriveDb({ drive: 100, punish: 1 })).toBe(56);
  });
});
