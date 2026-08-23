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

  it('starts the rack as a kit channel and a melodic one', async () => {
    const backend = new MockBackend();
    const mixer = await backend.getTracks();
    expect(mixer.tracks).toHaveLength(2);
    expect(mixer.tracks[0]?.sourceId).toBe('sampler:808-kit');
    expect(mixer.tracks[0]?.engine).toBe('sampler');
    expect(mixer.tracks[1]?.sourceId).toBe('sampler:salamander-piano');

    // The kit is on a General MIDI map, so its grid fires the GM kick; the
    // melodic channel fires middle C.
    const pattern = (await backend.getPattern()).patternSet.active;
    expect(pattern.channels.map((clip) => clip.steps?.key)).toEqual([36, 60]);
  });

  it('refuses a drum id the engine no longer has', async () => {
    const backend = new MockBackend();
    for (const gone of ['kick', 'snare', 'drum-kit']) {
      await expect(backend.addTrack(gone, 'Drums')).rejects.toThrow(`unknown source '${gone}'`);
    }
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
    expect(sources.some((source) => source.id === 'sampler:808-kit')).toBe(true);
    await backend.previewInstrument('poly-synth');
    expect(backend.lastPreview).toBe('poly-synth');
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

describe('slice mode', () => {
  it('finds more hits as sensitivity rises and never fewer', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\breaks\\amen.wav');
    const track = mixer.tracks.at(-1)!;
    const counts: number[] = [];
    for (let step = 0; step <= 10; step += 1) {
      const found = await backend.analyzeSlices(track.index, 'transient', step / 10, 16);
      counts.push(found.frames.length);
      expect(found.mode).toBe('transient');
      expect(found.frames[0]).toBe(0);
      expect([...found.frames].sort((a, b) => a - b)).toEqual(found.frames);
    }
    expect(counts.every((count, index) => index === 0 || count >= counts[index - 1]!)).toBe(true);
    expect(counts.at(-1)!).toBeGreaterThan(counts[0]!);
  });

  it('divides the assumed bar exactly in grid mode', async () => {
    const backend = new MockBackend();
    const source = await backend.loadAudioSource('C:\\breaks\\loop.wav');
    const mixer = await backend.setTrackSource(0, `oneshot:${source.id}`);
    const track = mixer.tracks[0]!;
    await backend.setBpm(120);
    const quarters = await backend.analyzeSlices(track.index, 'grid', 0.5, 4);
    expect(quarters.mode).toBe('grid');
    expect(quarters.bars).toBeGreaterThanOrEqual(1);
    expect(quarters.frames).toHaveLength(quarters.bars * 4);
    const span = quarters.rangeEnd - quarters.rangeStart;
    quarters.frames.forEach((frame, index) => {
      expect(frame).toBe(Math.round((span * index) / (quarters.bars * 4)));
    });
    const sixteenths = await backend.analyzeSlices(track.index, 'grid', 0.5, 16);
    expect(sixteenths.frames).toHaveLength(quarters.frames.length * 4);
  });

  it('slices the region rather than the file once the sample is trimmed', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\breaks\\amen.wav');
    const track = mixer.tracks.at(-1)!;
    const whole = await backend.analyzeSlices(track.index, 'grid', 0.5, 4);
    expect(whole.rangeStart).toBe(0);

    const source = (await backend.getAudioSources()).find((item) => item.id === track.audioSourceId)!;
    const half = Math.floor(source.frames / 2);
    await backend.setTrackSampleRegion(track.index, half, 0);
    const trimmed = await backend.analyzeSlices(track.index, 'grid', 0.5, 4);
    expect(trimmed.rangeStart).toBe(half);
    expect(trimmed.rangeEnd).toBe(source.frames);
    expect(trimmed.frames[0]).toBe(half);
    expect(trimmed.frames.every((frame) => frame >= half)).toBe(true);
  });

  it('stores a chop, repairs a damaged one, and answers with what it kept', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\breaks\\amen.wav');
    const track = mixer.tracks.at(-1)!;
    expect(track.slices).toEqual({
      mode: 'off',
      frames: [],
      sensitivity: 0.5,
      division: 16,
      keytrack: false,
    });

    const stored = await backend.setTrackSlices(track.index, {
      mode: 'transient',
      frames: [900, 100, 500, 100, 99_999_999],
      sensitivity: 4,
      division: 5,
      keytrack: true,
    });
    expect(stored.frames).toEqual([100, 500, 900]);
    expect(stored.sensitivity).toBe(1);
    expect(stored.division).toBe(4);
    expect(stored.keytrack).toBe(true);
    expect((await backend.getTracks()).tracks[track.index]!.slices.frames).toEqual([100, 500, 900]);
  });

  it('caps the table at the engine size', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\breaks\\amen.wav');
    const track = mixer.tracks.at(-1)!;
    const stored = await backend.setTrackSlices(track.index, {
      mode: 'grid',
      frames: Array.from({ length: 400 }, (_, index) => index * 10),
      sensitivity: 0.5,
      division: 16,
      keytrack: false,
    });
    expect(stored.frames).toHaveLength(128);
  });

  it('keeps the markers when the mode goes back to off, and forgets them on a new file', async () => {
    const backend = new MockBackend();
    const mixer = await backend.addSampleChannel('C:\\breaks\\amen.wav');
    const track = mixer.tracks.at(-1)!;
    const found = await backend.analyzeSlices(track.index, 'transient', 0.9, 16);
    await backend.setTrackSlices(track.index, {
      mode: 'transient',
      frames: found.frames,
      sensitivity: 0.9,
      division: 16,
      keytrack: false,
    });

    const off = await backend.setTrackSlices(track.index, {
      mode: 'off',
      frames: found.frames,
      sensitivity: 0.9,
      division: 16,
      keytrack: false,
    });
    expect(off.mode).toBe('off');
    expect(off.frames).toEqual(found.frames);
    expect(off.sensitivity).toBe(0.9);

    // A mode with nothing in it is not a mode.
    const empty = await backend.setTrackSlices(track.index, {
      mode: 'transient',
      frames: [],
      sensitivity: 0.9,
      division: 16,
      keytrack: false,
    });
    expect(empty.mode).toBe('off');

    // A different file is a different chop.
    await backend.setTrackSlices(track.index, {
      mode: 'transient',
      frames: found.frames,
      sensitivity: 0.9,
      division: 16,
      keytrack: false,
    });
    const other = await backend.loadAudioSource('C:\\breaks\\other.wav');
    const swapped = await backend.setTrackSource(track.index, `oneshot:${other.id}`);
    expect(swapped.tracks[track.index]!.slices.frames).toEqual([]);
    expect(swapped.tracks[track.index]!.slices.mode).toBe('off');
  });

  it('refuses a chop on a synth channel', async () => {
    const backend = new MockBackend();
    const synth = (await backend.getTracks()).tracks.find(
      (entry) => entry.engine !== 'sample' && entry.engine !== 'clip',
    )!;
    await expect(backend.analyzeSlices(synth.index, 'transient', 0.5, 16)).rejects.toThrow(
      'audio-file channel',
    );
    await expect(
      backend.setTrackSlices(synth.index, {
        mode: 'transient',
        frames: [0],
        sensitivity: 0.5,
        division: 16,
        keytrack: false,
      }),
    ).rejects.toThrow('audio-file channel');
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

describe('audio input and recording', () => {
  /** Long enough that the mock's minimum-length refusal does not fire. */
  const takeLength = () => new Promise((resolve) => setTimeout(resolve, 80));

  it('offers a device list including one the host will not describe', async () => {
    const backend = new MockBackend();
    const devices = await backend.getInputDevices();
    expect(devices.length).toBeGreaterThan(1);
    expect(devices.filter((device) => device.isDefault)).toHaveLength(1);
    expect(devices.some((device) => device.channels === 0)).toBe(true);
  });

  it('starts idle with no device open and no level', async () => {
    const backend = new MockBackend();
    const status = await backend.getInputStatus();
    expect(status.state).toBe('idle');
    expect(status.deviceName).toBe('');
    expect(status.peak).toBe(0);
    expect(status.hasDevices).toBe(true);
  });

  it('mirrors the engine and refuses a device that is not there', async () => {
    const backend = new MockBackend();
    await expect(backend.setInputDevice('no-such-device')).rejects.toThrow(/not available/);
    expect((await backend.getInputStatus()).deviceId).toBeNull();
  });

  it('refuses a device that reports no usable configuration', async () => {
    const backend = new MockBackend();
    await backend.setInputDevice('mock-broken');
    await expect(backend.setInputArmed(true)).rejects.toThrow(/no usable configuration/);
  });

  it('meters only while the device is open', async () => {
    const backend = new MockBackend();
    expect((await backend.getInputStatus()).peak).toBe(0);
    await backend.setInputArmed(true);
    const armed = await backend.getInputStatus();
    expect(armed.state).toBe('armed');
    expect(armed.peak).toBeGreaterThan(0);
    expect(armed.deviceName).toBe('Microphone (Realtek Audio)');
    await backend.setInputArmed(false);
    expect((await backend.getInputStatus()).peak).toBe(0);
  });

  it('records a take and loads it as an audio source', async () => {
    const backend = new MockBackend();
    await backend.startRecording();
    expect((await backend.getInputStatus()).state).toBe('recording');
    await takeLength();

    const take = await backend.stopRecording();
    expect(take.source.name).toBe('Recording 1.wav');
    expect(take.source.durationSeconds).toBeGreaterThan(0);
    expect(take.limitReached).toBe(false);
    // Still armed, so a retake needs no second gesture.
    expect((await backend.getInputStatus()).state).toBe('armed');

    // The take is a real source: it is listed, and it has a waveform to draw.
    const sources = await backend.getAudioSources();
    expect(sources.some((source) => source.id === take.source.id)).toBe(true);
    const waveform = await backend.getAudioWaveform(take.source.id, 64);
    expect(waveform.peaks).toHaveLength(64);

    // And the same command a dropped WAV uses puts it on the rack.
    const mixer = await backend.addSampleChannel(take.source.path);
    const channel = mixer.tracks.at(-1)!;
    expect(channel.name).toBe('Recording 1.wav');
    expect(channel.audioSourceId).toBe(take.source.id);
  });

  it('numbers takes in order', async () => {
    const backend = new MockBackend();
    for (const expected of ['Recording 1.wav', 'Recording 2.wav']) {
      await backend.startRecording();
      await takeLength();
      expect((await backend.stopRecording()).source.name).toBe(expected);
    }
  });

  it('refuses to stop or cancel when nothing is being recorded', async () => {
    const backend = new MockBackend();
    await expect(backend.stopRecording()).rejects.toThrow(/nothing is being recorded/);
    await expect(backend.cancelRecording()).rejects.toThrow(/nothing is being recorded/);
  });

  it('cancels back to armed without producing a source', async () => {
    const backend = new MockBackend();
    const before = (await backend.getAudioSources()).length;
    await backend.startRecording();
    await takeLength();
    expect((await backend.cancelRecording()).state).toBe('armed');
    expect((await backend.getAudioSources()).length).toBe(before);
  });

  it('refuses a device change under a running take', async () => {
    const backend = new MockBackend();
    await backend.startRecording();
    await expect(backend.setInputDevice('mock-usb')).rejects.toThrow(/stop the take/);
  });
});

describe('destructive sample edits', () => {
  /**
   * A session whose channel 0 is a one-shot over a loaded file.
   *
   * The starter rack is a sampled *instrument* and a synth, neither of which is
   * an editable take, so these build the channel they need. The returned id is
   * the source the channel plays.
   */
  async function withSample(): Promise<{ backend: MockBackend; sourceId: number }> {
    const backend = new MockBackend();
    const sample = await backend.loadAudioSource('C:\samples\take.wav');
    await backend.setTrackSource(0, `oneshot:${sample.id}`);
    return { backend, sourceId: sample.id };
  }

  /** The loudest point of a source's drawn envelope. */
  async function peakOf(backend: MockBackend, sourceId: number): Promise<number> {
    const wave = await backend.getAudioWaveform(sourceId, 512);
    return wave.peaks.reduce((worst, [lo, hi]) => Math.max(worst, Math.abs(lo), Math.abs(hi)), 0);
  }

  it('writes a new source and leaves the original loaded', async () => {
    const { backend } = await withSample();
    const before = (await backend.getTracks()).tracks[0]!.audioSourceId!;
    const edit = await backend.editTrackSample(0, { kind: 'reverse' });

    expect(edit.previousSourceId).toBe(before);
    expect(edit.source.id).not.toBe(before);
    expect(edit.source.name).toContain('(reversed)');
    expect((await backend.getAudioSources()).map((entry) => entry.id)).toContain(before);
    expect(edit.mixer.tracks[0]!.audioSourceId).toBe(edit.source.id);
  });

  it('actually turns the waveform around', async () => {
    const { backend, sourceId } = await withSample();
    const before = await backend.getAudioWaveform(sourceId, 64);
    const edit = await backend.editTrackSample(0, { kind: 'reverse' });
    const after = await backend.getAudioWaveform(edit.source.id, 64);
    expect(after.peaks).not.toEqual(before.peaks);
    expect(after.peaks.map(([, hi]) => hi)).toEqual(
      before.peaks.map(([, hi]) => hi).reverse(),
    );
  });

  it('normalizes to the same target the engine uses', async () => {
    const { backend } = await withSample();
    const edit = await backend.editTrackSample(0, { kind: 'normalize' });
    const peak = await peakOf(backend, edit.source.id);
    expect(20 * Math.log10(peak)).toBeCloseTo(-0.3, 2);
  });

  it('applies a gain in dB and clamps an absurd one', async () => {
    const { backend, sourceId } = await withSample();
    const before = await peakOf(backend, sourceId);
    const quieter = await backend.editTrackSample(0, { kind: 'gain', db: -6 });
    expect(await peakOf(backend, quieter.source.id)).toBeCloseTo(before * 10 ** (-6 / 20), 4);
    expect(quieter.source.name).toContain('-6.0 dB');

    const clamped = await backend.editTrackSample(0, { kind: 'gain', db: 999 });
    expect(clamped.source.name).toContain('+24.0 dB');
  });

  it('trims to the region, clears it, and reports the clip shift', async () => {
    const { backend, sourceId } = await withSample();
    const source = (await backend.getAudioSources()).find((entry) => entry.id === sourceId)!;
    const start = Math.floor(source.frames / 4);
    const end = Math.floor(source.frames / 2);
    await backend.setTrackSampleRegion(0, start, end);

    const edit = await backend.editTrackSample(0, { kind: 'trim' });
    expect(edit.source.frames).toBe(end - start);
    expect([edit.startFrames, edit.endFrames]).toEqual([0, 0]);
    expect(edit.clipOffsetShiftFrames).toBe(-start);
    expect(edit.mixer.tracks[0]!.sampleStartFrames).toBe(0);
    expect(edit.mixer.tracks[0]!.sampleEndFrames).toBe(0);
  });

  it('refuses a trim with no region, and any edit on a synth channel', async () => {
    const { backend } = await withSample();
    await expect(backend.editTrackSample(0, { kind: 'trim' })).rejects.toThrow('region');
    const synth = (await backend.getTracks()).tracks.find(
      (entry) => entry.engine !== 'sample' && entry.engine !== 'clip',
    )!;
    await expect(backend.editTrackSample(synth.index, { kind: 'reverse' })).rejects.toThrow(
      'no audio file',
    );
  });

  it('undoes in one repoint, region and all', async () => {
    const { backend } = await withSample();
    const before = (await backend.getTracks()).tracks[0]!;
    await backend.setTrackSampleRegion(0, 100, 4_000);
    const edit = await backend.editTrackSample(0, { kind: 'trim' });

    const restored = await backend.setTrackAudioSource(
      edit.track,
      edit.previousSourceId,
      edit.previousStartFrames,
      edit.previousEndFrames,
      -edit.clipOffsetShiftFrames,
    );
    const track = restored.tracks[0]!;
    expect(track.audioSourceId).toBe(before.audioSourceId);
    expect(track.sampleStartFrames).toBe(100);
    expect(track.sampleEndFrames).toBe(4_000);
  });

  it('leaves another channel on the same take alone', async () => {
    const { backend } = await withSample();
    const shared = (await backend.getTracks()).tracks[0]!.audioSourceId!;
    const twin = (await backend.setTrackSource(1, `oneshot:${shared}`)).tracks[1]!;
    expect(twin.audioSourceId).toBe(shared);

    const edit = await backend.editTrackSample(0, { kind: 'reverse' });
    const after = (await backend.getTracks()).tracks;
    expect(after[0]!.audioSourceId).toBe(edit.source.id);
    expect(after[1]!.audioSourceId).toBe(shared);
  });
});

describe('render to sample', () => {
  /** Switch on `steps` of channel 0 in the active pattern. */
  async function programABeat(backend: MockBackend, steps: number[]): Promise<void> {
    const pattern = (await backend.getPattern()).patternSet.active;
    const clip = pattern.channels.find((entry) => entry.channel === 0)!;
    for (const step of steps) clip.steps!.steps[step]!.on = true;
    await backend.setPattern(pattern);
  }

  it('refuses an empty grid rather than adding a silent channel', async () => {
    const backend = new MockBackend();
    const before = (await backend.getTracks()).tracks.length;
    await expect(backend.renderPatternToSample()).rejects.toThrow('silent');
    expect((await backend.getTracks()).tracks.length).toBe(before);
  });

  it('lands one loop of the pattern as a new sample channel', async () => {
    const backend = new MockBackend();
    await programABeat(backend, [0, 4, 8, 12]);
    const before = (await backend.getTracks()).tracks.length;

    const result = await backend.renderPatternToSample();
    expect(result.track).toBe(before);
    expect(result.mixer.tracks.length).toBe(before + 1);

    const landed = result.mixer.tracks[result.track]!;
    expect(landed.engine).toBe('sample');
    expect(landed.audioSourceId).toBeGreaterThan(0);
    expect(result.name).toBe('Pattern 1.wav');
    // 16 steps of 4/4 at 130 BPM is one bar, near enough two seconds.
    expect(result.frames / result.sampleRate).toBeCloseTo((16 / 4) * (60 / 130), 3);
    expect(result.hostedChannels).toBe(0);
  });

  it('renders the beat that is programmed, not a generic envelope', async () => {
    const backend = new MockBackend();
    await programABeat(backend, [0, 8]);
    const two = await backend.renderPatternToSample();
    const twoPeaks = (await backend.getAudioWaveform(
      two.mixer.tracks[two.track]!.audioSourceId!,
      64,
    )).peaks;

    const other = new MockBackend();
    await programABeat(other, [0, 4, 8, 12]);
    const four = await other.renderPatternToSample();
    const fourPeaks = (await other.getAudioWaveform(
      four.mixer.tracks[four.track]!.audioSourceId!,
      64,
    )).peaks;

    const onsets = (peaks: [number, number][]) =>
      peaks.filter((pair, index) => pair[1] > 0.5 && (peaks[index - 1]?.[1] ?? 0) <= 0.5).length;
    expect(onsets(twoPeaks)).toBe(2);
    expect(onsets(fourPeaks)).toBe(4);
  });
});
