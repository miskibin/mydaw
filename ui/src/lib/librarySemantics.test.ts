import { beforeEach, describe, expect, it } from 'vitest';

import {
  LIBRARY_SEMANTIC_CACHE_LIMIT,
  classifyLibraryEntry,
  clearLibrarySemanticCache,
  librarySemanticCacheSize,
  type LibrarySemanticPrimary,
} from './librarySemantics';

function file(name: string, relPath = name) {
  return classifyLibraryEntry({ name, relPath, kind: 'file' });
}

function dir(name: string, relPath = name) {
  return classifyLibraryEntry({ name, relPath, kind: 'dir' });
}

beforeEach(clearLibrarySemanticCache);

describe('library semantic precedence', () => {
  it('makes an explicit loop more important than a kick subtype', () => {
    expect(file('KSHMR_Kick_Loop_124.wav').primary).toBe('loop');
    expect(file('KSHMR_Kick_Loop_124.wav')).toMatchObject({
      shape: 'loop',
      content: 'kick',
      label: 'Kick loop',
      evidence: 'name',
    });
  });

  it('keeps a useful instrument primary for a one-shot', () => {
    expect(file('VFP_one_shot_snare_96.wav')).toMatchObject({
      primary: 'snare',
      shape: 'one-shot',
      content: 'snare',
      label: 'Snare one-shot',
    });
  });

  it('honors negative instrument phrases without losing the loop', () => {
    expect(file('No_Kick_Drum_Loop_123bpm.wav')).toMatchObject({
      primary: 'loop',
      shape: 'loop',
      content: 'drum',
      label: 'Drum loop',
    });
    expect(file('without_snare_percussion_loop.wav').content).toBe('percussion');
    expect(file('kickless_drum_loop.wav').content).toBe('drum');
  });

  it('does not infer a loop from BPM alone', () => {
    expect(file('PM_GB_Crash_One_Shot_123_BPM.wav')).toMatchObject({
      primary: 'cymbal',
      shape: 'one-shot',
      content: 'cymbal',
    });
    expect(file('Slow_Riser_120bpm.wav')).toMatchObject({
      primary: 'riser',
      shape: null,
    });
  });

  it('does not treat HH in a songstarter name as hi-hat', () => {
    expect(file('KSHMR_HH_Chill_SS_Better.wav', 'Songstarters/Hip Hop Chill/KSHMR HH Chill SS/KSHMR_HH_Chill_SS_Better.wav')).toMatchObject({
      primary: 'songstarter',
      shape: 'songstarter',
      content: null,
      evidence: 'folder',
    });
  });

  it('uses the nearest categorical parent when nested categories conflict', () => {
    expect(file('001.wav', 'Loops/One Shots/Kicks/001.wav')).toMatchObject({
      primary: 'kick',
      shape: 'one-shot',
      content: 'kick',
    });
    expect(file('002.wav', 'One Shots/Drum Loops/Kicks/002.wav')).toMatchObject({
      primary: 'loop',
      shape: 'loop',
      content: 'kick',
    });
  });

  it('does not let a vendor name containing loop classify descendants', () => {
    expect(file('001.wav', 'Loopmasters House Collection/Kicks/001.wav')).toMatchObject({
      primary: 'kick',
      shape: null,
      content: 'kick',
    });
  });
});

