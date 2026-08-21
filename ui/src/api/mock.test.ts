import { describe, expect, it } from 'vitest';

import { MockBackend } from './mock';

describe('rack sample replacement', () => {
  it('swaps only the source and preserves the row mixer state and inserts', async () => {
    const backend = new MockBackend();
    await backend.setTrackGain(0, -7.5);
    await backend.setTrackPan(0, 0.4);
    await backend.addInsert({ kind: 'track', index: 0 }, 'effects.eq');

    const sample = await backend.loadAudioSource('C:\\samples\\replacement.wav');
    const state = await backend.setTrackSource(0, `oneshot:${sample.id}`);
    const track = state.tracks[0]!;

    expect(track.engine).toBe('sample');
    expect(track.audioSourceId).toBe(sample.id);
    expect(track.sourceLabel).toBe('replacement.wav');
    expect(track.gainDb).toBe(-7.5);
    expect(track.pan).toBe(0.4);
    expect(track.inserts).toHaveLength(1);
  });

  it('starts the rack drums as sample channels', async () => {
    const backend = new MockBackend();
    const mixer = await backend.getTracks();
    expect(mixer.tracks[0]?.engine).toBe('sample');
    expect(mixer.tracks[0]?.audioSourceId).toBe(1);
    expect(mixer.tracks[1]?.engine).toBe('sample');
    const pattern = (await backend.getPattern()).patternSet.active;
    expect(pattern.channels.map((clip) => clip.steps?.key)).toEqual([60, 60, 60, 60, 60]);
  });

  it('places a Sounds-tab drum as a sample channel', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addTrack('snare', 'Snare');
    const track = mixer.tracks.at(-1)!;
    expect(track.engine).toBe('sample');
    expect(track.sourceId).toMatch(/^oneshot:/);
    expect(track.audioSourceId).toBeGreaterThan(0);
    expect(track.name).toBe('Snare');

    const kit = await backend.addTrack('drum-kit', 'Drum Kit');
    const placed = kit.tracks.at(-1)!;
    expect(placed.engine).toBe('synth');
    expect(placed.sourceId).toBe('drum-kit');
  });
});

describe('MIDI controller pairing', () => {
  it('lists mock ports off by default and pairs on enable', async () => {
    const backend = new MockBackend();
    const listed = await backend.listMidiPorts();
    expect(listed).toHaveLength(2);
    expect(listed.every((port) => !port.enabled)).toBe(true);
    expect((await backend.getMidiStatus()).enabledCount).toBe(0);

    const after = await backend.setMidiPortEnabled('Mock MIDI Keyboard', true);
    const keyboard = after.find((port) => port.name === 'Mock MIDI Keyboard')!;
    expect(keyboard.enabled).toBe(true);
    expect(keyboard.connected).toBe(true);
    expect(keyboard.status).toBe('connected');

    const status = await backend.getMidiStatus();
    expect(status.enabledCount).toBe(1);
    expect(status.activeName).toBe('Mock MIDI Keyboard');
    expect(status.activityAtMs).toBeGreaterThan(0);
  });

  it('refuses to pair a name that is not in the list', async () => {
    const backend = new MockBackend();
    await expect(backend.setMidiPortEnabled('No Such Device', true)).rejects.toThrow(
      "there is no MIDI port 'No Such Device'",
    );
  });
});

describe('browser preview', () => {
  it('auditions the clicked instrument id, not the selected rack source', async () => {
    const backend = new MockBackend();
    const sources = await backend.getSources();
    expect(sources.some((source) => source.id === 'kick')).toBe(true);
    await backend.previewInstrument('snare');
    expect(backend.lastPreview).toBe('snare');
  });

  it('stops a preview when the transport stops', async () => {
    const backend = new MockBackend();
    await backend.previewAudioFile('C:\\packs\\kick.wav');
    expect(backend.lastPreview).toBe('C:\\packs\\kick.wav');
    await backend.transportStop();
    expect(backend.lastPreview).toBeNull();
  });

  it('auditions an edited audio-file channel by track', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\packs\\hat.wav');
    const track = mixer.tracks.at(-1)!;
    await backend.setTrackPitch(track.index, 700);
    await backend.setTrackFades(track.index, 20, 80);

    await backend.previewTrackSample(track.index);

    expect(backend.lastPreview).toBe(`track:${track.index}`);
  });

  it('defaults preview volume to 80% of master and clamps the setting', async () => {
    const backend = new MockBackend();
    expect((await backend.getPreviewSettings()).volumePercent).toBe(80);
    expect((await backend.setPreviewVolume(150)).volumePercent).toBe(100);
    expect((await backend.setPreviewVolume(-4)).volumePercent).toBe(0);
    expect((await backend.setPreviewVolume(33)).volumePercent).toBe(33);
  });

  it('lets the user request ASIO and a buffer, and reports the actual stream', async () => {
    const backend = new MockBackend();
    const initial = await backend.getAudioSettings();
    expect(initial.preferAsio).toBe(false);
    expect(initial.bufferFrames).toBe(256);
    expect(initial.stream.backend).toBe('wasapi');
    expect(initial.stream.bufferFrames).toBe(256);

    const next = await backend.setAudioSettings(true, 300);
    expect(next.preferAsio).toBe(true);
    expect(next.bufferFrames).toBe(256);
    expect(next.stream.backend).toBe('wasapi');
    expect(next.stream.note).toMatch(/WASAPI/);
    expect(next.stream.latencyMs).toBeCloseTo((256 / 48_000) * 1000);
  });
});

