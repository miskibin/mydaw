import { describe, expect, it } from 'vitest';
import { guessIcon } from './instIcons';

describe('guessIcon', () => {
  it('names an 808 drum as drums and an 808 bass as bass', () => {
    for (const n of ['808 Kick Short', '808 Clap', '808 Closed Hat', '808 Snare']) expect(guessIcon(n)).toBe('drums');
    expect(guessIcon('808 Bass')).toBe('bass');
    expect(guessIcon('Sub 808')).toBe('bass');
  });
  it('recognises the common names', () => {
    expect(guessIcon('looperman-brazilian-guitar-sacos')).toBe('guitar');
    expect(guessIcon('Lead Vox')).toBe('vocal');
    expect(guessIcon('Pad Loop')).toBe('synth');
    expect(guessIcon('Untitled')).toBeNull();
  });
});
