import { describe, expect, it } from 'vitest';

import { createSampleKeyboardController, type SampleKeyboardSink } from './sampleKeyboard';

function sink(): SampleKeyboardSink & { log: string[] } {
  const log: string[] = [];
  return {
    log,
    noteOn(key, velocity) {
      log.push(`on:${key}:${velocity.toFixed(2)}`);
    },
    noteOff(key) {
      log.push(`off:${key}`);
    },
  };
}

function balanced(log: string[]): boolean {
  const held = new Map<number, number>();
  for (const entry of log) {
    if (entry.startsWith('on:')) {
      const key = Number(entry.split(':')[1]);
      held.set(key, (held.get(key) ?? 0) + 1);
    } else if (entry.startsWith('off:')) {
      const key = Number(entry.split(':')[1]);
      const next = (held.get(key) ?? 0) - 1;
      if (next < 0) return false;
      if (next === 0) held.delete(key);
      else held.set(key, next);
    }
  }
  return held.size === 0;
}

describe('sample keyboard controller', () => {
  it('sends a matching off on pointer up', () => {
    const notes = sink();
    const keys = createSampleKeyboardController(notes);
    keys.down(60, 1);
    keys.up(1);
    expect(notes.log).toEqual(['on:60:0.90', 'off:60']);
    expect(balanced(notes.log)).toBe(true);
  });

  it('glides by releasing the previous key first', () => {
    const notes = sink();
    const keys = createSampleKeyboardController(notes);
    keys.down(60, 1);
    keys.move(62, 1);
    keys.up(1);
    expect(notes.log).toEqual(['on:60:0.90', 'off:60', 'on:62:0.90', 'off:62']);
    expect(balanced(notes.log)).toBe(true);
  });

  it('releases on cancel, lost capture, track change, and cleanup', () => {
    for (const run of [
      (keys: ReturnType<typeof createSampleKeyboardController>) => {
        keys.down(54, 7);
        keys.cancel(7);
      },
      (keys: ReturnType<typeof createSampleKeyboardController>) => {
        keys.down(55, 3);
        keys.lostCapture(3);
      },
      (keys: ReturnType<typeof createSampleKeyboardController>) => {
        keys.setTrack('a');
        keys.down(56, 1);
        keys.setTrack('b');
      },
      (keys: ReturnType<typeof createSampleKeyboardController>) => {
        keys.down(57, 1);
        keys.cleanup();
      },
    ]) {
      const notes = sink();
      run(createSampleKeyboardController(notes));
      expect(balanced(notes.log), notes.log.join(' ')).toBe(true);
      expect(notes.log.some((entry) => entry.startsWith('on:'))).toBe(true);
    }
  });

  it('ignores move and up from a different pointer', () => {
    const notes = sink();
    const keys = createSampleKeyboardController(notes);
    keys.down(60, 1);
    keys.move(64, 2);
    keys.up(2);
    expect(keys.heldKey()).toBe(60);
    keys.up(1);
    expect(notes.log).toEqual(['on:60:0.90', 'off:60']);
    expect(balanced(notes.log)).toBe(true);
  });
});