describe('sample root pitch', () => {
  it('detects a tone fixture and reports no pitch for noise', async () => {
    const backend = new MockBackend();
    const toneMixer = await backend.addSampleChannel('C:\\packs\\tone-A4.wav');
    const noiseMixer = await backend.addSampleChannel('C:\\packs\\noise.wav');
    const detected = await backend.analyzeTrackPitch(toneMixer.tracks.at(-1)!.index);
    expect(detected).toMatchObject({ kind: 'detected', midiNote: 69, source: 'analysis' });
    const unpitched = await backend.analyzeTrackPitch(noiseMixer.tracks.at(-1)!.index);
    expect(unpitched).toEqual({ kind: 'noStablePitch', reason: 'unvoiced' });
  });

  it('stores a playback root on getTracks without changing pitchCents', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\packs\\tone-A4.wav');
    const track = mixer.tracks.at(-1)!;
    expect(track.rootKey).toBe(60);
    expect(track.rootMode).toBe('default');
    const after = await backend.setTrackSampleRoot(track.index, 54, 12, 'detected');
    const stored = after.tracks.at(-1)!;
    expect(stored.rootKey).toBe(54);
    expect(stored.rootFineCents).toBe(12);
    expect(stored.rootMode).toBe('detected');
    expect(stored.pitchCents).toBe(0);
    expect((await backend.getTracks()).tracks.at(-1)?.rootKey).toBe(54);
  });

  it('accepts a live note on an Audio Clip channel', async () => {
    const backend = new MockBackend();
    const source = await backend.loadAudioSource('C:\\takes\\vox.wav');
    await backend.addAudioClip(source.id, 0, 0, 3840);
    const clip = (await backend.getTracks()).tracks.find((entry) => entry.engine === 'clip')!;
    await backend.setSelectedTrack(clip.index);
    await backend.noteOn(54, 0.9);
    expect(backend.lastNoteOn).toEqual({ key: 54, velocity: 0.9 });
  });

  it('refuses root edits on a synth channel', async () => {
    const backend = new MockBackend();
    const synth = (await backend.getTracks()).tracks.find(
      (entry) => entry.engine !== 'sample' && entry.engine !== 'clip',
    )!;
    await expect(backend.analyzeTrackPitch(synth.index)).rejects.toThrow('audio-file channel');
    await expect(backend.setTrackSampleRoot(synth.index, 60, 0, 'manual')).rejects.toThrow(
      'audio-file channel',
    );
  });
});

describe('spectrum tap', () => {
  it('arms a track source without an insert index', async () => {
    const backend = new MockBackend();
    expect(await backend.getInsertSpectrum()).toBeNull();
    await backend.watchSpectrum({ kind: 'track', index: 0 }, null);
    const silent = await backend.getInsertSpectrum();
    expect(silent).not.toBeNull();
    expect(silent!.scopeL.length).toBeGreaterThan(1);
    expect(Math.max(...silent!.scopeL.map(Math.abs))).toBeLessThan(0.012);

    await backend.noteOn(60, 0.9);
    const sounding = await backend.getInsertSpectrum();
    expect(Math.max(...sounding!.scopeL.map(Math.abs))).toBeGreaterThan(0.2);

    await backend.watchSpectrum(null, null);
    expect(await backend.getInsertSpectrum()).toBeNull();
  });
});