describe('library semantic vocabulary', () => {
  const cases: readonly [string, LibrarySemanticPrimary][] = [
    ['drum_loop.wav', 'loop'],
    ['groove.wav', 'loop'],
    ['one-shot.wav', 'one-shot'],
    ['trackout.wav', 'stem'],
    ['construction_kit.wav', 'songstarter'],
    ['drums.wav', 'drum'],
    ['bassdrum.wav', 'kick'],
    ['snr_04.wav', 'snare'],
    ['hand_claps.wav', 'clap'],
    ['side-stick.wav', 'rim'],
    ['closed_hihat.wav', 'hi-hat'],
    ['crash_04.wav', 'cymbal'],
    ['floor_tom.wav', 'tom'],
    ['perc_conga.wav', 'percussion'],
    ['tambourine.wav', 'shaker'],
    ['subbass_808.wav', '808'],
    ['drum_break.wav', 'fill'],
    ['reese_bass.wav', 'bass'],
    ['analog_synth.wav', 'synth'],
    ['lead.wav', 'lead'],
    ['warm_pad.wav', 'pad'],
    ['pluck.wav', 'pluck'],
    ['rhodes_keys.wav', 'keys'],
    ['organ.wav', 'organ'],
    ['electric_gtr.wav', 'guitar'],
    ['orchestral_strings.wav', 'strings'],
    ['trumpet.wav', 'brass'],
    ['saxophone.wav', 'woodwind'],
    ['vibraphone.wav', 'bell'],
    ['acapella_vox.wav', 'vocal'],
    ['choir_chant.wav', 'choir'],
    ['spoken_dialogue.wav', 'spoken'],
    ['generic_sfx.wav', 'fx'],
    ['cinematic_impact.wav', 'impact'],
    ['up_sweep.wav', 'riser'],
    ['downlifter.wav', 'faller'],
    ['transition_whoosh.wav', 'transition'],
    ['field_recording_texture.wav', 'ambience'],
    ['melodic_hook.wav', 'melody'],
    ['minor_chords.wav', 'chord'],
    ['synth_arpeggio.wav', 'arp'],
  ];

  it.each(cases)('classifies %s as %s', (name, primary) => {
    expect(file(name).primary).toBe(primary);
  });

  const separators = [' ', '_', '-', '.', ' (', '['] as const;
  const separatorCases = [
    ['kick', 'kick'],
    ['snare', 'snare'],
    ['drum loop', 'loop'],
    ['one shot clap', 'clap'],
    ['vocal phrase', 'vocal'],
    ['synth pad', 'pad'],
    ['fx impact', 'impact'],
    ['guitar chord', 'guitar'],
    ['808 sub bass', '808'],
    ['hi hat', 'hi-hat'],
  ] as const;

  it.each(
    separatorCases.flatMap(([words, primary]) =>
      separators.map((separator) => [words.split(' ').join(separator), primary] as const),
    ),
  )('normalizes separators in %s', (name, primary) => {
    expect(file(`${name}.WAV`).primary).toBe(primary);
  });

  it('normalizes camel case, letter-number boundaries, accents and extensions', () => {
    expect(file('WarmSynthPad120BPM.FLAC').primary).toBe('pad');
    expect(file('Mélodic_Hook.ogg').primary).toBe('melody');
    expect(file('VOCAL.MP3').format).toBe('mp3');
  });

  it('falls back to the file format and keeps folders format-free', () => {
    expect(file('001.FLAC')).toMatchObject({ primary: 'audio', label: 'FLAC audio', format: 'flac' });
    expect(dir('Unsorted')).toMatchObject({ primary: 'audio', label: 'Audio', format: null });
  });
});

describe('library semantic folder context', () => {
  it.each([
    ['Drum Loops', 'loop'],
    ['One Shots', 'one-shot'],
    ['Audio Stems', 'stem'],
    ['Songstarters', 'songstarter'],
    ['Kicks', 'kick'],
    ['Snares', 'snare'],
    ['Hi Hats', 'hi-hat'],
    ['Vocal Samples', 'vocal'],
    ['Guitars', 'guitar'],
    ['Ambiences', 'ambience'],
  ] as const)('classifies folder %s', (name, primary) => {
    expect(dir(name).primary).toBe(primary);
  });

  it('lets a generic file inherit content and shape independently', () => {
    expect(file('001.wav', 'Drum Loops/Kicks/001.wav')).toMatchObject({
      primary: 'loop',
      shape: 'loop',
      content: 'kick',
      evidence: 'folder',
    });
  });
});

describe('library semantic cache', () => {
  it('returns the cached immutable result for the same entry', () => {
    const first = file('kick_loop.wav');
    const second = file('kick_loop.wav');
    expect(second).toBe(first);
    expect(librarySemanticCacheSize()).toBe(1);
  });

  it('is bounded and evicts the least-recently-used key', () => {
    const oldest = file('oldest_kick.wav');
    for (let index = 0; index < LIBRARY_SEMANTIC_CACHE_LIMIT; index += 1) {
      file(`unknown_${index}.wav`);
    }
    expect(librarySemanticCacheSize()).toBe(LIBRARY_SEMANTIC_CACHE_LIMIT);
    expect(file('oldest_kick.wav')).not.toBe(oldest);
  });
});
