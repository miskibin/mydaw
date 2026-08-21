import { describe, expect, it } from 'vitest';

import type { PlaylistTrackDto } from '../api/types';
import { laneSilenced } from './playlistLane';

function lane(partial: Partial<PlaylistTrackDto> & { id: number }): PlaylistTrackDto {
  return {
    name: `Track ${partial.id}`,
    muted: false,
    soloed: false,
    ...partial,
  };
}

describe('laneSilenced', () => {
  it('is mute when nothing is soloed', () => {
    const tracks = [lane({ id: 1 }), lane({ id: 2, muted: true })];
    expect(laneSilenced(tracks, 0)).toBe(false);
    expect(laneSilenced(tracks, 1)).toBe(true);
  });

  it('keeps only soloed lanes when any lane is soloed', () => {
    const tracks = [lane({ id: 1, soloed: true }), lane({ id: 2 })];
    expect(laneSilenced(tracks, 0)).toBe(false);
    expect(laneSilenced(tracks, 1)).toBe(true);
  });

  it('keeps a soloed muted lane audible', () => {
    const tracks = [lane({ id: 1, muted: true, soloed: true }), lane({ id: 2 })];
    expect(laneSilenced(tracks, 0)).toBe(false);
    expect(laneSilenced(tracks, 1)).toBe(true);
  });

  it('treats a missing index as silent', () => {
    expect(laneSilenced([], 0)).toBe(true);
  });
});
