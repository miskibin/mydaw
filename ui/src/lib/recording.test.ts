import { describe, expect, it } from 'vitest';

import type { InputStatusDto, RecordState } from '../api/types';
import {
  cancelTitle,
  formatElapsed,
  inputDeviceNote,
  nearLimit,
  recordAction,
  recordTitle,
} from './recording';

function status(state: RecordState, over: Partial<InputStatusDto> = {}): InputStatusDto {
  return {
    state,
    deviceName: state === 'idle' ? '' : 'Scarlett Solo',
    deviceId: null,
    deviceSampleRate: state === 'idle' ? 0 : 48_000,
    deviceChannels: state === 'idle' ? 0 : 2,
    sampleRate: 48_000,
    peak: 0,
    rms: 0,
    elapsedSeconds: 0,
    maxSeconds: 300,
    droppedSamples: 0,
    limitReached: false,
    fault: '',
    hasDevices: true,
    monitoring: true,
    offsetMs: 0,
    ...over,
  };
}

describe('elapsed take time', () => {
  it('counts in minutes and seconds, not the transport’s milliseconds', () => {
    expect(formatElapsed(0)).toBe('0:00');
    expect(formatElapsed(7.9)).toBe('0:07');
    expect(formatElapsed(61)).toBe('1:01');
    expect(formatElapsed(600)).toBe('10:00');
  });

  it('grows an hours field rather than showing 90 minutes', () => {
    expect(formatElapsed(3_600)).toBe('1:00:00');
    expect(formatElapsed(3_725)).toBe('1:02:05');
  });

  it('never shows a negative or a non-number', () => {
    expect(formatElapsed(-4)).toBe('0:00');
    expect(formatElapsed(Number.NaN)).toBe('0:00');
    expect(formatElapsed(Number.POSITIVE_INFINITY)).toBe('0:00');
  });
});

describe('the record button’s gesture', () => {
  it('arms when stopped and disarms when armed and stopped', () => {
    expect(recordAction(status('idle'), true)).toBe('arm');
    expect(recordAction(status('armed'), true)).toBe('disarm');
  });

  it('punches in while the transport runs and out while recording', () => {
    expect(recordAction(status('idle'), true, true)).toBe('punch-in');
    expect(recordAction(status('armed'), true, true)).toBe('punch-in');
    expect(recordAction(status('recording'), true, true)).toBe('punch-out');
    expect(recordAction(status('recording'), true, false)).toBe('punch-out');
  });

  it('offers arming before the first status has arrived', () => {
    expect(recordAction(undefined, true)).toBe('arm');
  });

  it('is unavailable with no capture device, whatever the status says', () => {
    expect(recordAction(status('idle'), false)).toBe('unavailable');
    expect(recordAction(undefined, false, true)).toBe('unavailable');
  });
});

describe('the record button’s tooltip', () => {
  it('explains every state, including the disabled one', () => {
    for (const [state, devices] of [
      ['idle', false],
      ['idle', true],
      ['armed', true],
      ['recording', true],
    ] as const) {
      const title = recordTitle(status(state), devices);
      expect(title.length, `${state}/${devices}`).toBeGreaterThan(20);
    }
  });

  it('names the device it will record from', () => {
    expect(recordTitle(status('armed'), true)).toContain('Scarlett Solo');
    expect(recordTitle(status('armed'), true, true)).toContain('Scarlett Solo');
  });

  it('says what to do about a machine with no input', () => {
    const title = recordTitle(status('idle'), false);
    expect(title).toContain('Settings');
  });

  it('distinguishes discarding a take from releasing the device', () => {
    expect(cancelTitle(status('recording'))).toContain('Discard');
    expect(cancelTitle(status('armed'))).toContain('Release');
  });
});

describe('the length limit', () => {
  it('warns in the last tenth of the budget and not before', () => {
    expect(nearLimit(status('recording', { elapsedSeconds: 200 }))).toBe(false);
    expect(nearLimit(status('recording', { elapsedSeconds: 270 }))).toBe(true);
  });

  it('does not warn when nothing is being recorded', () => {
    expect(nearLimit(status('armed', { elapsedSeconds: 999 }))).toBe(false);
    expect(nearLimit(undefined)).toBe(false);
  });

  it('does not divide by a missing limit', () => {
    expect(nearLimit(status('recording', { maxSeconds: 0, elapsedSeconds: 5 }))).toBe(false);
  });
});

describe('the device note in Settings', () => {
  it('names the layout and the rate', () => {
    expect(inputDeviceNote(1, 44_100)).toBe('Mono · 44.1 kHz');
    expect(inputDeviceNote(2, 48_000)).toBe('Stereo · 48.0 kHz');
    expect(inputDeviceNote(8, 96_000)).toBe('8 channels · 96.0 kHz');
  });

  it('says a device the host will not describe cannot be opened', () => {
    expect(inputDeviceNote(0, 0)).toContain('cannot be opened');
    expect(inputDeviceNote(2, 0)).toContain('cannot be opened');
  });
});
