import { describe, expect, it } from 'vitest';

import {
  f7PatternId,
  isRepeatClick,
  mixerSlotOpensEditor,
  mixerStripDestination,
  patternEditorTarget,
  pickerChangesView,
  rollChannelFromStrip,
  tabCounterpart,
} from './navigate';

describe('patternEditorTarget', () => {
  it('opens the rack from a playlist clip, even when the pattern has notes', () => {
    expect(
      patternEditorTarget({ hitCount: 0, noteCount: 4 }, { altKey: false, surface: 'clip' }),
    ).toBe('rack');
    expect(
      patternEditorTarget({ hitCount: 8, noteCount: 1 }, { altKey: false, surface: 'picker' }),
    ).toBe('rack');
  });

  it('opens the rack when the pattern is empty or only has steps', () => {
    expect(
      patternEditorTarget({ hitCount: 0, noteCount: 0 }, { altKey: false, surface: 'clip' }),
    ).toBe('rack');
    expect(
      patternEditorTarget({ hitCount: 12, noteCount: 0 }, { altKey: false, surface: 'picker' }),
    ).toBe('rack');
  });

  it('opens the piano roll when Alt is held', () => {
    expect(
      patternEditorTarget({ hitCount: 0, noteCount: 8 }, { altKey: true, surface: 'clip' }),
    ).toBe('roll');
    expect(
      patternEditorTarget({ hitCount: 12, noteCount: 0 }, { altKey: true, surface: 'picker' }),
    ).toBe('roll');
  });

  it('opens the piano roll from a rack tab, because the rack is already showing', () => {
    expect(
      patternEditorTarget({ hitCount: 12, noteCount: 0 }, { altKey: false, surface: 'tab' }),
    ).toBe('roll');
    expect(
      patternEditorTarget({ hitCount: 0, noteCount: 0 }, { altKey: true, surface: 'tab' }),
    ).toBe('roll');
  });
});

describe('isRepeatClick', () => {
  it('treats the second press of a double-click as a repeat, not a new paint', () => {
    expect(isRepeatClick(1)).toBe(false);
    expect(isRepeatClick(2)).toBe(true);
    expect(isRepeatClick(3)).toBe(true);
  });
});

describe('pickerChangesView', () => {
  it('never jumps to the playlist on a single click', () => {
    expect(pickerChangesView('click')).toBe(false);
  });

  it('lets a double-click leave the current view for the pattern editor', () => {
    expect(pickerChangesView('dblclick')).toBe(true);
  });
});

describe('tabCounterpart', () => {
  it('toggles playlist and rack', () => {
    expect(tabCounterpart('playlist')).toBe('rack');
    expect(tabCounterpart('rack')).toBe('playlist');
  });

  it('returns to the playlist from mixer or settings', () => {
    expect(tabCounterpart('mixer')).toBe('playlist');
    expect(tabCounterpart('settings')).toBe('playlist');
  });
});

describe('f7PatternId', () => {
  it('uses the first selected pattern clip when the playlist is up', () => {
    expect(
      f7PatternId('playlist', [
        { kind: 'audio' },
        { kind: 'pattern', patternId: 7 },
        { kind: 'pattern', patternId: 3 },
      ]),
    ).toBe(7);
  });

  it('is silent when no pattern clip is selected, or the playlist is not showing', () => {
    expect(f7PatternId('playlist', [{ kind: 'audio' }])).toBeNull();
    expect(f7PatternId('playlist', [])).toBeNull();
    expect(f7PatternId('rack', [{ kind: 'pattern', patternId: 7 }])).toBeNull();
  });
});

describe('rollChannelFromStrip', () => {
  it('follows the selected track, else the mixer selection', () => {
    expect(rollChannelFromStrip({ kind: 'track', index: 4 }, 0)).toBe(4);
    expect(rollChannelFromStrip({ kind: 'master' }, 2)).toBe(2);
    expect(rollChannelFromStrip({ kind: 'bus', index: 1 }, 3)).toBe(3);
  });
});

describe('mixer destinations', () => {
  it('takes a track strip to the rack and leaves buses and master alone', () => {
    expect(mixerStripDestination('track')).toBe('rack');
    expect(mixerStripDestination('bus')).toBeNull();
    expect(mixerStripDestination('master')).toBeNull();
  });

  it('opens the editor only for a filled insert slot', () => {
    expect(mixerSlotOpensEditor('fx')).toBe(true);
    expect(mixerSlotOpensEditor('add')).toBe(false);
    expect(mixerSlotOpensEditor('blank')).toBe(false);
    expect(mixerSlotOpensEditor('more')).toBe(false);
  });
});
