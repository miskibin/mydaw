/**
 * The browser-only backend.
 *
 * `pnpm dev` in a plain browser has no Rust side, so this stands in for the
 * whole engine: five tracks, a pattern set, an arrangement, buses, sends,
 * insert chains built from the **real** effect catalogue, a synthetic meter
 * stream and an invented audio file.
 *
 * Two rules keep it worth having:
 *
 * 1. **It mirrors the engine's refusals.** A mock that accepts what Rust
 *    rejects is a worse rehearsal than no mock at all -- the error path is the
 *    half of a view that never gets looked at otherwise.
 * 2. **It says what it cannot do.** There is no native window, no file dialog
 *    and no audio device in a browser, so those log and answer plausibly rather
 *    than pretending they happened.
 */

import type { Backend } from './backend';
import type {
  AssetStatus,
  AudioSourceDto,
  BusDto,
  ChannelClipDto,
  ClapGuiStatus,
  ClapPluginInfoDto,
  ClapSlot,
  ClapTarget,
  ClipDto,
  EffectInfoDto,
  InsertDto,
  InsertReadoutsDto,
  InsertTarget,
  LibraryEntryDto,
  LibraryRootDto,
  MasterDto,
  MeterFrame,
  MixerState,
  ParamDefDto,
  ParamValueDto,
  PatternDto,
  PatternSetDto,
  PitchAnalysis,
  PlayMode,
  PlaylistDto,
  PlaylistTrackDto,
  ProjectInfo,
  RenderToSampleDto,
  RootMode,
  SampleEditDto,
  SampleOp,
  SendDto,
  SequencerState,
  SliceAnalysis,
  SliceMode,
  SliceState,
  SourceDto,
  SpectrumDto,
  StreamInfo,
  StripRef,
  MidiPortDto,
  MidiStatusDto,
  PreviewSettings,
  AudioOutput,
  InputDeviceDto,
  InputStatusDto,
  RecordState,
  RecordedTakeDto,
  TrackDto,
  UiSnapshot,
  Unsubscribe,
  Vst3GuiStatus,
  Vst3PluginInfoDto,
  Vst3Slot,
  WaveformDto,
  SourcePreviewDto,
} from './types';
import { engineOf } from '../lib/channel';
import { occupiedChannels } from '../lib/pattern';
import { snapAudioBufferFrames } from '../lib/audioOutput';
import { clampPreviewVolumePercent } from '../lib/previewVolume';

// ---------------------------------------------------------------------------
// Engine constants, mirrored
// ---------------------------------------------------------------------------

const MAX_TRACKS = 16;
const MAX_BUSES = 8;
const MAX_INSERTS = 8;
const MIN_GAIN_DB = -60;
const MAX_GAIN_DB = 6;
const MAX_SEND_LEVEL = 1.25;
const MAX_SEND_DB = 1.9382;
const MAX_PATTERNS = 32;
const MAX_PLAYLIST_TRACKS = 16;
const MAX_CLIPS = 512;
const MAX_SONG_TICKS = 3_840 * 512;
const MAX_NOTES = 1024;
const MAX_WAVEFORM_BINS = 4096;

/**
 * Bins the mock stores per source.
 *
 * The engine keeps samples and folds them to whatever the caller asks for; the
 * mock keeps one fixed-resolution envelope and folds that. A thousand-odd bins
 * is finer than any window draws, so a fold never invents detail.
 */
const MOCK_ENVELOPE_BINS = 1024;

/** Peak a Normalize lands on, matching `app/src/sample_edit.rs`. */
const NORMALIZE_TARGET_DBFS = -0.3;

/** Largest gain one destructive Gain edit applies, matching the engine. */
const MAX_EDIT_GAIN_DB = 24;

/** Where the engine writes destructive edits, mirrored for plausible paths. */
const SAMPLE_EDIT_DIR = 'C:\\Users\\wave\\AppData\\Local\\WAVE Studio\\sample-edits';

/** Where the engine writes pattern renders. */
const RENDER_DIR = 'C:\\Users\\wave\\AppData\\Local\\WAVE Studio\\renders';

/** What the status bar calls each destructive edit. */
const OP_LABELS: Record<SampleOp['kind'], string> = {
  reverse: 'Reverse',
  normalize: 'Normalize',
  trim: 'Trim',
  gain: 'Gain',
};

/** Fold a stored envelope down to `bins` min/max pairs. */
function foldPeaks(peaks: readonly [number, number][], bins: number): [number, number][] {
  const out: [number, number][] = [];
  const count = Math.max(1, Math.min(bins, peaks.length));
  for (let bin = 0; bin < count; bin++) {
    const from = Math.floor((bin * peaks.length) / count);
    const to = Math.max(from + 1, Math.floor(((bin + 1) * peaks.length) / count));
    let lo = 0;
    let hi = 0;
    for (let index = from; index < to && index < peaks.length; index++) {
      const pair = peaks[index];
      if (!pair) continue;
      lo = Math.min(lo, pair[0]);
      hi = Math.max(hi, pair[1]);
    }
    out.push([lo, hi]);
  }
  return out;
}

/** `MAX_SLICES` in `plugins/sequencer/src/oneshot.rs`. */
const MAX_SLICES = 128;
const DEFAULT_SLICE_SENSITIVITY = 0.5;
const DEFAULT_SLICE_DIVISION = 16;
/** `MIN_GAP_MS` in `crates/slice-analysis`. */
const SLICE_MIN_GAP_MS = 30;
const PPQ = 960;
const TICKS_PER_STEP = 240;
const TICKS_PER_BAR = 3840;
const PATTERN_LENGTHS = [16, 32, 64];
const DEFAULT_STEPS = 16;
const STEP_VELOCITY = 0.78;
const DEFAULT_BPM = 130;
const MIN_BPM = 20;
const MAX_BPM = 999;
/** Mirrors `KIT_ID` in `app/src/project.rs`. */
const KIT_SOURCE_ID = 'sampler:808-kit';
/** Mirrors `KIT_STEP_KEY`: the General MIDI kick, which is what the kit maps. */
const KIT_STEP_KEY = 36;
const SAMPLE_RATE = 48_000;
const FRAME_HZ = 30;
/** Mirrors `MAX_RECORD_SECONDS` in `plugins/audio-io/src/record.rs`. */
const MAX_RECORD_SECONDS = 300;

/**
 * The key each starter channel's grid fires, mirroring `starter_pattern_set`.
 *
 * The kit is on a General MIDI map, so its channel fires the GM kick; anything
 * else fires middle C and plays at recorded pitch.
 */
const DEFAULT_ROW_KEYS = [KIT_STEP_KEY, 60];

// ---------------------------------------------------------------------------
// The effect catalogue, mirrored from `plugins/effects`
// ---------------------------------------------------------------------------
//
// Verbatim, because this is what every knob in the inspector is laid out from:
// a mock catalogue with a different range or curve would let a knob be built
// against a taper the engine does not have.

/** `ParamDef::new` -- continuous, unquantised, 10 ms ramp. */
function param(
  id: string,
  name: string,
  unit: string,
  min: number,
  max: number,
  defaultValue: number,
  curve: ParamDefDto['curve'],
): ParamDefDto {
  return { id, name, unit, min, max, defaultValue, curve, step: null, smoothingMs: 10 };
}

/** `ParamDef::choice` / `::toggle` -- quantised, and **not** smoothed. */
function choice(id: string, name: string, count: number, defaultValue: number): ParamDefDto {
  return {
    id,
    name,
    unit: '',
    min: 0,
    max: count - 1,
    defaultValue,
    curve: 'linear',
    step: 1,
    smoothingMs: 0,
  };
}

function toggle(id: string, name: string, on: boolean): ParamDefDto {
  return choice(id, name, 2, on ? 1 : 0);
}

/**
 * `ParamDef::stepped(..)` / `::smoothed(..)` -- a continuous knob that lands on
 * a grid, or one whose ramp is longer than the standard 10 ms.
 *
 * Both matter to the shell: `step` is what a drag quantises to, and a
 * `smoothingMs` of zero is how the engine declares a switch, so a stepped knob
 * that kept the default ramp would be drawn as a segmented control.
 */
function shaped(
  def: ParamDefDto,
  extra: { step?: number; smoothingMs?: number },
): ParamDefDto {
  return { ...def, ...extra };
}

const EFFECTS: readonly EffectInfoDto[] = [
  {
    typeId: 'effects.eq',
    name: 'EQ',
    description: 'Five bands: highpass, low shelf, two parametric bells and a high shelf',
    params: [
      toggle('bypass', 'Bypass', false),
      toggle('hp.enabled', 'HP On', false),
      param('hp.freq', 'HP Freq', 'Hz', 20, 1000, 80, 'log'),
      choice('hp.slope', 'HP Slope', 2, 1),
      toggle('low.enabled', 'Low On', true),
      param('low.freq', 'Low Freq', 'Hz', 20, 20000, 120, 'log'),
      param('low.gain', 'Low Gain', 'dB', -18, 18, 0, 'db'),
      param('low.q', 'Low Q', '', 0.1, 10, 0.7, 'log'),
      toggle('bell1.enabled', 'Bell 1 On', true),
      param('bell1.freq', 'Bell 1 Freq', 'Hz', 20, 20000, 500, 'log'),
      param('bell1.gain', 'Bell 1 Gain', 'dB', -18, 18, 0, 'db'),
      param('bell1.q', 'Bell 1 Q', '', 0.1, 10, 1, 'log'),
      toggle('bell2.enabled', 'Bell 2 On', true),
      param('bell2.freq', 'Bell 2 Freq', 'Hz', 20, 20000, 3000, 'log'),
      param('bell2.gain', 'Bell 2 Gain', 'dB', -18, 18, 0, 'db'),
      param('bell2.q', 'Bell 2 Q', '', 0.1, 10, 1, 'log'),
      toggle('high.enabled', 'High On', true),
      param('high.freq', 'High Freq', 'Hz', 20, 20000, 8000, 'log'),
      param('high.gain', 'High Gain', 'dB', -18, 18, 0, 'db'),
      param('high.q', 'High Q', '', 0.1, 10, 0.7, 'log'),
      param('output', 'Output', 'dB', -18, 18, 0, 'db'),
    ],
    readouts: [],
  },
  {
    typeId: 'effects.compressor',
    name: 'Compressor',
    description:
      'Feed-forward compressor with peak/RMS detection, soft knee and a sidechain highpass',
    params: [
      toggle('bypass', 'Bypass', false),
      param('threshold', 'Threshold', 'dB', -60, 0, -18, 'db'),
      param('ratio', 'Ratio', ':1', 1, 20, 4, 'log'),
      param('attack', 'Attack', 'ms', 0.05, 100, 10, 'log'),
      param('release', 'Release', 'ms', 5, 2000, 100, 'log'),
      toggle('auto_release', 'Auto Release', false),
      param('knee', 'Knee', 'dB', 0, 18, 6, 'linear'),
      choice('detector', 'Detector', 2, 0),
      param('rms_ms', 'RMS Window', 'ms', 1, 100, 10, 'log'),
      toggle('sc_hpf_on', 'SC Filter', false),
      param('sc_hpf', 'SC Freq', 'Hz', 20, 500, 100, 'log'),
      param('makeup', 'Makeup', 'dB', -24, 24, 0, 'db'),
      toggle('auto_makeup', 'Auto Makeup', false),
      param('mix', 'Mix', '%', 0, 100, 100, 'linear'),
    ],
    readouts: [
      { id: 'gr', name: 'Gain Reduction', unit: 'dB' },
      { id: 'gr_max', name: 'Max Gain Reduction', unit: 'dB' },
    ],
  },
  {
    typeId: 'effects.delay',
    name: 'Delay',
    description:
      'Tempo-synced stereo or ping-pong delay with in-loop damping and one-knob ducking',
    params: [
      toggle('bypass', 'Bypass', false),
      toggle('sync', 'Sync', true),
      choice('division', 'Division', 7, 3),
      choice('modifier', 'Modifier', 3, 0),
      param('time_ms', 'Time', 'ms', 1, 4000, 375, 'log'),
      param('feedback', 'Feedback', '%', 0, 110, 35, 'linear'),
      param('damp_hp', 'Damp HP', 'Hz', 20, 2000, 150, 'log'),
      param('damp_lp', 'Damp LP', 'Hz', 200, 20000, 6500, 'log'),
      choice('mode', 'Mode', 2, 0),
      param('duck', 'Duck', '%', 0, 100, 0, 'linear'),
      param('mix', 'Mix', '%', 0, 100, 30, 'linear'),
    ],
    readouts: [],
  },
  {
    typeId: 'effects.saturator',
    name: 'Saturator',
    description: 'Tape, tube and clip waveshapers with a tilt, level-matched drive and 4x oversampling',
    params: [
      toggle('bypass', 'Bypass', false),
      choice('type', 'Type', 3, 0),
      param('drive', 'Drive', 'dB', 0, 36, 0, 'db'),
      toggle('auto_gain', 'Auto Gain', true),
      param('tone', 'Tone', 'dB', -12, 12, 0, 'db'),
      param('mix', 'Mix', '%', 0, 100, 100, 'linear'),
      param('output', 'Output', 'dB', -24, 24, 0, 'db'),
      choice('oversample', 'Oversampling', 3, 2),
    ],
    readouts: [
      { id: 'auto_gain_db', name: 'Applied Auto Gain', unit: 'dB' },
      { id: 'latency', name: 'Latency', unit: 'samples' },
    ],
  },
  {
    typeId: 'effects.limiter',
    name: 'Limiter',
    description: 'Stereo-linked peak limiter with lookahead. Ceiling is the loudest sample that leaves.',
    params: [
      toggle('bypass', 'Bypass', false),
      param('gain', 'Gain', 'dB', -12, 24, 0, 'db'),
      param('ceiling', 'Ceiling', 'dB', -12, 0, 0, 'db'),
      param('release', 'Release', 'ms', 20, 500, 100, 'log'),
      param('lookahead', 'Lookahead', 'ms', 0.1, 5, 1, 'log'),
    ],
    readouts: [
      { id: 'gr', name: 'Gain Reduction', unit: 'dB' },
      { id: 'gr_max', name: 'Max Gain Reduction', unit: 'dB' },
    ],
  },
  {
    typeId: 'effects.reverb',
    name: 'Reverb',
    description: 'Algorithmic room: decay, size, damp and mix. The display is the space.',
    params: [
      toggle('bypass', 'Bypass', false),
      param('mix', 'Mix', '%', 0, 100, 25, 'linear'),
      param('decay', 'Decay', 's', 0.2, 12, 1.6, 'log'),
      param('size', 'Size', '%', 20, 150, 80, 'linear'),
      param('damp', 'Damp', '%', 0, 100, 40, 'linear'),
      param('predelay', 'Predelay', 'ms', 0, 80, 12, 'linear'),
      param('width', 'Width', '%', 0, 100, 100, 'linear'),
    ],
    readouts: [],
  },
  {
    typeId: 'effects.utility',
    name: 'Utility',
    description: 'Gain, pan, stereo width, mono, polarity and a DC blocker',
    params: [
      toggle('bypass', 'Bypass', false),
      param('gain', 'Gain', 'dB', -36, 36, 0, 'db'),
      param('pan', 'Pan', '', -1, 1, 0, 'linear'),
      param('width', 'Width', '%', 0, 200, 100, 'linear'),
      toggle('mono', 'Mono', false),
      toggle('invert_l', 'Invert L', false),
      toggle('invert_r', 'Invert R', false),
      toggle('dc_block', 'DC Filter', false),
    ],
    readouts: [{ id: 'correlation', name: 'Correlation', unit: '' }],
  },
  {
    typeId: 'effects.chorus',
    name: 'Chorus',
    description: 'Two or three modulated delay voices with stereo spread -- ensemble, not echo',
    params: [
      toggle('bypass', 'Bypass', false),
      param('rate', 'Rate', 'Hz', 0.01, 10, 0.6, 'log'),
      shaped(param('depth', 'Depth', 'ms', 0, 10, 2.5, 'linear'), { smoothingMs: 150 }),
      shaped(param('delay', 'Delay', 'ms', 1, 30, 9, 'log'), { smoothingMs: 150 }),
      choice('voices', 'Voices', 2, 1),
      param('spread', 'Spread', '%', 0, 100, 50, 'linear'),
      param('mix', 'Mix', '%', 0, 100, 50, 'linear'),
    ],
    readouts: [],
  },
  {
    typeId: 'effects.flanger',
    name: 'Flanger',
    description: 'A swept short delay with bipolar feedback -- the comb filter you can hear moving',
    params: [
      toggle('bypass', 'Bypass', false),
      param('rate', 'Rate', 'Hz', 0.01, 10, 0.25, 'log'),
      shaped(param('depth', 'Depth', 'ms', 0, 10, 3, 'linear'), { smoothingMs: 150 }),
      shaped(param('delay', 'Manual', 'ms', 0.1, 10, 1, 'log'), { smoothingMs: 150 }),
      param('feedback', 'Feedback', '%', -95, 95, 40, 'linear'),
      param('spread', 'Spread', '%', 0, 100, 0, 'linear'),
      param('mix', 'Mix', '%', 0, 100, 50, 'linear'),
    ],
    readouts: [],
  },
  {
    typeId: 'effects.phaser',
    name: 'Phaser',
    description: 'Four to eight swept all-pass stages with resonant feedback -- notches that breathe',
    params: [
      toggle('bypass', 'Bypass', false),
      param('rate', 'Rate', 'Hz', 0.01, 10, 0.5, 'log'),
      param('depth', 'Depth', '%', 0, 100, 70, 'linear'),
      shaped(param('freq', 'Centre', 'Hz', 100, 8000, 800, 'log'), { smoothingMs: 150 }),
      choice('stages', 'Stages', 3, 0),
      param('feedback', 'Feedback', '%', 0, 90, 30, 'linear'),
      param('spread', 'Spread', '%', 0, 100, 0, 'linear'),
      param('mix', 'Mix', '%', 0, 100, 50, 'linear'),
    ],
    readouts: [],
  },
  {
    typeId: 'effects.autofilter',
    name: 'Auto Filter',
    description:
      'Resonant lowpass, highpass or bandpass swept by a synced LFO and an envelope follower',
    params: [
      toggle('bypass', 'Bypass', false),
      choice('mode', 'Mode', 3, 0),
      shaped(param('cutoff', 'Cutoff', 'Hz', 30, 20000, 20000, 'log'), { smoothingMs: 60 }),
      param('res', 'Resonance', '%', 0, 100, 0, 'linear'),
      param('lfo_amount', 'LFO Amount', '%', 0, 100, 0, 'linear'),
      toggle('lfo_sync', 'LFO Sync', false),
      param('lfo_rate', 'LFO Rate', 'Hz', 0.01, 20, 1, 'log'),
      choice('lfo_div', 'LFO Division', 7, 2),
      param('spread', 'Spread', '%', 0, 100, 0, 'linear'),
      param('env_amount', 'Env Amount', '%', -100, 100, 0, 'linear'),
      param('env_attack', 'Env Attack', 'ms', 0.1, 100, 5, 'log'),
      param('env_release', 'Env Release', 'ms', 10, 1000, 120, 'log'),
      param('mix', 'Mix', '%', 0, 100, 100, 'linear'),
    ],
    readouts: [
      { id: 'cutoff', name: 'Cutoff', unit: 'Hz' },
      { id: 'env', name: 'Envelope', unit: '' },
    ],
  },
  {
    typeId: 'effects.bitcrusher',
    name: 'Bitcrusher',
    description: 'Bit-depth reduction, sample-rate decimation and a lo-fi filter, with a dry blend',
    params: [
      toggle('bypass', 'Bypass', false),
      shaped(param('bits', 'Bit Depth', 'bits', 1, 24, 24, 'linear'), { step: 1 }),
      shaped(param('downsample', 'Downsample', 'x', 1, 64, 1, 'log'), { step: 1 }),
      shaped(param('tone', 'Filter', 'Hz', 200, 20000, 20000, 'log'), { smoothingMs: 40 }),
      param('mix', 'Mix', '%', 0, 100, 100, 'linear'),
    ],
    readouts: [],
  },
];

function effectInfo(typeId: string): EffectInfoDto {
  const info = EFFECTS.find((entry) => entry.typeId === typeId);
  if (!info) throw new Error(`there is no effect '${typeId}'`);
  return info;
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const MOCK_SNAPSHOT: UiSnapshot = {
  plugins: [
    { id: 'kernel', name: 'Microkernel', state: 'started' },
    { id: 'audio-io', name: 'Audio I/O (WASAPI)', state: 'started' },
    { id: 'metering', name: 'Metering', state: 'started' },
    { id: 'sequencer', name: 'Sequencer', state: 'started' },
    { id: 'instruments', name: 'Internal Instruments', state: 'started' },
    { id: 'effects', name: 'Internal Effects', state: 'started' },
    { id: 'sampler', name: 'Sampler', state: 'started' },
    { id: 'host-clap', name: 'CLAP Host', state: 'registered' },
    { id: 'host-vst3', name: 'VST3 Host', state: 'registered' },
  ],
  panels: [],
};

/** The built-in sources, in the order and spelling `Source::BUILT_IN` lists them. */
const INTERNAL_SOURCES: readonly Omit<SourceDto, 'selected'>[] = [
  {
    id: 'poly-synth',
    label: 'Poly Synth',
    description: 'Eight-voice synth: three analog oscillators into a filter',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  {
    id: 'test-tone',
    label: 'Test Tone',
    description: '440 Hz sine, gated on the transport',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: false,
  },
  {
    id: 'sampler:salamander-piano',
    label: 'Salamander Grand Piano',
    description: 'Yamaha C5 grand, 30 sampled notes x 3 velocity layers',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  // WAVE Factory Vol. 1. In a real session these appear only once their
  // instrument has been downloaded and decoded; the mock offers them all, so
  // browser-only development can reach every source the shell can show.
  {
    id: 'sampler:808-kit',
    label: '808 Kit',
    description: '30 analogue drum-machine one-shots on a General MIDI drum map',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  {
    id: 'sampler:808-bass',
    label: '808 Bass',
    description: 'Long-decay analogue kick played chromatically, C1-B3, two tail lengths',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  {
    id: 'sampler:percussion-fx',
    label: 'Percussion & FX',
    description: 'Hand percussion on a General MIDI map, velocity-layered where recorded',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  {
    id: 'sampler:studio-drums',
    label: 'Studio Drums',
    description: 'Acoustic kit on a General MIDI map, up to four velocity layers per pad',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  {
    id: 'sampler:marimba',
    label: 'Marimba',
    description: 'Five octaves, wholetone-sampled, two dynamics',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
  {
    id: 'sampler:glockenspiel',
    label: 'Glockenspiel',
    description: 'Two and a half octaves, two dynamics',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    takesNotes: true,
  },
];

const FIXTURE_GAIN_ID = 'dev.mydaw.fixture.gain';

const CLAP_PLUGINS: readonly ClapPluginInfoDto[] = [
  {
    id: 'dev.mydaw.fixture.synth',
    name: 'MYDAW Fixture Synth',
    vendor: 'MYDAW',
    version: '0.1.0',
    path: 'D:\\MYDAW\\target\\debug\\mydaw-synth.clap',
    kind: 'instrument',
    features: ['instrument', 'synthesizer', 'stereo'],
  },
  {
    id: 'com.example.polybox',
    name: 'Example PolyBox',
    vendor: 'Example Audio',
    version: '3.0.2',
    path: 'C:\\Program Files\\Common Files\\CLAP\\ExamplePolyBox.clap',
    kind: 'instrument',
    features: ['instrument', 'sampler'],
  },
  {
    id: 'com.example.reverb',
    name: 'Example Reverb',
    vendor: 'Example Audio',
    version: '2.1.0',
    path: 'C:\\Program Files\\Common Files\\CLAP\\ExampleReverb.clap',
    kind: 'audio-effect',
    features: ['audio-effect', 'reverb'],
  },
  {
    // Declares no vendor, no version and one bare feature, all of which CLAP
    // allows -- and it is the one with no editor, so the "this plugin has no
    // editor" path stays reachable in a plain browser.
    id: FIXTURE_GAIN_ID,
    name: 'MYDAW Fixture Gain',
    vendor: '',
    version: '',
    path: 'D:\\MYDAW\\target\\debug\\mydaw-gain.clap',
    kind: 'audio-effect',
    features: ['audio-effect'],
  },
];

const VST3_PLUGINS: readonly Vst3PluginInfoDto[] = [
  {
    id: '58545356736673586572756D00000000',
    name: 'Example Serial Synth',
    vendor: 'Example Records',
    version: '1.4.0',
    path: 'C:\\Program Files\\Common Files\\VST3\\ExampleSerial.vst3',
    kind: 'instrument',
    subcategories: ['Instrument', 'Synth'],
  },
  {
    id: '41545356746E4148564F582041535049',
    name: 'Example Vocal Air',
    vendor: 'Example Audio',
    version: '4.2.1',
    path: 'C:\\Program Files\\Common Files\\VST3\\ExampleVocalAir.vst3',
    kind: 'audio-effect',
    subcategories: ['Fx', 'Pitch Shift'],
  },
  {
    // No `IPluginFactory2`, so the module declares nothing at all.
    id: '0123456789ABCDEF0123456789ABCDEF',
    name: 'Example Legacy Comp',
    vendor: '',
    version: '',
    path: 'C:\\Program Files\\Common Files\\VST3\\ExampleLegacy.vst3',
    kind: 'unknown',
    subcategories: [],
  },
];

const UNTITLED = 'Untitled';
const PROJECT_DIR = 'D:\\Songs';
const MOCK_PACKS = 'D:\\MYDAW\\assets\\packs';
const MOCK_FACTORY = 'D:\\MYDAW\\assets\\instruments';
/** The instrument directories the Factory root lists, and a few of each. */
const MOCK_FACTORY_KITS = {
  '808-kit': ['kick-tone50-decay50.wav', 'snare-tone50-snap25.wav', 'hat-closed.wav'],
  'salamander-piano': ['A0v4.flac', 'C4v8.flac'],
} as const;
/** Where the real build writes takes; see `app/src/recording.rs`. */
const MOCK_RECORDINGS = 'C:\\Users\\wave\\AppData\\Local\\WAVE Studio\\recordings';
const MOCK_DRUMS = [
  'clap.wav',
  'hihat-closed.wav',
  'hihat-open.wav',
  'kick.wav',
  'snare-m.wav',
  'stick-m.wav',
  'tom-m.wav',
] as const;
const MOCK_LOOP_FILES = [
  'KSHMR_Kick_Loop_124.wav',
  'No_Kick_Drum_Loop_123bpm.wav',
  'WarmSynthPad120BPM.flac',
] as const;
const MOCK_ONE_SHOTS = ['PM_GB_Crash_One_Shot_123_BPM.wav', 'VFP_one_shot_snare_96.wav'] as const;
const MOCK_SONGSTARTERS = ['KSHMR_HH_Chill_SS_Better.wav', 'KSHMR_HH_Chill_SS_Dust.wav'] as const;
const MOCK_FOLDERS_KEY = 'mydaw.mock.libraryFolders';

function loadMockFolders(): LibraryRootDto[] {
  try {
    const raw = localStorage.getItem(MOCK_FOLDERS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as LibraryRootDto[];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function saveMockFolders(folders: LibraryRootDto[]): void {
  try {
    localStorage.setItem(MOCK_FOLDERS_KEY, JSON.stringify(folders));
  } catch {
    /* quota / private mode — the session still has the array */
  }
}


/**
 * What the poly synth exposes, mirroring `poly_synth::PARAMS` in the engine.
 *
 * The browser mock has to answer the same shape the engine does, or the synth
 * window would only exist inside Tauri -- which is where it is hardest to look
 * at it.
 */
const SYNTH_PARAMS: readonly ParamDefDto[] = [
  { id: 'wave', name: 'Wave', unit: '', min: 0, max: 4, defaultValue: 0, curve: 'linear', step: 1, smoothingMs: 0 },
  { id: 'osc1Coarse', name: 'Coarse', unit: 'st', min: -24, max: 24, defaultValue: 0, curve: 'linear', step: 1, smoothingMs: 10 },
  { id: 'osc1Fine', name: 'Fine', unit: 'ct', min: -100, max: 100, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 10 },
  { id: 'osc2Wave', name: 'Wave', unit: '', min: 0, max: 4, defaultValue: 0, curve: 'linear', step: 1, smoothingMs: 0 },
  { id: 'osc2Coarse', name: 'Coarse', unit: 'st', min: -24, max: 24, defaultValue: 0, curve: 'linear', step: 1, smoothingMs: 10 },
  { id: 'osc2Fine', name: 'Fine', unit: 'ct', min: -100, max: 100, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 10 },
  { id: 'osc2Mix', name: 'Mix', unit: '', min: 0, max: 1, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 10 },
  { id: 'osc3Wave', name: 'Wave', unit: '', min: 0, max: 4, defaultValue: 2, curve: 'linear', step: 1, smoothingMs: 0 },
  { id: 'osc3Coarse', name: 'Coarse', unit: 'st', min: -24, max: 24, defaultValue: -12, curve: 'linear', step: 1, smoothingMs: 10 },
  { id: 'osc3Fine', name: 'Fine', unit: 'ct', min: -100, max: 100, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 10 },
  { id: 'osc3Mix', name: 'Mix', unit: '', min: 0, max: 1, defaultValue: 0.5, curve: 'linear', step: null, smoothingMs: 10 },
  { id: 'cutoff', name: 'Cutoff', unit: 'Hz', min: 60, max: 18_000, defaultValue: 3_000, curve: 'log', step: null, smoothingMs: 10 },
  { id: 'resonance', name: 'Res', unit: '', min: 0.5, max: 12, defaultValue: 0.8, curve: 'log', step: null, smoothingMs: 10 },
  { id: 'envAmount', name: 'Env', unit: '%', min: -100, max: 100, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 10 },
  { id: 'attack', name: 'Attack', unit: 'ms', min: 1, max: 2_000, defaultValue: 5, curve: 'log', step: null, smoothingMs: 0 },
  { id: 'decay', name: 'Decay', unit: 'ms', min: 1, max: 4_000, defaultValue: 80, curve: 'log', step: null, smoothingMs: 0 },
  { id: 'sustain', name: 'Sustain', unit: '', min: 0, max: 1, defaultValue: 0.6, curve: 'linear', step: null, smoothingMs: 0 },
  { id: 'release', name: 'Release', unit: 'ms', min: 1, max: 4_000, defaultValue: 120, curve: 'log', step: null, smoothingMs: 0 },
];

/**
 * The sampler's knobs. Mirrors `PARAMS` in `plugins/sampler/src/node.rs`, in
 * that order -- the same mock/live parity the synth list above keeps.
 */
const SAMPLER_PARAMS: ParamDefDto[] = [
  { id: 'attack', name: 'Attack', unit: 'ms', min: 0, max: 2_000, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 0 },
  { id: 'decay', name: 'Decay', unit: 'ms', min: 1, max: 8_000, defaultValue: 1_000, curve: 'log', step: null, smoothingMs: 0 },
  { id: 'sustain', name: 'Sustain', unit: '', min: 0, max: 1, defaultValue: 1, curve: 'linear', step: null, smoothingMs: 0 },
  { id: 'release', name: 'Release', unit: 'ms', min: 1, max: 8_000, defaultValue: 300, curve: 'log', step: null, smoothingMs: 0 },
  { id: 'glide', name: 'Glide', unit: 'ms', min: 0, max: 1_000, defaultValue: 0, curve: 'linear', step: null, smoothingMs: 0 },
  { id: 'mono', name: 'Mono', unit: '', min: 0, max: 1, defaultValue: 0, curve: 'linear', step: 1, smoothingMs: 0 },
];

/**
 * Where each installed instrument wants those knobs, by source id.
 *
 * The live engine reads this from the instrument's manifest -- `release_ms` and
 * `default_params` in `assets-manifest.json` -- and applies it when a track
 * selects the instrument. Mirrored here so the browser shows an 808 Bass as the
 * mono, gliding instrument it ships as rather than as a bare sampler.
 */
const SAMPLER_DEFAULTS: Readonly<Record<string, Readonly<Record<string, number>>>> = {
  'sampler:salamander-piano': { release: 400 },
  'sampler:808-kit': { release: 250 },
  'sampler:808-bass': { release: 150, mono: 1, glide: 60 },
  'sampler:percussion-fx': { release: 250 },
  'sampler:studio-drums': { release: 250 },
  'sampler:marimba': { release: 1_200 },
  'sampler:glockenspiel': { release: 1_200 },
};

/** The knobs an instrument declares, by source id. */
function sourceParamDefs(sourceId: string): ParamDefDto[] {
  if (sourceId === 'poly-synth') return SYNTH_PARAMS.map((def) => ({ ...def }));
  // Every sampled instrument has the same knobs; only their starting positions
  // differ. See `source_param_defs` in `app/src/project.rs`.
  if (sourceId.startsWith('sampler:')) return SAMPLER_PARAMS.map((def) => ({ ...def }));
  return [];
}

/** Where a track's knobs start when it selects `sourceId`. */
function sourceParamValues(sourceId: string): ParamValueDto[] {
  const wanted = SAMPLER_DEFAULTS[sourceId] ?? {};
  return sourceParamDefs(sourceId).map((def) => ({
    id: def.id,
    value: wanted[def.id] ?? def.defaultValue,
  }));
}

/** The grid division the engine offers that is closest to `division`. */
function nearestDivision(division: number): number {
  const wanted = Number.isFinite(division) ? division : DEFAULT_SLICE_DIVISION;
  return [4, 8, 16].reduce((best, candidate) =>
    Math.abs(candidate - wanted) < Math.abs(best - wanted) ? candidate : best,
  );
}

/** How many 4/4 bars `span` frames is at `bpm`, rounded and floored at one. */
function assumedBars(span: number, sampleRate: number, bpm: number): number {
  if (!(bpm > 0) || !(sampleRate > 0)) return 1;
  const barFrames = (sampleRate * 4 * 60) / bpm;
  return Math.min(64, Math.max(1, Math.round(span / barFrames)));
}

/** An unsliced channel. Mirrors `SliceStateDto::default()`. */
function noSlices(): SliceState {
  return {
    mode: 'off',
    frames: [],
    sensitivity: DEFAULT_SLICE_SENSITIVITY,
    division: DEFAULT_SLICE_DIVISION,
    keytrack: false,
  };
}

/** Default pitch/region for a mixer strip that is not a sample channel. */
const SAMPLE_PLAY = {
  pitchCents: 0,
  fadeInMs: 0,
  fadeOutMs: 0,
  sampleStartFrames: 0,
  sampleEndFrames: 0,
  rootKey: 60,
  rootFineCents: 0,
  rootMode: 'default' as RootMode,
  detectedPitch: null,
} as const;

/** The instrument-knob half of a strip that has no instrument knobs. */
function noSourceParams(): Pick<TrackDto, 'sourceParamDefs' | 'sourceParams'> {
  return { sourceParamDefs: [], sourceParams: [] };
}
/**
 * The rack a fresh session opens with: a kit channel and a melodic one.
 *
 * The same two `Project::starter` builds -- and both are present here because
 * the mock has every instrument "installed", where a real fresh clone has none
 * and gets the melodic channel alone.
 */
function starterTracks(): TrackDto[] {
  const wanted = [KIT_SOURCE_ID, 'sampler:salamander-piano'];
  return wanted.map((sourceId, index) => {
    const source = INTERNAL_SOURCES.find((entry) => entry.id === sourceId)!;
    return {
      index,
      id: index,
      name: source.label,
      sourceId: source.id,
      sourceLabel: source.label,
      engine: engineOf(source.id),
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: index === 0,
      clap: null,
      vst3: null,
      missing: null,
      inserts: [],
      ...SAMPLE_PLAY,
      slices: noSlices(),
      sourceParamDefs: sourceParamDefs(sourceId),
      sourceParams: sourceParamValues(sourceId),
    };
  });
}

/** An empty pattern with a grid on each of `channels`. */
function emptyPattern(id: number, name: string, channels: readonly number[]): PatternDto {
  return {
    id,
    name,
    steps: DEFAULT_STEPS,
    channels: channels.map<ChannelClipDto>((channel) => ({
      channel,
      mode: 'steps',
      steps: {
        key: DEFAULT_ROW_KEYS[channel] ?? 60,
        // The engine labels a grid row by its key, not by the channel it sits
        // on, so a row is "Key 36" until the roll gives it a name.
        label: '',
        steps: Array.from({ length: DEFAULT_STEPS }, () => ({
          on: false,
          velocity: STEP_VELOCITY,
        })),
      },
      notes: [],
    })),
  };
}

function projectName(path: string): string {
  const file = path.split(/[\\/]/).pop() ?? path;
  const dot = file.lastIndexOf('.');
  // `> 0`, not `>= 0`: a leading dot is a hidden file's name, not an extension.
  return dot > 0 ? file.slice(0, dot) : file;
}

function projectInfo(path: string | null): ProjectInfo {
  return { path, name: path === null ? UNTITLED : projectName(path), missingPlugins: 0, warnings: [] };
}

// ---------------------------------------------------------------------------
// The mock
// ---------------------------------------------------------------------------

interface MockBus {
  id: number;
  name: string;
  color: string | null;
  gainDb: number;
  pan: number;
  muted: boolean;
  soloed: boolean;
  inserts: InsertDto[];
}

interface MockSend {
  id: number;
  sourceKind: 'track' | 'bus';
  sourceIndex: number;
  dest: number;
  level: number;
  pre: boolean;
}

/** Everything a save writes down. */
interface MockSession {
  tracks: TrackDto[];
  buses: MockBus[];
  sends: MockSend[];
  masterGainDb: number;
  masterInserts: InsertDto[];
  masterClap: ClapSlot | null;
  masterVst3: Vst3Slot | null;
  patterns: PatternDto[];
  activePattern: number;
  playlistTracks: PlaylistTrackDto[];
  clips: ClipDto[];
  bpm: number;
  playMode: PlayMode;
  selected: number;
}

export class MockBackend implements Backend {
  readonly kind = 'mock' as const;

  private playing = false;
  private samplePos = 0;
  private bpm = DEFAULT_BPM;
  private playMode: PlayMode = 'pat';
  private looping = true;

  private tracks: TrackDto[] = starterTracks();
  private buses: MockBus[] = [];
  private sends: MockSend[] = [];
  private masterGainDb = 0;
  private masterInserts: InsertDto[] = [];
  private masterClap: ClapSlot | null = null;
  private masterVst3: Vst3Slot | null = null;
  private selected = 0;

  private patterns: PatternDto[] = [emptyPattern(1, 'Pattern 1', [0, 1])];
  private activePattern = 1;
  private playlistTracks: PlaylistTrackDto[] = Array.from({ length: 4 }, (_, index) => ({
    id: index + 1,
    name: `Track ${index + 1}`,
    muted: false,
    soloed: false,
  }));
  private clips: ClipDto[] = [];
  private audioSources: AudioSourceDto[] = [];
  /**
   * Each source's envelope, by id.
   *
   * The mock owns *shapes* rather than samples, and a destructive edit rewrites
   * the shape -- see `envelope`. Filled lazily, so a session that never opens a
   * sample editor never builds one.
   */
  private waveforms = new Map<number, [number, number][]>();

  private nextTrackId = 5;
  private nextBusId = 0;
  private nextSendId = 0;
  private nextInsertId = 0;
  private nextPatternId = 2;
  private nextLaneId = 5;
  private nextClipId = 1;
  /** Ids start at 1: nothing is loaded until a session loads it. */
  private nextAudioId = 1;
  private nextInstance = 1;
  private inventedAudio = 0;
  private extraFolders: LibraryRootDto[] = loadMockFolders();
  private fileSamplers: Omit<SourceDto, 'selected'>[] = [];
  private nextFolder = loadMockFolders().reduce((max, folder) => {
    const match = /^extra-mock-(\d+)$/.exec(folder.id);
    return match ? Math.max(max, Number(match[1]) + 1) : max;
  }, 1);

  private openEditors = new Set<string>();
  private clapStates = new Map<string, string>();
  private project: ProjectInfo = projectInfo(null);
  private saved = new Map<string, MockSession>();
  private lastSavedPath: string | null = null;

  // One row per downloadable instrument, matching `assets-manifest.json`. The
  // piano is installed and the factory kits are not, which is the state the
  // download footer has the most to render: a tick, six buttons, and a
  // progress bar as soon as one is pressed.
  private assets: AssetStatus[] = [
    {
      id: 'salamander-piano',
      name: 'Salamander Grand Piano',
      state: 'ready',
      progress: 1,
      bytesTotal: 148_000_000,
      bytesDone: 148_000_000,
      error: null,
    },
    ...(
      [
        // The kit is bundled -- `tauri.conf.json` ships `assets/instruments` --
        // so it is installed before anyone presses anything, which is what lets
        // the starter rack put it on a channel. The rest are downloads, so the
        // browser footer's Download button still does something visible.
        ['808-kit', '808 Kit', 2_300_000, 'ready'],
        ['808-bass', '808 Bass', 2_200_000, 'absent'],
        ['percussion-fx', 'Percussion & FX', 14_900_000, 'absent'],
        ['studio-drums', 'Studio Drums', 23_000_000, 'absent'],
        ['marimba', 'Marimba', 22_100_000, 'absent'],
        ['glockenspiel', 'Glockenspiel', 9_300_000, 'absent'],
      ] as const
    ).map(([id, name, bytesTotal, state]) => ({
      id,
      name,
      state: state as AssetStatus['state'],
      progress: state === 'ready' ? 1 : 0,
      bytesTotal,
      bytesDone: state === 'ready' ? bytesTotal : 0,
      error: null,
    })),
  ];

  private midiPorts: MidiPortDto[] = [
    { name: 'Mock MIDI Keyboard', connected: false, enabled: false, status: 'available' },
    { name: 'loopMIDI Port', connected: false, enabled: false, status: 'available' },
  ];
  private midiActivityAtMs = 0;
  private previewVolumePercent = 80;
  private preferAsio = false;
  private audioBufferFrames = 256;

  /**
   * A plausible capture rig: the built-in microphone the OS defaults to, an
   * interface at a *different* sample rate (so the rate-conversion copy in
   * Settings has something to be about), and a device the host will not
   * describe — the state that is otherwise impossible to look at.
   */
  private inputDevices: InputDeviceDto[] = [
    { id: 'mock-builtin', name: 'Microphone (Realtek Audio)', channels: 2, sampleRate: 48_000, isDefault: true },
    { id: 'mock-usb', name: 'Focusrite Scarlett Solo', channels: 1, sampleRate: 44_100, isDefault: false },
    { id: 'mock-broken', name: 'Line In (unavailable)', channels: 0, sampleRate: 0, isDefault: false },
  ];
  private inputDeviceId: string | null = null;
  private recordState: RecordState = 'idle';
  /** `performance.now()` when the take started, for the elapsed readout. */
  private recordStartedMs = 0;
  private takesRecorded = 0;
  /** Last Sounds / Audio audition, for tests. Cleared by transport stop. */
  lastPreview: string | null = null;
  lastNoteOn: { key: number; velocity: number } | null = null;
  private spectrumWatch: { target: InsertTarget; index: number | null } | null = null;
  private noteUntilMs = 0;

  async getUiSnapshot(): Promise<UiSnapshot> {
    return structuredClone(MOCK_SNAPSHOT);
  }

  // --- hosted plugins ------------------------------------------------------

  async getClapPlugins(): Promise<ClapPluginInfoDto[]> {
    return structuredClone(CLAP_PLUGINS) as ClapPluginInfoDto[];
  }

  async getVst3Plugins(): Promise<Vst3PluginInfoDto[]> {
    return structuredClone(VST3_PLUGINS) as Vst3PluginInfoDto[];
  }

  async getMasterEffects(): Promise<ClapPluginInfoDto[]> {
    return CLAP_PLUGINS.filter((plugin) => plugin.kind === 'audio-effect').map((p) => ({ ...p }));
  }

  async getVst3MasterEffects(): Promise<Vst3PluginInfoDto[]> {
    return VST3_PLUGINS.filter(
      (plugin) => plugin.kind === 'audio-effect' || plugin.kind === 'unknown',
    ).map((p) => ({ ...p }));
  }

  private static targetKey(target: ClapTarget): string {
    return target.slot === 'master' ? 'master' : `track:${target.index}`;
  }

  private requireClapSlot(target: ClapTarget): ClapSlot {
    const slot = target.slot === 'master' ? this.masterClap : this.track(target.index).clap;
    if (!slot) {
      throw new Error(
        target.slot === 'master'
          ? 'there is no plugin on the master insert'
          : `track ${target.index} has no hosted plugin`,
      );
    }
    return slot;
  }

  private requireVst3Slot(target: ClapTarget): Vst3Slot {
    const slot = target.slot === 'master' ? this.masterVst3 : this.track(target.index).vst3;
    if (!slot) {
      throw new Error(
        target.slot === 'master'
          ? 'there is no VST3 plugin on the master insert'
          : `track ${target.index} has no hosted VST3 plugin`,
      );
    }
    return slot;
  }

  async openClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    const slot = this.requireClapSlot(target);
    if (!slot.hasEditor) throw new Error(`${slot.name} has no editor.`);
    const key = MockBackend.targetKey(target);
    this.openEditors.add(key);
    console.info(`[mock] open_clap_gui ${key}: no native window outside Tauri`);
    return { open: true, hasEditor: true, pluginId: slot.pluginId, width: 400, height: 250 };
  }

  async closeClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    const slot = this.requireClapSlot(target);
    this.openEditors.delete(MockBackend.targetKey(target));
    return { open: false, hasEditor: slot.hasEditor, pluginId: slot.pluginId, width: 0, height: 0 };
  }

  async openVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    const slot = this.requireVst3Slot(target);
    if (!slot.hasEditor) throw new Error('This plugin has no editor.');
    this.openEditors.add(MockBackend.targetKey(target));
    return { open: true, hasEditor: true, cid: slot.cid, width: 800, height: 600 };
  }

  async closeVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    const slot = this.requireVst3Slot(target);
    this.openEditors.delete(MockBackend.targetKey(target));
    return { open: false, hasEditor: slot.hasEditor, cid: slot.cid, width: 0, height: 0 };
  }

  async saveClapState(target: ClapTarget): Promise<string> {
    const slot = this.requireClapSlot(target);
    return btoa(`mock-clap-state:${slot.pluginId}`);
  }

  async loadClapState(target: ClapTarget, stateBase64: string): Promise<void> {
    this.requireClapSlot(target);
    this.clapStates.set(MockBackend.targetKey(target), stateBase64);
  }

  /** Whether one editor is up. Exposed so a view test can assert it. */
  isGuiOpen(target: ClapTarget): boolean {
    return this.openEditors.has(MockBackend.targetKey(target));
  }

  // --- the transport -------------------------------------------------------

  async transportPlay(): Promise<void> {
    this.playing = true;
  }

  async transportStop(): Promise<void> {
    this.playing = false;
    this.lastPreview = null;
    this.noteUntilMs = 0;
  }

  async transportSeek(samplePos: number): Promise<void> {
    this.samplePos = Math.max(0, Math.round(samplePos));
  }

  async setPlayMode(mode: PlayMode): Promise<SequencerState> {
    if (mode !== 'pat' && mode !== 'song') {
      throw new Error(`'${mode}' is not a play mode; expected 'pat' or 'song'`);
    }
    this.playMode = mode;
    return this.sequencerState();
  }

  async setLoop(looping: boolean): Promise<SequencerState> {
    this.looping = looping;
    return this.sequencerState();
  }

  async noteOn(key: number, velocity: number): Promise<void> {
    this.lastNoteOn = { key, velocity };
    this.noteUntilMs = Number.POSITIVE_INFINITY;
    console.info(`[mock] note_on key=${key} velocity=${velocity.toFixed(2)}`);
  }

  async noteOff(key: number): Promise<void> {
    this.noteUntilMs = performance.now() + 500;
    console.info(`[mock] note_off key=${key}`);
  }

  // --- instruments ---------------------------------------------------------

  private sourceList(): SourceDto[] {
    const current = this.tracks[this.selected]?.sourceId;
    const scanned = CLAP_PLUGINS.filter((plugin) => plugin.kind === 'instrument').map<
      Omit<SourceDto, 'selected'>
    >((plugin) => ({
      id: `clap:${plugin.id}`,
      label: plugin.name || plugin.id,
      description: plugin.vendor ? `${plugin.vendor} — ${plugin.id}` : plugin.id,
      kind: 'clap',
      vendor: plugin.vendor,
      bundlePath: plugin.path,
      pluginId: plugin.id,
      takesNotes: true,
    }));
    // A sampled instrument is only offered once its content is installed and
    // decoded -- that is what makes pressing Download in the browser footer do
    // something visible, here as in the real shell.
    const ready = new Set(
      this.assets.filter((asset) => asset.state === 'ready').map((asset) => `sampler:${asset.id}`),
    );
    const builtIn = INTERNAL_SOURCES.filter(
      (source) => !source.id.startsWith('sampler:') || ready.has(source.id),
    );
    // Built-ins, then samplers, then CLAP instruments -- and **no VST3**, which
    // reach the UI through `get_vst3_plugins` instead. Mirrors `state.rs`.
    return [...builtIn, ...this.fileSamplers, ...scanned].map((source) => ({
      ...source,
      selected: source.id === current,
    }));
  }

  async getSources(): Promise<SourceDto[]> {
    return this.sourceList();
  }

  async setSource(id: string): Promise<SourceDto[]> {
    const wanted = this.sourceList().find((source) => source.id === id);
    if (!wanted) throw new Error(`unknown source '${id}'`);
    if (wanted.kind === 'clap' && wanted.bundlePath && wanted.pluginId) {
      await this.setTrackSourceClap(this.selected, wanted.bundlePath, wanted.pluginId);
    } else {
      await this.setTrackSource(this.selected, id);
    }
    return this.getSources();
  }

  async setTrackSource(track: number, source: string): Promise<MixerState> {
    const sampleId = /^oneshot:(\d+)$/.exec(source)?.[1];
    const sample = sampleId
      ? this.audioSources.find((entry) => entry.id === Number(sampleId))
      : undefined;
    const wanted =
      INTERNAL_SOURCES.find((entry) => entry.id === source) ??
      this.fileSamplers.find((entry) => entry.id === source) ??
      (sample
        ? {
            id: source,
            label: sample.name,
            description: sample.path,
            kind: 'internal' as const,
            takesNotes: true,
          }
        : undefined);
    if (!wanted) throw new Error(`unknown source '${source}'`);
    const def = this.track(track);
    def.sourceId = wanted.id;
    def.sourceLabel = wanted.label;
    def.engine = engineOf(wanted.id);
    def.name = wanted.label;
    def.clap = null;
    def.vst3 = null;
    def.audioSourceId = sample?.id ?? null;
    // And a change of file is a change of chop: a slice is a frame position in
    // one file (see `TrackDef::set_source` in the engine).
    def.slices = noSlices();
    // A change of instrument is a change of knobs: the old ones belonged to the
    // old sound (see `TrackDef::set_source` in the engine).
    def.sourceParamDefs = sourceParamDefs(wanted.id);
    def.sourceParams = sourceParamValues(wanted.id);
    this.openEditors.delete(MockBackend.targetKey({ slot: 'track', index: track }));
    return this.mixerState();
  }

  private findClap(bundlePath: string, pluginId: string, kind: ClapPluginInfoDto['kind']) {
    const plugin = CLAP_PLUGINS.find((e) => e.path === bundlePath && e.id === pluginId);
    if (!plugin) throw new Error(`no plugin '${pluginId}' in bundle ${bundlePath}`);
    if (plugin.kind !== kind) {
      throw new Error(`${plugin.name || plugin.id} is a ${plugin.kind}, not a ${kind}`);
    }
    return plugin;
  }

  private newClapSlot(plugin: ClapPluginInfoDto): ClapSlot {
    return {
      instance: this.nextInstance++,
      pluginId: plugin.id,
      name: plugin.name || plugin.id,
      bundlePath: plugin.path,
      hasEditor: plugin.id !== FIXTURE_GAIN_ID,
      guiOpen: false,
    };
  }

  async setTrackSourceClap(
    track: number,
    bundlePath: string,
    pluginId: string,
  ): Promise<MixerState> {
    const plugin = this.findClap(bundlePath, pluginId, 'instrument');
    const def = this.track(track);
    def.clap = this.newClapSlot(plugin);
    def.vst3 = null;
    def.sourceId = `clap:${plugin.id}`;
    def.sourceLabel = def.clap.name;
    def.engine = 'clap';
    def.name = def.clap.name;
    def.audioSourceId = null;
    this.openEditors.delete(MockBackend.targetKey({ slot: 'track', index: track }));
    return this.mixerState();
  }

  private findVst3(modulePath: string, cid: string, wanted: 'instrument' | 'audio-effect') {
    const plugin = VST3_PLUGINS.find((e) => e.path === modulePath && e.id === cid);
    if (!plugin) throw new Error(`no plugin class ${cid} in module ${modulePath}`);
    // `unknown` is accepted as an effect: Rust's bus-layout check is what
    // actually decides, and a stricter rule here would be a fiction.
    const usable =
      plugin.kind === wanted || (wanted === 'audio-effect' && plugin.kind === 'unknown');
    if (!usable) throw new Error(`${plugin.name || plugin.id} is a ${plugin.kind}, not a ${wanted}`);
    return plugin;
  }

  private newVst3Slot(plugin: Vst3PluginInfoDto): Vst3Slot {
    return {
      instance: this.nextInstance++,
      cid: plugin.id,
      name: plugin.name || plugin.id,
      vendor: plugin.vendor,
      modulePath: plugin.path,
      hasEditor: plugin.kind !== 'unknown',
      guiOpen: false,
    };
  }

  async setTrackSourceVst3(track: number, modulePath: string, cid: string): Promise<MixerState> {
    const plugin = this.findVst3(modulePath, cid, 'instrument');
    const def = this.track(track);
    def.vst3 = this.newVst3Slot(plugin);
    def.clap = null;
    def.sourceId = `vst3:${plugin.id}`;
    def.sourceLabel = def.vst3.name;
    def.engine = 'vst3';
    def.name = def.vst3.name;
    def.audioSourceId = null;
    this.openEditors.delete(MockBackend.targetKey({ slot: 'track', index: track }));
    return this.mixerState();
  }

  // --- the mixer -----------------------------------------------------------

  private track(index: number): TrackDto {
    const track = this.tracks[index];
    if (!track) throw new Error(`there is no track ${index}`);
    return track;
  }

  private bus(index: number): MockBus {
    const bus = this.buses[index];
    if (!bus) throw new Error(`there is no bus ${index}`);
    return bus;
  }

  private checkGain(db: number): number {
    if (!Number.isFinite(db) || db < MIN_GAIN_DB || db > MAX_GAIN_DB) {
      throw new Error(`gain ${db} dB is not a number between ${MIN_GAIN_DB} and ${MAX_GAIN_DB}`);
    }
    return db;
  }

  private checkPan(pan: number): number {
    if (!Number.isFinite(pan) || pan < -1 || pan > 1) {
      throw new Error(`pan ${pan} is not a number between -1.0 and 1.0`);
    }
    return pan;
  }

  /** Solo is resolved here, across tracks *and* buses, exactly as Rust does. */
  private mixerState(): MixerState {
    const anySoloed =
      this.tracks.some((track) => track.soloed) || this.buses.some((bus) => bus.soloed);
    const withClapGui = (slot: ClapSlot | null, target: ClapTarget): ClapSlot | null =>
      slot ? { ...slot, guiOpen: this.openEditors.has(MockBackend.targetKey(target)) } : null;
    const withVst3Gui = (slot: Vst3Slot | null, target: ClapTarget): Vst3Slot | null =>
      slot ? { ...slot, guiOpen: this.openEditors.has(MockBackend.targetKey(target)) } : null;

    const master: MasterDto = {
      gainDb: this.masterGainDb,
      clap: withClapGui(this.masterClap, { slot: 'master' }),
      vst3: withVst3Gui(this.masterVst3, { slot: 'master' }),
      missing: null,
      inserts: structuredClone(this.masterInserts),
    };

    return {
      tracks: this.tracks.map((track, index) => ({
        ...track,
        index,
        silenced: anySoloed ? !track.soloed : track.muted,
        selected: index === this.selected,
        clap: withClapGui(track.clap, { slot: 'track', index }),
        vst3: withVst3Gui(track.vst3, { slot: 'track', index }),
        inserts: structuredClone(track.inserts),
      })),
      buses: this.buses.map<BusDto>((bus, index) => ({
        index,
        id: bus.id,
        name: bus.name,
        ...(bus.color === null ? {} : { color: bus.color }),
        gainDb: bus.gainDb,
        pan: bus.pan,
        muted: bus.muted,
        soloed: bus.soloed,
        silenced: anySoloed ? !bus.soloed : bus.muted,
        inserts: structuredClone(bus.inserts),
      })),
      sends: this.sends.map<SendDto>((send) => ({
        id: send.id,
        sourceKind: send.sourceKind,
        sourceIndex: send.sourceIndex,
        dest: send.dest,
        level: send.level,
        levelDb: send.level <= 0 ? MIN_GAIN_DB : 20 * Math.log10(send.level),
        pre: send.pre,
      })),
      master,
      selected: this.selected,
      maxTracks: MAX_TRACKS,
      maxBuses: MAX_BUSES,
      maxInserts: MAX_INSERTS,
      minGainDb: MIN_GAIN_DB,
      maxGainDb: MAX_GAIN_DB,
      maxSendLevel: MAX_SEND_LEVEL,
      maxSendDb: MAX_SEND_DB,
    };
  }

  async getTracks(): Promise<MixerState> {
    return this.mixerState();
  }

  async setTrackGain(track: number, db: number): Promise<MixerState> {
    this.track(track).gainDb = this.checkGain(db);
    return this.mixerState();
  }

  async setTrackPan(track: number, pan: number): Promise<MixerState> {
    this.track(track).pan = this.checkPan(pan);
    return this.mixerState();
  }

  async setTrackPitch(track: number, cents: number): Promise<MixerState> {
    const value = Number.isFinite(cents) ? Math.min(1_200, Math.max(-1_200, cents)) : 0;
    this.track(track).pitchCents = value;
    return this.mixerState();
  }

  async analyzeTrackPitch(track: number): Promise<PitchAnalysis> {
    const entry = this.track(track);
    if (entry.engine !== 'clip' && entry.engine !== 'sample') {
      throw new Error(`track ${track} is not an audio-file channel`);
    }
    const source = this.audioSources.find((item) => item.id === entry.audioSourceId);
    const path = source?.path ?? '';
    const name = path.split(/[\\/]/).pop() ?? path;
    if (/noise/i.test(name)) {
      return { kind: 'noStablePitch', reason: 'unvoiced' };
    }
    if (/tone-A4/i.test(name) || /(^|[^A-Za-z0-9])A4([^A-Za-z0-9]|$)/.test(name.replace(/\.[^.]+$/, ''))) {
      const detected = {
        kind: 'detected' as const,
        midiNote: 69,
        cents: 0,
        confidence: 1,
        voicedCoverage: 1,
        source: /tone-A4/i.test(name) ? ('analysis' as const) : ('filename' as const),
      };
      entry.detectedPitch = detected;
      return detected;
    }
    const token = name.match(/(?:^|[^A-Za-z0-9])([A-Ga-g])([#b]?)_?([0-9])(?:$|[^A-Za-z0-9])/);
    if (token) {
      const letter = token[1]!.toUpperCase();
      const accidental = token[2] === '#' ? 1 : token[2] === 'b' || token[2] === 'B' ? -1 : 0;
      const octave = Number(token[3]);
      const natural = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 }[letter] ?? 0;
      const midi = (octave + 1) * 12 + natural + accidental;
      if (midi >= 0 && midi <= 127) {
        const detected = {
          kind: 'detected' as const,
          midiNote: midi,
          cents: 0,
          confidence: 1,
          voicedCoverage: 1,
          source: 'filename' as const,
        };
        entry.detectedPitch = detected;
        return detected;
      }
    }
    return { kind: 'noStablePitch', reason: 'unvoiced' };
  }

  async setTrackSampleRoot(
    track: number,
    key: number,
    fineCents: number,
    mode: RootMode,
  ): Promise<MixerState> {
    const entry = this.track(track);
    if (entry.engine !== 'clip' && entry.engine !== 'sample') {
      throw new Error(`track ${track} is not an audio-file channel`);
    }
    if (!Number.isInteger(key) || key < 0 || key > 127) {
      throw new Error(`root key ${key} is not a MIDI note 0..=127`);
    }
    if (!Number.isFinite(fineCents)) {
      throw new Error(`root fine cents ${fineCents} is not a finite number between -50 and 50`);
    }
    entry.rootKey = key;
    entry.rootFineCents = Math.min(50, Math.max(-50, fineCents));
    entry.rootMode = mode;
    if (mode === 'detected') {
      entry.detectedPitch = {
        midiNote: key,
        cents: entry.rootFineCents,
        confidence: entry.detectedPitch?.confidence ?? 1,
        voicedCoverage: entry.detectedPitch?.voicedCoverage ?? 1,
        source: entry.detectedPitch?.source ?? 'analysis',
      };
    }
    return this.mixerState();
  }

  /**
   * Where the slices would fall — the mock's stand-in for the Rust detector.
   *
   * Not a spectral-flux pass: there is no PCM in the browser, only the
   * envelope `getAudioWaveform` invents. What it *does* reproduce is every
   * property the window depends on — onsets at plausible places in the file,
   * a sensitivity that adds hits as it rises and never takes them away, the
   * 30 ms minimum gap, the region as the range, the 128-slice ceiling, and the
   * grid's exact division of the assumed bar.
   */
  async analyzeSlices(
    track: number,
    mode: SliceMode,
    sensitivity: number,
    division: number,
  ): Promise<SliceAnalysis> {
    const entry = this.track(track);
    if (entry.engine !== 'clip' && entry.engine !== 'sample') {
      throw new Error(`track ${track} is not an audio-file channel`);
    }
    const source = this.audioSources.find((item) => item.id === entry.audioSourceId);
    if (!source) throw new Error(`audio source ${entry.audioSourceId} is not loaded`);
    const total = source.frames;
    const start = Math.min(Math.max(0, Math.floor(entry.sampleStartFrames)), total);
    const rawEnd = Math.floor(entry.sampleEndFrames);
    const end = rawEnd <= 0 || rawEnd > total ? total : Math.max(start, rawEnd);
    const span = end - start;
    if (span <= 0) {
      return {
        frames: [start],
        mode: mode === 'grid' ? 'grid' : 'transient',
        bars: 0,
        truncated: false,
        rangeStart: start,
        rangeEnd: end,
      };
    }

    if (mode === 'grid') {
      const picked = nearestDivision(division);
      const bars = assumedBars(span, source.sampleRate, this.bpm);
      const wanted = Math.max(1, bars * picked);
      const count = Math.min(wanted, MAX_SLICES);
      return {
        frames: Array.from(
          { length: count },
          (_, index) => start + Math.round((span * index) / wanted),
        ),
        mode: 'grid',
        bars,
        truncated: wanted > count,
        rangeStart: start,
        rangeEnd: end,
      };
    }

    // Sixteen candidate onsets across the range, each with a fixed "strength"
    // that a rising sensitivity progressively admits. The set only ever grows,
    // which is the property the slider's readout promises.
    const level = Number.isFinite(sensitivity) ? Math.min(1, Math.max(0, sensitivity)) : 0.5;
    const minGap = Math.max(1, Math.round((SLICE_MIN_GAP_MS / 1000) * source.sampleRate));
    const candidates = Array.from({ length: 16 }, (_, index) => ({
      frame: start + Math.round((span * index) / 16),
      // Downbeats are strong, offbeats weaker, sixteenths weakest: a plausible
      // break, and deterministic so a test can name the counts.
      strength: index % 4 === 0 ? 0.95 : index % 2 === 0 ? 0.55 : 0.25,
    }));
    const frames: number[] = [start];
    for (const candidate of candidates) {
      if (candidate.strength < 1 - level) continue;
      const last = frames[frames.length - 1]!;
      if (candidate.frame - last < minGap) continue;
      frames.push(candidate.frame);
      if (frames.length >= MAX_SLICES) break;
    }
    return {
      frames,
      mode: 'transient',
      bars: 0,
      truncated: false,
      rangeStart: start,
      rangeEnd: end,
    };
  }

  async setTrackSlices(track: number, slices: SliceState): Promise<SliceState> {
    const entry = this.track(track);
    if (entry.engine !== 'clip' && entry.engine !== 'sample') {
      throw new Error(`track ${track} is not an audio-file channel`);
    }
    const source = this.audioSources.find((item) => item.id === entry.audioSourceId);
    const limit = source?.frames ?? 0xffffffff;
    const frames = [
      ...new Set(
        (slices.frames ?? [])
          .map((frame) => Math.max(0, Math.floor(frame)))
          .filter((frame) => frame <= limit),
      ),
    ]
      .sort((a, b) => a - b)
      .slice(0, MAX_SLICES);
    const sensitivity = Number.isFinite(slices.sensitivity)
      ? Math.min(1, Math.max(0, slices.sensitivity))
      : DEFAULT_SLICE_SENSITIVITY;
    // A mode with nothing in it is not a mode -- the engine says the same.
    const mode: SliceMode = frames.length === 0 ? 'off' : slices.mode;
    entry.slices = {
      mode,
      frames,
      sensitivity,
      division: nearestDivision(slices.division),
      keytrack: !!slices.keytrack,
    };
    return { ...entry.slices, frames: [...frames] };
  }

  async setTrackFades(track: number, fadeInMs: number, fadeOutMs: number): Promise<MixerState> {
    const sane = (ms: number) => (Number.isFinite(ms) ? Math.min(10_000, Math.max(0, ms)) : 0);
    const entry = this.track(track);
    entry.fadeInMs = sane(fadeInMs);
    entry.fadeOutMs = sane(fadeOutMs);
    return this.mixerState();
  }

  async setTrackSampleRegion(
    track: number,
    startFrames: number,
    endFrames: number,
  ): Promise<MixerState> {
    const def = this.track(track);
    def.sampleStartFrames = Math.max(0, Math.floor(startFrames));
    def.sampleEndFrames = Math.max(0, Math.floor(endFrames));
    return this.mixerState();
  }

  async setTrackMute(track: number, muted: boolean): Promise<MixerState> {
    this.track(track).muted = muted;
    return this.mixerState();
  }

  async setTrackSolo(track: number, soloed: boolean): Promise<MixerState> {
    this.track(track).soloed = soloed;
    return this.mixerState();
  }

  async setMasterGain(db: number): Promise<MixerState> {
    this.masterGainDb = this.checkGain(db);
    return this.mixerState();
  }

  async setSelectedTrack(track: number): Promise<MixerState> {
    this.track(track);
    this.selected = track;
    return this.mixerState();
  }

  async addTrack(source: string, name?: string): Promise<MixerState> {
    if (this.tracks.length >= MAX_TRACKS) {
      throw new Error(`a project may have at most ${MAX_TRACKS} tracks`);
    }
    const wanted = INTERNAL_SOURCES.find((entry) => entry.id === source);
    if (!wanted) throw new Error(`unknown source '${source}'`);
    const index = this.tracks.length;
    this.tracks.push({
      index,
      id: this.nextTrackId++,
      name: name ?? wanted.label,
      sourceId: wanted.id,
      sourceLabel: wanted.label,
      engine: engineOf(wanted.id),
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: false,
      clap: null,
      vst3: null,
      missing: null,
      inserts: [],
      ...SAMPLE_PLAY,
      slices: noSlices(),
      sourceParamDefs: sourceParamDefs(wanted.id),
      sourceParams: sourceParamValues(wanted.id),
    });
    // A new rack channel gets a grid in every pattern, the way the engine's
    // `PatternSet::add_channel` does -- otherwise the row would be missing
    // until something else touched the pattern.
    for (const pattern of this.patterns) {
      if (!pattern.channels.some((clip) => clip.channel === index)) {
        pattern.channels.push({
          channel: index,
          mode: 'steps',
          steps: {
            key: DEFAULT_ROW_KEYS[index] ?? 60,
            label: '',
            steps: Array.from({ length: pattern.steps }, () => ({
              on: false,
              velocity: STEP_VELOCITY,
            })),
          },
          notes: [],
        });
      }
    }
    return this.mixerState();
  }

  private pushOneshotTrack(sample: AudioSourceDto, name: string): MixerState {
    const index = this.tracks.length;
    const id = `oneshot:${sample.id}`;
    this.tracks.push({
      index,
      id: this.nextTrackId++,
      name,
      sourceId: id,
      sourceLabel: name,
      engine: 'sample',
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: false,
      clap: null,
      vst3: null,
      missing: null,
      inserts: [],
      ...SAMPLE_PLAY,
      slices: noSlices(),
      ...noSourceParams(),
      audioSourceId: sample.id,
    });
    for (const pattern of this.patterns) {
      if (!pattern.channels.some((clip) => clip.channel === index)) {
        pattern.channels.push({
          channel: index,
          mode: 'steps',
          steps: {
            key: 60,
            label: '',
            steps: Array.from({ length: pattern.steps }, () => ({
              on: false,
              velocity: STEP_VELOCITY,
            })),
          },
          notes: [],
        });
      }
    }
    return this.mixerState();
  }

  async addSampleChannel(path: string): Promise<MixerState> {
    if (this.tracks.length >= MAX_TRACKS) {
      throw new Error(`a project may have at most ${MAX_TRACKS} tracks`);
    }
    const loaded = await this.loadAudioSource(path);
    const name = loaded.name;
    const id = `oneshot:${loaded.id}`;
    const index = this.tracks.length;
    this.tracks.push({
      index,
      id: this.nextTrackId++,
      name,
      sourceId: id,
      sourceLabel: name,
      engine: 'sample',
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: false,
      clap: null,
      vst3: null,
      missing: null,
      inserts: [],
      ...SAMPLE_PLAY,
      slices: noSlices(),
      ...noSourceParams(),
      audioSourceId: loaded.id,
    });
    for (const pattern of this.patterns) {
      if (!pattern.channels.some((clip) => clip.channel === index)) {
        pattern.channels.push({
          channel: index,
          mode: 'steps',
          steps: {
            key: DEFAULT_ROW_KEYS[index] ?? 60,
            label: '',
            steps: Array.from({ length: pattern.steps }, () => ({
              on: false,
              velocity: STEP_VELOCITY,
            })),
          },
          notes: [],
        });
      }
    }
    return this.mixerState();
  }

  async removeTrack(track: number): Promise<MixerState> {
    const removed = this.track(track);
    const selectedBefore = this.selected;
    if (this.tracks.length === 1) throw new Error('a project must keep at least one track');
    if (removed.engine === 'clip' && removed.audioSourceId != null) {
      const sourceId = removed.audioSourceId;
      this.clips = this.clips.filter(
        (clip) => !(clip.kind === 'audio' && clip.sourceId === sourceId),
      );
    }
    this.tracks.splice(track, 1);
    // A track index *is* a note channel, so removing one renumbers every
    // pattern -- the engine does this rather than leaving the arrangement
    // pointing at the wrong instrument.
    for (const pattern of this.patterns) {
      pattern.channels = pattern.channels
        .filter((clip) => clip.channel !== track)
        .map((clip) => (clip.channel > track ? { ...clip, channel: clip.channel - 1 } : clip));
    }
    this.sends = this.sends
      .filter((send) => !(send.sourceKind === 'track' && send.sourceIndex === track))
      .map((send) =>
        send.sourceKind === 'track' && send.sourceIndex > track
          ? { ...send, sourceIndex: send.sourceIndex - 1 }
          : send,
      );
    this.openEditors = MockBackend.reindexEditors(this.openEditors, track);
    this.selected =
      selectedBefore === track
        ? Math.max(0, track - 1)
        : selectedBefore > track
          ? selectedBefore - 1
          : selectedBefore;
    return this.mixerState();
  }

  /** Drop `removed`'s editor key and shift every key above it down by one. */
  private static reindexEditors(open: ReadonlySet<string>, removed: number): Set<string> {
    const next = new Set<string>();
    for (const key of open) {
      if (key === 'master') {
        next.add(key);
        continue;
      }
      const index = Number.parseInt(key.slice('track:'.length), 10);
      if (index === removed) continue;
      next.add(
        MockBackend.targetKey({ slot: 'track', index: index > removed ? index - 1 : index }),
      );
    }
    return next;
  }

  async setMasterEffectClap(bundlePath: string, pluginId: string): Promise<MixerState> {
    this.masterClap = this.newClapSlot(this.findClap(bundlePath, pluginId, 'audio-effect'));
    this.masterVst3 = null;
    this.openEditors.delete('master');
    return this.mixerState();
  }

  async setMasterEffectVst3(modulePath: string, cid: string): Promise<MixerState> {
    this.masterVst3 = this.newVst3Slot(this.findVst3(modulePath, cid, 'audio-effect'));
    this.masterClap = null;
    this.openEditors.delete('master');
    return this.mixerState();
  }

  async clearMasterEffect(): Promise<MixerState> {
    this.masterClap = null;
    this.masterVst3 = null;
    this.openEditors.delete('master');
    return this.mixerState();
  }

  // --- buses, sends and inserts --------------------------------------------

  async getEffects(): Promise<EffectInfoDto[]> {
    return structuredClone(EFFECTS) as EffectInfoDto[];
  }

  async addBus(name?: string): Promise<MixerState> {
    if (this.buses.length >= MAX_BUSES) {
      throw new Error(`a project may have at most ${MAX_BUSES} buses`);
    }
    const id = this.nextBusId++;
    this.buses.push({
      id,
      name: name?.trim() || `Bus ${this.buses.length + 1}`,
      color: null,
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      inserts: [],
    });
    return this.mixerState();
  }

  async removeBus(bus: number): Promise<MixerState> {
    this.bus(bus);
    this.buses.splice(bus, 1);
    // A bus index is a position, and a send left pointing at the bus that moved
    // into the gap would silently reroute somebody's reverb.
    this.sends = this.sends
      .filter(
        (send) => send.dest !== bus && !(send.sourceKind === 'bus' && send.sourceIndex === bus),
      )
      .map((send) => ({
        ...send,
        dest: send.dest > bus ? send.dest - 1 : send.dest,
        sourceIndex:
          send.sourceKind === 'bus' && send.sourceIndex > bus
            ? send.sourceIndex - 1
            : send.sourceIndex,
      }));
    return this.mixerState();
  }

  async renameBus(bus: number, name: string): Promise<MixerState> {
    if (!name.trim()) throw new Error('a bus needs a name');
    this.bus(bus).name = name.trim();
    return this.mixerState();
  }

  async setBusColor(bus: number, color: string | null): Promise<MixerState> {
    this.bus(bus).color = color && color.trim() ? color.trim() : null;
    return this.mixerState();
  }

  async setBusGain(bus: number, db: number): Promise<MixerState> {
    this.bus(bus).gainDb = this.checkGain(db);
    return this.mixerState();
  }

  async setBusPan(bus: number, pan: number): Promise<MixerState> {
    this.bus(bus).pan = this.checkPan(pan);
    return this.mixerState();
  }

  async setBusMute(bus: number, muted: boolean): Promise<MixerState> {
    this.bus(bus).muted = muted;
    return this.mixerState();
  }

  async setBusSolo(bus: number, soloed: boolean): Promise<MixerState> {
    this.bus(bus).soloed = soloed;
    return this.mixerState();
  }

  /** Every bus `from` reaches, directly or through others. */
  private reaches(from: number, seen = new Set<number>()): Set<number> {
    for (const send of this.sends) {
      if (send.sourceKind !== 'bus' || send.sourceIndex !== from) continue;
      if (seen.has(send.dest)) continue;
      seen.add(send.dest);
      this.reaches(send.dest, seen);
    }
    return seen;
  }

  async addSend(
    sourceKind: 'track' | 'bus',
    sourceIndex: number,
    dest: number,
  ): Promise<MixerState> {
    this.bus(dest);
    if (sourceKind === 'track') this.track(sourceIndex);
    else this.bus(sourceIndex);
    if (sourceKind === 'bus' && sourceIndex === dest) {
      throw new Error(`bus ${this.bus(dest).name} cannot send to itself`);
    }
    if (
      this.sends.some(
        (send) =>
          send.sourceKind === sourceKind && send.sourceIndex === sourceIndex && send.dest === dest,
      )
    ) {
      throw new Error('that send already exists');
    }
    if (sourceKind === 'bus' && this.reaches(dest).has(sourceIndex)) {
      throw new Error(
        `that would feed ${this.bus(sourceIndex).name} back into itself through ${this.bus(dest).name}`,
      );
    }
    this.sends.push({ id: this.nextSendId++, sourceKind, sourceIndex, dest, level: 1, pre: false });
    return this.mixerState();
  }

  private send(sendId: number): MockSend {
    const send = this.sends.find((entry) => entry.id === sendId);
    if (!send) throw new Error(`there is no send ${sendId}`);
    return send;
  }

  async removeSend(sendId: number): Promise<MixerState> {
    this.send(sendId);
    this.sends = this.sends.filter((send) => send.id !== sendId);
    return this.mixerState();
  }

  async setSendLevel(sendId: number, level: number): Promise<void> {
    if (!Number.isFinite(level) || level < 0 || level > MAX_SEND_LEVEL) {
      throw new Error(`send level ${level} is not between 0 and ${MAX_SEND_LEVEL}`);
    }
    this.send(sendId).level = level;
  }

  async setSendPre(sendId: number, pre: boolean): Promise<void> {
    this.send(sendId).pre = pre;
  }

  private chain(target: InsertTarget): InsertDto[] {
    switch (target.kind) {
      case 'master':
        return this.masterInserts;
      case 'bus':
        return this.bus(target.index).inserts;
      default:
        return this.track(target.index).inserts;
    }
  }

  /** Renumber a chain after an edit: `index` is a position, and positions move. */
  private static renumber(chain: InsertDto[]): void {
    chain.forEach((insert, index) => {
      insert.index = index;
    });
  }

  async addInsert(
    target: InsertTarget,
    typeId: string,
    position?: number,
  ): Promise<MixerState> {
    const info = effectInfo(typeId);
    const chain = this.chain(target);
    if (chain.length >= MAX_INSERTS) {
      throw new Error(`a chain may hold at most ${MAX_INSERTS} effects`);
    }
    const insert: InsertDto = {
      index: 0,
      id: this.nextInsertId++,
      typeId,
      name: info.name,
      bypassed: false,
      params: info.params.map<ParamValueDto>((def) => ({ id: def.id, value: def.defaultValue })),
      readouts: info.readouts.map<ParamValueDto>((def) => ({ id: def.id, value: 0 })),
      sidechain: null,
    };
    const at = position === undefined ? chain.length : Math.min(chain.length, Math.max(0, position));
    chain.splice(at, 0, insert);
    MockBackend.renumber(chain);
    return this.mixerState();
  }

  async removeInsert(target: InsertTarget, index: number): Promise<MixerState> {
    const chain = this.chain(target);
    if (!chain[index]) throw new Error(`there is no insert ${index} on that chain`);
    chain.splice(index, 1);
    MockBackend.renumber(chain);
    return this.mixerState();
  }

  async moveInsert(target: InsertTarget, from: number, to: number): Promise<MixerState> {
    const chain = this.chain(target);
    const insert = chain[from];
    if (!insert) throw new Error(`there is no insert ${from} on that chain`);
    if (to < 0 || to >= chain.length) throw new Error(`there is no position ${to} on that chain`);
    chain.splice(from, 1);
    chain.splice(to, 0, insert);
    MockBackend.renumber(chain);
    return this.mixerState();
  }

  async setInsertParam(
    target: InsertTarget,
    index: number,
    paramId: string,
    value: number,
  ): Promise<number> {
    const chain = this.chain(target);
    const insert = chain[index];
    if (!insert) throw new Error(`there is no insert ${index} on that chain`);
    const def = effectInfo(insert.typeId).params.find((entry) => entry.id === paramId);
    if (!def) throw new Error(`${insert.name} has no parameter '${paramId}'`);
    // Clamp, then snap to the step grid, and answer with what was stored -- the
    // whole reason this command has a return value.
    const clamped = Math.min(def.max, Math.max(def.min, value));
    const stored =
      def.step && def.step > 0
        ? Math.min(def.max, def.min + Math.round((clamped - def.min) / def.step) * def.step)
        : clamped;
    const slot = insert.params.find((entry) => entry.id === paramId);
    if (slot) slot.value = stored;
    else insert.params.push({ id: paramId, value: stored });
    if (paramId === 'bypass') insert.bypassed = stored >= 0.5;
    return stored;
  }

  async setSourceParam(track: number, paramId: string, value: number): Promise<number> {
    const entry = this.track(track);
    const def = entry.sourceParamDefs.find((candidate) => candidate.id === paramId);
    if (!def) throw new Error(`${entry.name} has no parameter '${paramId}'`);
    const clamped = Math.min(def.max, Math.max(def.min, value));
    const stored =
      def.step && def.step > 0
        ? Math.min(def.max, def.min + Math.round((clamped - def.min) / def.step) * def.step)
        : clamped;
    const slot = entry.sourceParams.find((candidate) => candidate.id === paramId);
    if (slot) slot.value = stored;
    else entry.sourceParams.push({ id: paramId, value: stored });
    return stored;
  }

  async setInsertBypass(
    target: InsertTarget,
    index: number,
    bypassed: boolean,
  ): Promise<MixerState> {
    const insert = this.chain(target)[index];
    if (!insert) throw new Error(`there is no insert ${index} on that chain`);
    insert.bypassed = bypassed;
    const slot = insert.params.find((entry) => entry.id === 'bypass');
    if (slot) slot.value = bypassed ? 1 : 0;
    return this.mixerState();
  }

  /**
   * Plausible meters: a compressor's gain reduction breathes while the
   * transport runs and rests at zero when it does not, so the bar in the
   * inspector is worth looking at without an engine behind it.
   */
  async getChainReadouts(target: InsertTarget): Promise<InsertReadoutsDto[]> {
    const now = performance.now() / 1000;
    return this.chain(target).map<InsertReadoutsDto>((insert, index) => ({
      index,
      id: insert.id,
      readouts: effectInfo(insert.typeId).readouts.map<ParamValueDto>((def) => {
        if (!this.playing || insert.bypassed) return { id: def.id, value: 0 };
        if (def.id === 'gr' || def.id === 'gr_max') {
          const peak = def.id === 'gr_max';
          if (insert.sidechain) {
            // Keyed: the reduction follows the key's own envelope, so the
            // display's two traces line up the way they do with an engine
            // behind them. That is the whole point of the demo -- a duck you
            // can see arriving on the beat.
            const duck = 18 * MockBackend.keyEnvelope(now, index);
            return { id: def.id, value: peak ? duck : duck * 0.86 };
          }
          const base = peak ? 4 + 5 * Math.abs(Math.sin(now * 1.4 + index)) : 3 + 4 * Math.abs(Math.sin(now * 2.1 + index));
          return { id: def.id, value: base };
        }
        if (def.id === 'latency') return { id: def.id, value: 32 };
        if (def.id === 'auto_gain_db') return { id: def.id, value: -((this.playing ? 6 : 0) as number) };
        if (def.id === 'correlation') return { id: def.id, value: 0.72 };
        // The auto-filter's two: a cutoff that breathes an octave either side
        // of where the knob is, and the follower that is moving it.
        if (def.id === 'cutoff') {
          const knob =
            insert.params.find((entry) => entry.id === 'cutoff')?.value ?? 20_000;
          return { id: def.id, value: knob * 2 ** Math.sin(now * 1.6 + index) };
        }
        if (def.id === 'env') {
          return { id: def.id, value: 0.5 + 0.45 * Math.sin(now * 2.7 + index) };
        }
        return { id: def.id, value: 0 };
      }),
    }));
  }

  /**
   * A four-to-the-floor envelope, 0..1, for the mock's key signal.
   *
   * Two beats a second with a fast attack and a ~180 ms tail: recognisably a
   * kick, so a screenshot of the editor shows the shape a reader expects rather
   * than an abstract wobble.
   */
  private static keyEnvelope(now: number, offset: number): number {
    const phase = (now * 2 + offset * 0.13) % 1;
    return Math.exp(-phase * 5.5) * (1 - Math.exp(-phase * 90));
  }


  async watchSpectrum(target: InsertTarget | null, index: number | null): Promise<void> {
    this.spectrumWatch = target ? { target, index } : null;
  }

  async getInsertSpectrum(): Promise<SpectrumDto | null> {
    if (!this.spectrumWatch) return null;
    const bins = 128;
    const now = performance.now() / 700;
    const binsPost = Array.from({ length: bins }, (_, i) => {
      const t = i / (bins - 1);
      const body = -10 - t * 42;
      const kick = 14 * Math.exp(-t * t * 18) * (0.55 + 0.45 * Math.sin(now * 3.2));
      const presence = 8 * Math.exp(-((t - 0.62) ** 2) * 40) * (0.4 + 0.6 * Math.abs(Math.sin(now * 1.7)));
      const air = 5 * Math.max(0, t - 0.78) * (0.5 + 0.5 * Math.sin(now * 2.4));
      return Math.max(-90, body + kick + presence + air);
    });
    const sounding = this.playing || performance.now() < this.noteUntilMs;
    const amp = sounding ? 0.4 : 0;
    const scope = Array.from({ length: 64 }, (_, i) => Math.sin(now + i * 0.2) * amp);
    return {
      binsPre: binsPost.map((d) => d - 3),
      binsPost,
      scopeL: scope,
      scopeR: scope.map((s, i) => s * 0.7 + Math.cos(now + i * 0.15) * 0.2),
      correlation: 0.72,
    };
  }

  async setInsertListen(_target: InsertTarget, _index: number, _band: string | null): Promise<void> {
    // The mock has no audio path; solo is a no-op that must still resolve.
  }

  async setInsertSidechain(
    target: InsertTarget,
    index: number,
    source: StripRef | null,
  ): Promise<MixerState> {
    const insert = this.chain(target)[index];
    if (!insert) throw new Error(`there is no insert ${index} on that chain`);
    if (source) {
      const exists =
        source.kind === 'track'
          ? source.index < this.tracks.length
          : source.index < this.buses.length;
      if (!exists) {
        throw new Error(`there is no ${source.kind} ${source.index} to key off`);
      }
    }
    insert.sidechain = source;
    return this.mixerState();
  }

  // --- patterns ------------------------------------------------------------

  private pattern(id: number): PatternDto {
    const pattern = this.patterns.find((entry) => entry.id === id);
    if (!pattern) throw new Error(`there is no pattern ${id}`);
    return pattern;
  }

  private patternSet(): PatternSetDto {
    return {
      patterns: this.patterns.map((pattern) => ({
        id: pattern.id,
        name: pattern.name,
        ...(pattern.color ? { color: pattern.color } : {}),
        steps: pattern.steps,
        lengthTicks: pattern.steps * TICKS_PER_STEP,
        channelCount: pattern.channels.length,
        hitCount: pattern.channels.reduce(
          (total, clip) => total + (clip.steps?.steps.filter((cell) => cell.on).length ?? 0),
          0,
        ),
        noteCount: pattern.channels.reduce((total, clip) => total + clip.notes.length, 0),
        voiceCount: occupiedChannels(pattern).length,
      })),
      activeId: this.activePattern,
      active: this.derivedPattern(this.pattern(this.activePattern)),
      maxPatterns: MAX_PATTERNS,
    };
  }

  /** `mode` is derived on the way out, exactly as `ChannelClipDto::of` does. */
  private derivedPattern(pattern: PatternDto): PatternDto {
    return structuredClone({
      ...pattern,
      channels: pattern.channels.map((clip) => ({
        ...clip,
        mode: clip.notes.length > 0 ? ('roll' as const) : ('steps' as const),
      })),
    });
  }

  private playlist(): PlaylistDto {
    const endTicks = this.clips.reduce((end, clip) => Math.max(end, clip.start + clip.length), 0);
    return {
      tracks: this.playlistTracks.map((track) => ({ ...track })),
      clips: this.clips.map((clip) => ({ ...clip })),
      endTicks,
      maxTracks: MAX_PLAYLIST_TRACKS,
      maxClips: MAX_CLIPS,
      maxTicks: MAX_SONG_TICKS,
    };
  }

  private sequencerState(): SequencerState {
    return {
      bpm: this.bpm,
      sampleRate: SAMPLE_RATE,
      playMode: this.playMode,
      looping: this.looping,
      ppq: PPQ,
      ticksPerStep: TICKS_PER_STEP,
      ticksPerBar: TICKS_PER_BAR,
      patternSet: this.patternSet(),
      playlist: this.playlist(),
    };
  }

  async getPattern(): Promise<SequencerState> {
    return this.sequencerState();
  }

  async getPatternContent(patternId: number): Promise<PatternDto> {
    const pattern = this.patterns.find((entry) => entry.id === patternId);
    // Refused rather than answered with an empty pattern, exactly as the engine
    // does: a clip pointing at a pattern that is gone is a bug worth seeing.
    if (!pattern) throw new Error(`there is no pattern ${patternId}`);
    return structuredClone(pattern);
  }

  /** Validates the way `Pattern::try_from` does, so the error path is reachable. */
  async setPattern(pattern: PatternDto): Promise<SequencerState> {
    if (!PATTERN_LENGTHS.includes(pattern.steps)) {
      throw new Error(`a pattern must be 16, 32 or 64 steps long, got ${pattern.steps}`);
    }
    if (pattern.channels.length > MAX_TRACKS) {
      throw new Error(`a pattern may speak for at most ${MAX_TRACKS} channels`);
    }
    const seen = new Set<number>();
    const limit = pattern.steps * TICKS_PER_STEP;
    for (const clip of pattern.channels) {
      if (seen.has(clip.channel)) throw new Error(`two clips on channel ${clip.channel}`);
      seen.add(clip.channel);
      if (clip.steps && clip.steps.steps.length !== pattern.steps) {
        throw new Error(
          `channel ${clip.channel} sent ${clip.steps.steps.length} cells for a ${pattern.steps}-step pattern`,
        );
      }
      if (clip.steps && (clip.steps.key < 0 || clip.steps.key > 127)) {
        throw new Error(`channel ${clip.channel} has key ${clip.steps.key}, outside 0..127`);
      }
      if (clip.notes.length > MAX_NOTES) {
        throw new Error(`channel ${clip.channel} has more than ${MAX_NOTES} notes`);
      }
      for (const note of clip.notes) {
        if (note.key < 0 || note.key > 127) throw new Error(`note key ${note.key} is outside 0..127`);
        if (note.start < 0 || note.length < 1 || note.start + note.length > limit) {
          throw new Error(`a note does not fit a ${limit}-tick pattern`);
        }
        if (!Number.isFinite(note.velocity) || note.velocity < 0 || note.velocity > 1) {
          throw new Error(`velocity ${note.velocity} is outside 0..1`);
        }
      }
    }
    const id = pattern.id === 0 ? this.activePattern : pattern.id;
    const existing = this.pattern(id);
    const index = this.patterns.indexOf(existing);
    this.patterns[index] = structuredClone({ ...pattern, id, name: pattern.name || existing.name });
    return this.sequencerState();
  }

  async addPattern(name?: string): Promise<SequencerState> {
    if (this.patterns.length >= MAX_PATTERNS) {
      throw new Error(`a project may hold at most ${MAX_PATTERNS} patterns`);
    }
    const id = this.nextPatternId++;
    this.patterns.push(
      emptyPattern(
        id,
        name?.trim() || `Pattern ${id}`,
        this.tracks.map((_, index) => index),
      ),
    );
    return this.sequencerState();
  }

  async clonePattern(patternId: number, name?: string): Promise<SequencerState> {
    const source = this.pattern(patternId);
    if (this.patterns.length >= MAX_PATTERNS) {
      throw new Error(`a project may hold at most ${MAX_PATTERNS} patterns`);
    }
    const id = this.nextPatternId++;
    const copy = structuredClone({ ...source, id, name: name?.trim() || `${source.name} copy` });
    this.patterns.splice(this.patterns.indexOf(source) + 1, 0, copy);
    return this.sequencerState();
  }

  async splitPatternByChannel(patternId: number): Promise<SequencerState> {
    const source = this.pattern(patternId);
    const voices = occupiedChannels(source);
    if (voices.length < 2) {
      throw new Error('split by channel needs a pattern that speaks for more than one channel');
    }
    const extra = voices.length - 1;
    if (this.patterns.length + extra > MAX_PATTERNS) {
      throw new Error(`a project may have at most ${MAX_PATTERNS} patterns`);
    }
    const instances = this.clips
      .filter((clip) => clip.kind === 'pattern' && clip.patternId === patternId)
      .map((clip) => ({ track: clip.track, start: clip.start, length: clip.length }));
    const needed = instances.reduce((max, clip) => Math.max(max, clip.track + extra), 0);
    if (instances.length > 0 && needed >= MAX_PLAYLIST_TRACKS) {
      throw new Error(`a playlist may have at most ${MAX_PLAYLIST_TRACKS} tracks`);
    }
    if (this.clips.length + instances.length * extra > MAX_CLIPS) {
      throw new Error(`a playlist may hold at most ${MAX_CLIPS} clips`);
    }

    const baseName = source.name;
    const copies: PatternDto[] = voices.slice(1).map((channel) => {
      const id = this.nextPatternId++;
      return structuredClone({
        ...source,
        id,
        name: `${baseName} - ${this.channelLabel(channel)}`,
        channels: source.channels.filter((clip) => clip.channel === channel),
      });
    });
    source.name = `${baseName} - ${this.channelLabel(voices[0]!)}`;
    source.channels = source.channels.filter((clip) => clip.channel === voices[0]);
    this.patterns.splice(this.patterns.indexOf(source) + 1, 0, ...copies);

    const ids = [patternId, ...copies.map((pattern) => pattern.id)];
    while (instances.length > 0 && this.playlistTracks.length <= needed) {
      this.playlistTracks.push({
        id: this.nextLaneId++,
        name: `Track ${this.playlistTracks.length + 1}`,
        muted: false,
        soloed: false,
      });
    }
    for (const instance of instances) {
      for (let offset = 1; offset < ids.length; offset += 1) {
        const splitId = ids[offset];
        if (splitId === undefined) continue;
        this.clips.push({
          id: this.nextClipId++,
          kind: 'pattern',
          track: instance.track + offset,
          start: instance.start,
          length: instance.length,
          patternId: splitId,
        });
      }
    }
    return this.sequencerState();
  }

  private channelLabel(channel: number): string {
    const name = this.tracks[channel]?.name.trim();
    return name || `Channel ${channel + 1}`;
  }

  async removePattern(patternId: number): Promise<SequencerState> {
    this.pattern(patternId);
    if (this.patterns.length === 1) throw new Error('a project must keep at least one pattern');
    this.patterns = this.patterns.filter((pattern) => pattern.id !== patternId);
    // The clips go with it: a set without the pattern and an arrangement still
    // naming it is a state no caller should be able to publish.
    this.clips = this.clips.filter((clip) => clip.patternId !== patternId);
    if (this.activePattern === patternId) this.activePattern = this.patterns[0]!.id;
    return this.sequencerState();
  }

  async renamePattern(patternId: number, name: string): Promise<SequencerState> {
    if (!name.trim()) throw new Error('a pattern needs a name');
    this.pattern(patternId).name = name.trim();
    return this.sequencerState();
  }

  async setPatternColor(patternId: number, color: string | null): Promise<SequencerState> {
    const pattern = this.pattern(patternId);
    if (color && color.trim()) pattern.color = color.trim();
    else delete pattern.color;
    return this.sequencerState();
  }

  async setPatternLength(patternId: number, steps: number): Promise<SequencerState> {
    if (!PATTERN_LENGTHS.includes(steps)) {
      throw new Error(`a pattern must be 16, 32 or 64 steps long, got ${steps}`);
    }
    const pattern = this.pattern(patternId);
    const limit = steps * TICKS_PER_STEP;
    for (const clip of pattern.channels) {
      const hanging = clip.notes.find((note) => note.start < limit && note.start + note.length > limit);
      if (hanging) {
        throw new Error(
          `a note on channel ${clip.channel} would hang past the end of a ${steps}-step pattern`,
        );
      }
    }
    // Shortening is not destructive: cells past the new end are kept and simply
    // not played, so lengthening again gives them back.
    for (const clip of pattern.channels) {
      if (!clip.steps) continue;
      const cells = clip.steps.steps.slice(0, steps);
      while (cells.length < steps) cells.push({ on: false, velocity: STEP_VELOCITY });
      clip.steps = { ...clip.steps, steps: cells };
    }
    pattern.steps = steps;
    return this.sequencerState();
  }

  async setActivePattern(patternId: number): Promise<SequencerState> {
    this.pattern(patternId);
    this.activePattern = patternId;
    return this.sequencerState();
  }

  async setBpm(bpm: number): Promise<number> {
    if (!Number.isFinite(bpm) || bpm < MIN_BPM || bpm > MAX_BPM) {
      throw new Error(`tempo ${bpm} is not a number between ${MIN_BPM} and ${MAX_BPM}`);
    }
    this.bpm = bpm;
    return this.bpm;
  }

  // --- the playlist --------------------------------------------------------

  private lane(index: number): PlaylistTrackDto {
    const lane = this.playlistTracks[index];
    if (!lane) throw new Error(`there is no playlist track ${index}`);
    return lane;
  }

  async addPlaylistTrack(name?: string): Promise<SequencerState> {
    if (this.playlistTracks.length >= MAX_PLAYLIST_TRACKS) {
      throw new Error(`an arrangement may have at most ${MAX_PLAYLIST_TRACKS} lanes`);
    }
    this.playlistTracks.push({
      id: this.nextLaneId++,
      name: name?.trim() || `Track ${this.playlistTracks.length + 1}`,
      muted: false,
      soloed: false,
    });
    return this.sequencerState();
  }

  async removePlaylistTrack(track: number): Promise<SequencerState> {
    this.lane(track);
    if (this.playlistTracks.length === 1) {
      throw new Error('an arrangement must keep at least one lane');
    }
    this.playlistTracks.splice(track, 1);
    this.clips = this.clips
      .filter((clip) => clip.track !== track)
      .map((clip) => (clip.track > track ? { ...clip, track: clip.track - 1 } : clip));
    return this.sequencerState();
  }

  async renamePlaylistTrack(track: number, name: string): Promise<SequencerState> {
    if (!name.trim()) throw new Error('a lane needs a name');
    this.lane(track).name = name.trim();
    return this.sequencerState();
  }

  async setPlaylistTrackMute(track: number, muted: boolean): Promise<SequencerState> {
    this.lane(track).muted = muted;
    return this.sequencerState();
  }

  async setPlaylistTrackSolo(track: number, soloed: boolean): Promise<SequencerState> {
    this.lane(track).soloed = soloed;
    return this.sequencerState();
  }

  private checkClipBounds(start: number, length: number): void {
    if (!Number.isInteger(start) || start < 0) throw new Error(`${start} is not a start tick`);
    if (!Number.isInteger(length) || length <= 0) throw new Error('a clip cannot be zero long');
    if (start + length > MAX_SONG_TICKS) throw new Error('that clip runs past the end of the song');
    if (this.clips.length >= MAX_CLIPS) {
      throw new Error(`an arrangement may hold at most ${MAX_CLIPS} clips`);
    }
  }

  async addPatternClip(
    patternId: number,
    track: number,
    start: number,
    length?: number,
  ): Promise<SequencerState> {
    this.pattern(patternId);
    this.lane(track);
    const span = length ?? this.pattern(patternId).steps * TICKS_PER_STEP;
    this.checkClipBounds(start, span);
    this.clips.push({ id: this.nextClipId++, kind: 'pattern', track, start, length: span, patternId });
    return this.sequencerState();
  }

  private clip(clipId: number): ClipDto {
    const clip = this.clips.find((entry) => entry.id === clipId);
    if (!clip) throw new Error(`there is no clip ${clipId}`);
    return clip;
  }

  async removeClip(clipId: number): Promise<SequencerState> {
    this.clip(clipId);
    this.clips = this.clips.filter((clip) => clip.id !== clipId);
    return this.sequencerState();
  }

  async moveClip(clipId: number, track: number, start: number): Promise<SequencerState> {
    const clip = this.clip(clipId);
    this.lane(track);
    if (!Number.isInteger(start) || start < 0) throw new Error(`${start} is not a start tick`);
    if (start + clip.length > MAX_SONG_TICKS) throw new Error('that clip runs past the end');
    clip.track = track;
    clip.start = start;
    return this.sequencerState();
  }

  async resizeClip(clipId: number, length: number): Promise<SequencerState> {
    const clip = this.clip(clipId);
    if (!Number.isInteger(length) || length <= 0) throw new Error('a clip cannot be zero long');
    clip.length = length;
    return this.sequencerState();
  }

  // --- audio files ---------------------------------------------------------

  /** How long a file is at the session's current tempo, rounded up. */
  private ticksFor(seconds: number): number {
    return Math.ceil((seconds * this.bpm * PPQ) / 60);
  }

  async loadAudioSource(path: string): Promise<AudioSourceDto> {
    const already = this.audioSources.find((source) => source.path === path);
    if (already) return { ...already, durationTicks: this.ticksFor(already.durationSeconds) };
    const name = path.split(/[\\/]/).pop() ?? path;
    // A deterministic length from the name, so the same invented file is the
    // same length every session.
    const seconds = 1.5 + (name.length % 7);
    const source: AudioSourceDto = {
      id: this.nextAudioId++,
      path,
      name,
      frames: Math.round(seconds * 44_100),
      sampleRate: 44_100,
      durationSeconds: seconds,
      durationTicks: this.ticksFor(seconds),
      missing: false,
      bytes: Math.round(seconds * 44_100 * 4),
    };
    this.audioSources.push(source);
    return { ...source };
  }

  /**
   * There is no file dialog outside Tauri, so one is invented -- the same trade
   * the project's Save As makes. A dialog that could never succeed would leave
   * audio clips unreachable in a plain browser, which is the one thing the mock
   * exists to prevent.
   */
  async openAudioFile(): Promise<AudioSourceDto | null> {
    this.inventedAudio += 1;
    const path = `${PROJECT_DIR}\\takes\\take-${this.inventedAudio}.wav`;
    console.info(`[mock] no file dialog outside Tauri, loading ${path}`);
    return this.loadAudioSource(path);
  }

  async getAudioSources(): Promise<AudioSourceDto[]> {
    return this.audioSources.map((source) => ({
      ...source,
      durationTicks: this.ticksFor(source.durationSeconds),
    }));
  }

  async unloadAudioSource(sourceId: number): Promise<AudioSourceDto[]> {
    const playing = this.clips.filter((clip) => clip.sourceId === sourceId).length;
    if (playing > 0) {
      throw new Error(`${playing} clip${playing > 1 ? 's' : ''} still play that file`);
    }
    this.audioSources = this.audioSources.filter((source) => source.id !== sourceId);
    return this.getAudioSources();
  }

  /**
   * The envelope of one source, at full mock resolution.
   *
   * Generated once per source and then **kept**, because the destructive edits
   * ([`editTrackSample`](MockBackend.editTrackSample)) transform it: a mock
   * that regenerated the shape from the source id on every request would draw
   * the same waveform after a Reverse as before it, and the flow the UI is
   * being exercised for would prove nothing.
   */
  private envelope(sourceId: number): [number, number][] {
    const held = this.waveforms.get(sourceId);
    if (held) return held;
    const made: [number, number][] = Array.from({ length: MOCK_ENVELOPE_BINS }, (_, index) => {
      const t = index / MOCK_ENVELOPE_BINS;
      const envelope =
        (0.35 + 0.65 * Math.abs(Math.sin(t * Math.PI * (2 + sourceId)))) * (1 - t * 0.45);
      const level = Math.min(0.98, envelope * (0.7 + 0.3 * Math.abs(Math.sin(t * 47))));
      return [-level, level] as [number, number];
    });
    this.waveforms.set(sourceId, made);
    return made;
  }

  /** A plausible envelope: a decaying, wobbling take, deterministic per source. */
  async getAudioWaveform(sourceId: number, targetBins?: number): Promise<WaveformDto> {
    const source = this.audioSources.find((entry) => entry.id === sourceId);
    if (!source) throw new Error(`there is no audio source ${sourceId}`);
    const bins = Math.min(MAX_WAVEFORM_BINS, Math.max(1, targetBins ?? 1024));
    const peaks = foldPeaks(this.envelope(sourceId), bins);
    return { sourceId, frames: source.frames, bins, maxBins: MAX_WAVEFORM_BINS, peaks };
  }

  // --- destructive sample edits --------------------------------------------
  //
  // The engine writes a new WAV and loads it as a new source; the mock does the
  // same to its stored envelope. Both ends agree on the shape that matters to
  // the UI: a new source id, the old one still listed, and a waveform that
  // actually changed.

  async editTrackSample(track: number, op: SampleOp): Promise<SampleEditDto> {
    const def = this.track(track);
    const previousSourceId = def.audioSourceId;
    if (previousSourceId == null) {
      throw new Error(`track ${track} plays no audio file`);
    }
    const source = this.audioSources.find((entry) => entry.id === previousSourceId);
    if (!source) throw new Error(`audio source ${previousSourceId} is not loaded`);

    const peaks = this.envelope(previousSourceId);
    const frames = source.frames;
    // Captured before `repoint` writes over them: `def` is the live track, and
    // reading the "previous" region out of it afterwards would report the new
    // one and make every undo a no-op.
    const previousStartFrames = def.sampleStartFrames;
    const previousEndFrames = def.sampleEndFrames;
    const start = def.sampleStartFrames;
    const end = def.sampleEndFrames === 0 || def.sampleEndFrames > frames
      ? frames
      : def.sampleEndFrames;

    let next = peaks;
    let nextFrames = frames;
    let region: [number, number] = [def.sampleStartFrames, def.sampleEndFrames];
    let shift = 0;
    let suffix: string;
    switch (op.kind) {
      case 'reverse': {
        next = peaks.slice().reverse();
        region = start === 0 && end === frames ? [0, 0] : [frames - end, frames - start];
        suffix = 'reversed';
        break;
      }
      case 'normalize': {
        const peak = peaks.reduce((worst, [lo, hi]) => Math.max(worst, Math.abs(lo), Math.abs(hi)), 0);
        if (peak <= 1e-6) throw new Error(`${source.name} is silent; there is nothing to normalize`);
        const scale = 10 ** (NORMALIZE_TARGET_DBFS / 20) / peak;
        next = peaks.map(([lo, hi]) => [lo * scale, hi * scale] as [number, number]);
        suffix = 'normalized';
        break;
      }
      case 'trim': {
        if (start >= end || (start === 0 && end === frames)) {
          throw new Error(
            'Trim needs a region shorter than the whole file — drag one on the waveform first',
          );
        }
        const first = Math.floor((start / frames) * peaks.length);
        const last = Math.max(first + 1, Math.ceil((end / frames) * peaks.length));
        next = peaks.slice(first, last);
        nextFrames = end - start;
        region = [0, 0];
        shift = -start;
        suffix = 'trimmed';
        break;
      }
      case 'gain': {
        const db = Math.max(-MAX_EDIT_GAIN_DB, Math.min(MAX_EDIT_GAIN_DB, op.db));
        const scale = 10 ** (db / 20);
        next = peaks.map(([lo, hi]) => [lo * scale, hi * scale] as [number, number]);
        suffix = `${db >= 0 ? '+' : ''}${db.toFixed(1)} dB`;
        break;
      }
    }

    const stem = source.name.replace(/\.[^.]+$/, '');
    const seconds = source.sampleRate > 0 ? nextFrames / source.sampleRate : source.durationSeconds;
    const written: AudioSourceDto = {
      id: this.nextAudioId++,
      path: `${SAMPLE_EDIT_DIR}\\${stem} (${suffix}).wav`,
      name: `${stem} (${suffix}).wav`,
      frames: nextFrames,
      sampleRate: source.sampleRate,
      durationSeconds: seconds,
      durationTicks: this.ticksFor(seconds),
      missing: false,
      bytes: nextFrames * 8,
    };
    this.audioSources.push(written);
    this.waveforms.set(written.id, next);

    this.repoint(track, written.id, region[0], region[1], shift);
    return {
      track,
      op: OP_LABELS[op.kind],
      source: written,
      previousSourceId,
      startFrames: region[0],
      endFrames: region[1],
      previousStartFrames,
      previousEndFrames,
      clipOffsetShiftFrames: shift,
      mixer: this.mixerState(),
    };
  }

  async setTrackAudioSource(
    track: number,
    sourceId: number,
    startFrames: number,
    endFrames: number,
    clipOffsetShiftFrames: number,
  ): Promise<MixerState> {
    if (!this.audioSources.some((entry) => entry.id === sourceId)) {
      throw new Error(`audio source ${sourceId} is not loaded`);
    }
    this.repoint(track, sourceId, startFrames, endFrames, clipOffsetShiftFrames);
    return this.mixerState();
  }

  /** Point one channel (and only its own clips) at a different loaded file. */
  private repoint(
    track: number,
    sourceId: number,
    startFrames: number,
    endFrames: number,
    clipOffsetShiftFrames: number,
  ): void {
    const def = this.track(track);
    const from = def.audioSourceId;
    def.audioSourceId = sourceId;
    def.sourceId = def.sourceId.startsWith('clip:') ? `clip:${sourceId}` : `oneshot:${sourceId}`;
    def.sampleStartFrames = Math.max(0, Math.floor(startFrames));
    def.sampleEndFrames = Math.max(0, Math.floor(endFrames));
    if (from == null || from === sourceId) return;
    for (const clip of this.clips) {
      if (clip.sourceId === from && clip.track === track) {
        clip.sourceId = sourceId;
        clip.offsetFrames = Math.max(0, (clip.offsetFrames ?? 0) + clipOffsetShiftFrames);
      }
    }
  }

  /**
   * Render one loop of the active pattern and land it as a sample channel.
   *
   * The mock has no DSP, so the "render" is a synthetic envelope built from the
   * steps that are actually on: every switched-on step becomes a decaying hit at
   * its own position in the loop. That is enough for the flow under test -- a
   * new channel appears, its waveform is of *this* pattern, and an empty grid
   * refuses rather than adding a channel of silence, exactly as the engine does.
   */
  async renderPatternToSample(): Promise<RenderToSampleDto> {
    if (this.tracks.length >= MAX_TRACKS) {
      throw new Error(`a project may have at most ${MAX_TRACKS} tracks`);
    }
    const pattern = this.patterns.find((entry) => entry.id === this.activePattern);
    if (!pattern) throw new Error('there is no active pattern');

    const hits: number[] = [];
    for (const channel of pattern.channels) {
      channel.steps?.steps.forEach((step, index) => {
        if (step.on) hits.push(index / pattern.steps);
      });
      for (const note of channel.notes) hits.push(note.start / (pattern.steps * TICKS_PER_STEP));
    }
    if (hits.length === 0) {
      throw new Error(
        'the rendered pattern is silent — check that a channel is unmuted and has steps',
      );
    }

    const peaks: [number, number][] = Array.from({ length: MOCK_ENVELOPE_BINS }, (_, index) => {
      const t = index / MOCK_ENVELOPE_BINS;
      let level = 0;
      for (const hit of hits) {
        const since = t - hit;
        if (since >= 0 && since < 0.12) level = Math.max(level, 0.9 * Math.exp(-since * 40));
      }
      return [-level, level] as [number, number];
    });

    const seconds = (pattern.steps / 4) * (60 / this.bpm);
    const source: AudioSourceDto = {
      id: this.nextAudioId++,
      path: `${RENDER_DIR}\\${pattern.name}.wav`,
      name: `${pattern.name}.wav`,
      frames: Math.round(seconds * 48_000),
      sampleRate: 48_000,
      durationSeconds: seconds,
      durationTicks: this.ticksFor(seconds),
      missing: false,
      bytes: Math.round(seconds * 48_000 * 8),
    };
    this.audioSources.push(source);
    this.waveforms.set(source.id, peaks);
    const track = this.tracks.length;
    this.pushOneshotTrack(source, source.name);
    return {
      track,
      name: source.name,
      frames: source.frames,
      sampleRate: source.sampleRate,
      peak: 0.9,
      hostedChannels: this.tracks.filter((entry) => entry.clap || entry.vst3).length,
      mixer: this.mixerState(),
    };
  }

  async previewSource(sourceId: string, targetBins?: number): Promise<SourcePreviewDto> {
    if (sourceId.startsWith('clap:') || sourceId.startsWith('vst3:')) {
      throw new Error(
        'a hosted plugin draws its own window — click the channel name to open it',
      );
    }
    const source = INTERNAL_SOURCES.find((entry) => entry.id === sourceId);
    if (!source) throw new Error(`unknown source '${sourceId}'`);
    const engine = engineOf(sourceId);
    if (engine !== 'synth' && engine !== 'sampler' && engine !== 'tone') {
      throw new Error(`unknown source '${sourceId}'`);
    }
    const bins = Math.min(MAX_WAVEFORM_BINS, Math.max(1, targetBins ?? 512));
    const decay =
      sourceId === 'hat-closed' ? 0.92 : sourceId === 'hat-open' ? 0.55 : sourceId === 'kick' ? 0.7 : 0.5;
    const peaks: [number, number][] = Array.from({ length: bins }, (_, index) => {
      const t = index / bins;
      const envelope = Math.pow(1 - t, 1 + decay) * (0.45 + 0.55 * Math.abs(Math.sin(t * Math.PI * 6)));
      const level = Math.min(0.95, envelope);
      return [-level, level];
    });
    const peak = peaks.reduce((acc, pair) => Math.max(acc, pair[1]), 0);
    return {
      sourceId,
      engine,
      frames: 24_000,
      sampleRate: 48_000,
      durationMs: 500,
      bins,
      peaks,
      peak,
    };
  }

  async importDroppedAudio(fileName: string, bytes: Uint8Array): Promise<string> {
    console.info(`[mock] import ${fileName} (${bytes.length} bytes) — nothing is written`);
    return `${PROJECT_DIR}\\imports\\${fileName}`;
  }

  async getLibraryRoots(): Promise<LibraryRootDto[]> {
    // Factory first, then Packs, then the folders the user added -- the order
    // `library::roots` produces.
    return [
      { id: 'factory', name: 'Factory', kind: 'factory', path: MOCK_FACTORY },
      { id: 'packs', name: 'Packs', kind: 'packs', path: MOCK_PACKS },
      ...this.extraFolders,
    ];
  }

  async addLibraryFolder(): Promise<LibraryRootDto[] | null> {
    const n = this.nextFolder++;
    this.extraFolders.push({
      id: `extra-mock-${n}`,
      name: n === 1 ? 'Samples' : `Samples ${n}`,
      kind: 'extra',
      path: `${PROJECT_DIR}\\Samples${n === 1 ? '' : ` ${n}`}`,
    });
    saveMockFolders(this.extraFolders);
    console.info('[mock] no folder dialog outside Tauri, added a sample folder');
    return this.getLibraryRoots();
  }

  async removeLibraryFolder(id: string): Promise<LibraryRootDto[]> {
    // Neither built-in root is a folder the user added, so neither can be taken
    // away -- the engine answers both the same way.
    if (id === 'factory' || id === 'packs') {
      throw new Error(`there is no library folder '${id}'`);
    }
    const before = this.extraFolders.length;
    this.extraFolders = this.extraFolders.filter((folder) => folder.id !== id);
    if (this.extraFolders.length === before) {
      throw new Error(`there is no library folder '${id}'`);
    }
    saveMockFolders(this.extraFolders);
    return this.getLibraryRoots();
  }

  async listLibraryDir(rootId: string, relPath: string): Promise<LibraryEntryDto[]> {
    if (rootId === 'factory') {
      // One directory per installed instrument, and the samples inside it: a
      // kit is an instrument you play *and* a folder of one-shots you can drag
      // out or chop, which is the whole reason this root is listed.
      const installed = this.assets.filter((asset) => asset.state === 'ready');
      if (relPath === '') {
        return installed.map((asset) => ({
          name: asset.id,
          kind: 'dir' as const,
          relPath: asset.id,
          path: `${MOCK_FACTORY}\\${asset.id}`,
        }));
      }
      const files = MOCK_FACTORY_KITS[relPath as keyof typeof MOCK_FACTORY_KITS];
      if (files === undefined || !installed.some((asset) => asset.id === relPath)) return [];
      return files.map((name) => ({
        name,
        kind: 'file' as const,
        relPath: `${relPath}/${name}`,
        path: `${MOCK_FACTORY}\\${relPath}\\${name}`,
      }));
    }
    if (rootId === 'packs' && relPath === '') {
      return [
        {
          name: 'Drums',
          kind: 'dir',
          relPath: 'Drums',
          path: `${MOCK_PACKS}\\Drums`,
        },
        {
          name: 'Drum Loops',
          kind: 'dir',
          relPath: 'Drum Loops',
          path: `${MOCK_PACKS}\\Drum Loops`,
        },
        {
          name: 'One Shots',
          kind: 'dir',
          relPath: 'One Shots',
          path: `${MOCK_PACKS}\\One Shots`,
        },
        {
          name: 'Songstarters',
          kind: 'dir',
          relPath: 'Songstarters',
          path: `${MOCK_PACKS}\\Songstarters`,
        },
      ];
    }
    if (rootId === 'packs' && relPath === 'Drums') {
      return MOCK_DRUMS.map((name) => ({
        name,
        kind: 'file' as const,
        relPath: `Drums/${name}`,
        path: `${MOCK_PACKS}\\Drums\\${name}`,
      }));
    }
    const mockCategory =
      relPath === 'Drum Loops'
        ? MOCK_LOOP_FILES
        : relPath === 'One Shots'
          ? MOCK_ONE_SHOTS
          : relPath === 'Songstarters'
            ? MOCK_SONGSTARTERS
            : null;
    if (rootId === 'packs' && mockCategory !== null) {
      return mockCategory.map((name) => ({
        name,
        kind: 'file' as const,
        relPath: `${relPath}/${name}`,
        path: `${MOCK_PACKS}\\${relPath}\\${name}`,
      }));
    }
    if (this.extraFolders.some((folder) => folder.id === rootId) && relPath === '') {
      const root = this.extraFolders.find((folder) => folder.id === rootId)!;
      return [
        {
          name: 'loop.wav',
          kind: 'file',
          relPath: 'loop.wav',
          path: `${root.path}\\loop.wav`,
        },
      ];
    }
    return [];
  }

  async previewAudioFile(path: string): Promise<void> {
    this.lastPreview = path;
    console.info(`[mock] preview ${path}`);
  }

  async previewTrackSample(track: number): Promise<void> {
    const entry = this.track(track);
    if (entry.audioSourceId == null || (entry.engine !== 'clip' && entry.engine !== 'sample')) {
      throw new Error(`track ${track} is not an audio-file channel`);
    }
    this.lastPreview = `track:${track}`;
    console.info(`[mock] preview edited sample on track ${track}`);
  }

  async previewInstrument(sourceId: string): Promise<void> {
    this.lastPreview = sourceId;
    console.info(`[mock] preview instrument ${sourceId}`);
  }

  async getPreviewSettings(): Promise<PreviewSettings> {
    return { version: 1, volumePercent: this.previewVolumePercent };
  }

  async setPreviewVolume(percent: number): Promise<PreviewSettings> {
    this.previewVolumePercent = clampPreviewVolumePercent(percent);
    return this.getPreviewSettings();
  }

  async getAudioSettings(): Promise<AudioOutput> {
    return {
      version: 1,
      preferAsio: this.preferAsio,
      bufferFrames: this.audioBufferFrames,
      asioCompiled: false,
      stream: await this.getStreamInfo(),
    };
  }

  async setAudioSettings(preferAsio: boolean, bufferFrames: number): Promise<AudioOutput> {
    this.preferAsio = preferAsio;
    this.audioBufferFrames = snapAudioBufferFrames(bufferFrames);
    return this.getAudioSettings();
  }

  private ensureClipChannel(sourceId: number): void {
    if (this.tracks.some((track) => track.engine === 'clip' && track.audioSourceId === sourceId)) {
      return;
    }
    if (this.tracks.length >= MAX_TRACKS) {
      throw new Error(`a project may have at most ${MAX_TRACKS} tracks`);
    }
    const source = this.audioSources.find((entry) => entry.id === sourceId);
    const name = source?.name ?? `Audio ${sourceId}`;
    const index = this.tracks.length;
    this.tracks.push({
      index,
      id: this.nextTrackId++,
      name,
      sourceId: `clip:${sourceId}`,
      sourceLabel: name,
      engine: 'clip',
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: false,
      clap: null,
      vst3: null,
      missing: null,
      inserts: [],
      ...SAMPLE_PLAY,
      slices: noSlices(),
      ...noSourceParams(),
      audioSourceId: sourceId,
    });
    for (const pattern of this.patterns) {
      if (!pattern.channels.some((clip) => clip.channel === index)) {
        pattern.channels.push({
          channel: index,
          mode: 'steps',
          steps: {
            key: DEFAULT_ROW_KEYS[index] ?? 60,
            label: '',
            steps: Array.from({ length: pattern.steps }, () => ({
              on: false,
              velocity: STEP_VELOCITY,
            })),
          },
          notes: [],
        });
      }
    }
  }

  async addAudioClip(
    sourceId: number,
    track: number,
    start: number,
    length?: number,
    offsetFrames?: number,
    gainDb?: number,
  ): Promise<SequencerState> {
    const source = this.audioSources.find((entry) => entry.id === sourceId);
    if (!source) throw new Error(`there is no audio source ${sourceId}`);
    this.ensureClipChannel(sourceId);
    this.lane(track);
    const span = length ?? this.ticksFor(source.durationSeconds);
    this.checkClipBounds(start, span);
    this.clips.push({
      id: this.nextClipId++,
      kind: 'audio',
      track,
      start,
      length: span,
      sourceId,
      offsetFrames: offsetFrames ?? 0,
      gainDb: gainDb ?? 0,
    });
    return this.sequencerState();
  }

  async setClipGain(clipId: number, gainDb: number): Promise<SequencerState> {
    const clip = this.clip(clipId);
    if (clip.kind !== 'audio') throw new Error('a pattern clip has no gain of its own');
    clip.gainDb = gainDb;
    return this.sequencerState();
  }

  async setClipOffset(clipId: number, offsetFrames: number): Promise<SequencerState> {
    const clip = this.clip(clipId);
    if (clip.kind !== 'audio') throw new Error('a pattern clip has no offset');
    clip.offsetFrames = Math.max(0, Math.round(offsetFrames));
    return this.sequencerState();
  }

  // --- the project ---------------------------------------------------------

  async getProjectInfo(): Promise<ProjectInfo> {
    return { ...this.project, warnings: [...this.project.warnings] };
  }

  private session(): MockSession {
    return structuredClone({
      tracks: this.tracks,
      buses: this.buses,
      sends: this.sends,
      masterGainDb: this.masterGainDb,
      masterInserts: this.masterInserts,
      masterClap: this.masterClap,
      masterVst3: this.masterVst3,
      patterns: this.patterns,
      activePattern: this.activePattern,
      playlistTracks: this.playlistTracks,
      clips: this.clips,
      bpm: this.bpm,
      playMode: this.playMode,
      selected: this.selected,
    });
  }

  private restore(session: MockSession): void {
    const copy = structuredClone(session);
    this.tracks = copy.tracks;
    this.buses = copy.buses;
    this.sends = copy.sends;
    this.masterGainDb = copy.masterGainDb;
    this.masterInserts = copy.masterInserts;
    this.masterClap = copy.masterClap;
    this.masterVst3 = copy.masterVst3;
    this.patterns = copy.patterns;
    this.activePattern = copy.activePattern;
    this.playlistTracks = copy.playlistTracks;
    this.clips = copy.clips;
    this.bpm = copy.bpm;
    this.playMode = copy.playMode;
    this.selected = copy.selected;
    this.reinstantiate();
  }

  /**
   * Everything that belonged to the session being replaced and cannot outlive
   * it. Both ways a session is replaced destroy every plugin instance, so no
   * window may still claim to be open and a saved handle must not be handed out
   * a second time.
   */
  private reinstantiate(): void {
    this.openEditors.clear();
    this.clapStates.clear();
    for (const track of this.tracks) {
      if (track.clap) track.clap.instance = this.nextInstance++;
      if (track.vst3) track.vst3.instance = this.nextInstance++;
    }
    if (this.masterClap) this.masterClap.instance = this.nextInstance++;
    if (this.masterVst3) this.masterVst3.instance = this.nextInstance++;
  }

  async newProject(): Promise<ProjectInfo> {
    this.tracks = starterTracks();
    this.buses = [];
    this.sends = [];
    this.masterGainDb = 0;
    this.masterInserts = [];
    this.masterClap = null;
    this.masterVst3 = null;
    this.patterns = [emptyPattern(1, 'Pattern 1', [0, 1])];
    this.activePattern = 1;
    this.nextPatternId = 2;
    this.playlistTracks = Array.from({ length: 4 }, (_, index) => ({
      id: index + 1,
      name: `Track ${index + 1}`,
      muted: false,
      soloed: false,
    }));
    this.clips = [];
    this.bpm = DEFAULT_BPM;
    this.playMode = 'pat';
    this.selected = 0;
    this.nextTrackId = 5;
    this.reinstantiate();
    this.project = projectInfo(null);
    return this.getProjectInfo();
  }

  /**
   * There is no dialog outside Tauri, so the most recent save stands in for
   * one, and `null` comes back when nothing has been written yet -- the same
   * `null` a real cancel answers with, which is the path worth rehearsing.
   */
  async openProject(path?: string | null): Promise<ProjectInfo | null> {
    const chosen = path ?? this.lastSavedPath;
    if (chosen === null || chosen === undefined) {
      console.info('[mock] open_project: no dialog outside Tauri, and nothing saved to offer');
      return null;
    }
    const session = this.saved.get(chosen);
    if (!session) throw new Error(`there is no project at ${chosen}`);
    this.restore(session);
    this.project = projectInfo(chosen);
    return this.getProjectInfo();
  }

  async saveProject(saveAs: boolean, path?: string | null): Promise<ProjectInfo | null> {
    const chosen = path ?? (saveAs ? null : this.project.path) ?? this.invent();
    this.saved.set(chosen, this.session());
    this.lastSavedPath = chosen;
    this.project = projectInfo(chosen);
    return this.getProjectInfo();
  }

  private invent(): string {
    const stem = this.project.path === null ? UNTITLED : projectName(this.project.path);
    let candidate = `${PROJECT_DIR}\\${stem}.mydaw`;
    for (let n = 2; this.saved.has(candidate); n += 1) {
      candidate = `${PROJECT_DIR}\\${stem} ${n}.mydaw`;
    }
    return candidate;
  }

  /**
   * A plausible device. `running: true` even when nothing is playing, because
   * the mock stands in for a session whose device *is* open -- a status bar
   * permanently reading "no device" would leave the readouts unrehearsable.
   */
  async getStreamInfo(): Promise<StreamInfo> {
    const bufferFrames = this.audioBufferFrames > 0 ? this.audioBufferFrames : 256;
    return {
      deviceName: 'Mock Output (browser)',
      sampleRate: SAMPLE_RATE,
      channels: 2,
      bufferFrames,
      maxBufferFrames: 4096,
      requestedBufferFrames: this.audioBufferFrames,
      latencyMs: (bufferFrames / SAMPLE_RATE) * 1000,
      backend: 'wasapi',
      note: this.preferAsio
        ? 'ASIO was requested, but this WAVE build has no ASIO host. Using WASAPI — other apps keep playing, and the buffer size still applies.'
        : '',
      running: true,
    };
  }

  // --- audio input and recording -------------------------------------------

  async getInputDevices(): Promise<InputDeviceDto[]> {
    return structuredClone(this.inputDevices);
  }

  async setInputDevice(deviceId: string | null): Promise<InputStatusDto> {
    if (this.recordState === 'recording') {
      throw new Error('stop the take before changing the input device');
    }
    if (deviceId !== null && !this.inputDevices.some((device) => device.id === deviceId)) {
      throw new Error(`input device '${deviceId}' is not available`);
    }
    this.inputDeviceId = deviceId;
    return this.getInputStatus();
  }

  /** The device that would be opened: the chosen one, or the host default. */
  private activeInputDevice(): InputDeviceDto | undefined {
    return this.inputDeviceId === null
      ? this.inputDevices.find((device) => device.isDefault)
      : this.inputDevices.find((device) => device.id === this.inputDeviceId);
  }

  /**
   * A fake level: two slow sines beating against each other, so the meter moves
   * the way a voice does rather than sitting still. Zero while idle, because an
   * unopened device cannot be hearing anything.
   */
  private inputLevel(): { peak: number; rms: number } {
    if (this.recordState === 'idle') return { peak: 0, rms: 0 };
    const t = performance.now() / 1000;
    const envelope = 0.45 + 0.4 * Math.abs(Math.sin(t * 1.7)) * (0.6 + 0.4 * Math.sin(t * 0.31));
    const peak = Math.min(0.99, Math.max(0, envelope));
    return { peak, rms: peak * 0.62 };
  }

  async getInputStatus(): Promise<InputStatusDto> {
    const device = this.activeInputDevice();
    const open = this.recordState !== 'idle';
    const { peak, rms } = this.inputLevel();
    const elapsed =
      this.recordState === 'recording'
        ? Math.min(MAX_RECORD_SECONDS, (performance.now() - this.recordStartedMs) / 1000)
        : 0;
    return {
      state: this.recordState,
      deviceName: open ? (device?.name ?? '') : '',
      deviceId: this.inputDeviceId,
      deviceSampleRate: open ? (device?.sampleRate ?? 0) : 0,
      deviceChannels: open ? (device?.channels ?? 0) : 0,
      sampleRate: SAMPLE_RATE,
      peak,
      rms,
      elapsedSeconds: elapsed,
      maxSeconds: MAX_RECORD_SECONDS,
      droppedSamples: 0,
      limitReached: elapsed >= MAX_RECORD_SECONDS,
      fault: '',
      hasDevices: this.inputDevices.length > 0,
    };
  }

  async setInputArmed(armed: boolean): Promise<InputStatusDto> {
    if (!armed) {
      this.recordState = 'idle';
      return this.getInputStatus();
    }
    const device = this.activeInputDevice();
    if (!device) throw new Error('no audio input device is available');
    // Mirrors the engine's refusal: a device the host will not describe is a
    // device that will not open.
    if (device.channels === 0) {
      throw new Error(`input device error: ${device.name} reported no usable configuration`);
    }
    this.recordState = 'armed';
    console.info(`[mock] armed ${device.name}; the level is invented`);
    return this.getInputStatus();
  }

  async startRecording(): Promise<InputStatusDto> {
    if (this.recordState === 'recording') throw new Error('a take is already being recorded');
    if (this.recordState === 'idle') await this.setInputArmed(true);
    this.recordState = 'recording';
    this.recordStartedMs = performance.now();
    return this.getInputStatus();
  }

  /**
   * There is no capture device in a browser, so the take is *invented* at the
   * length the button was actually held -- which is what makes the whole
   * gesture (record, channel, sample editor with a waveform) demonstrable
   * without hardware. The same trade `openAudioFile` makes for the dialog.
   */
  async stopRecording(): Promise<RecordedTakeDto> {
    if (this.recordState !== 'recording') throw new Error('nothing is being recorded');
    const seconds = Math.min(MAX_RECORD_SECONDS, (performance.now() - this.recordStartedMs) / 1000);
    this.recordState = 'armed';
    if (seconds < 0.05) {
      throw new Error('nothing was recorded; the input device produced no audio');
    }

    this.takesRecorded += 1;
    const name = `Recording ${this.takesRecorded}.wav`;
    const source: AudioSourceDto = {
      id: this.nextAudioId++,
      path: `${MOCK_RECORDINGS}\\${name}`,
      name,
      frames: Math.round(seconds * SAMPLE_RATE),
      sampleRate: SAMPLE_RATE,
      durationSeconds: seconds,
      durationTicks: this.ticksFor(seconds),
      missing: false,
      bytes: Math.round(seconds * SAMPLE_RATE * 8),
    };
    this.audioSources.push(source);
    console.info(`[mock] no capture device outside Tauri, invented ${name} (${seconds.toFixed(2)} s)`);
    return {
      source: { ...source },
      droppedSamples: 0,
      limitReached: seconds >= MAX_RECORD_SECONDS,
      peak: 0.82,
    };
  }

  async cancelRecording(): Promise<InputStatusDto> {
    if (this.recordState !== 'recording') throw new Error('nothing is being recorded');
    this.recordState = 'armed';
    return this.getInputStatus();
  }

  async listMidiPorts(): Promise<MidiPortDto[]> {
    return structuredClone(this.midiPorts);
  }

  async setMidiPortEnabled(name: string, enabled: boolean): Promise<MidiPortDto[]> {
    const port = this.midiPorts.find((entry) => entry.name === name);
    if (!port) throw new Error(`there is no MIDI port '${name}'`);
    port.enabled = enabled;
    port.connected = enabled;
    port.status = enabled ? 'connected' : 'available';
    if (enabled) this.midiActivityAtMs = Date.now();
    return this.listMidiPorts();
  }

  async getMidiStatus(): Promise<MidiStatusDto> {
    const enabled = this.midiPorts.filter((port) => port.enabled);
    return {
      activityAtMs: this.midiActivityAtMs,
      enabledCount: enabled.length,
      activeName: enabled[0]?.name ?? null,
    };
  }

  async getAssetStatus(): Promise<AssetStatus[]> {
    return structuredClone(this.assets);
  }

  async startAssetDownload(id: string): Promise<AssetStatus[]> {
    const asset = this.assets.find((entry) => entry.id === id);
    if (!asset) throw new Error(`there is no asset '${id}'`);
    if (asset.state === 'ready' || asset.state === 'downloading') return this.getAssetStatus();
    asset.state = 'downloading';
    asset.progress = 0;
    asset.bytesDone = 0;
    asset.error = null;
    const timer = setInterval(() => {
      asset.bytesDone = Math.min(asset.bytesTotal, asset.bytesDone + asset.bytesTotal / 25);
      asset.progress = asset.bytesTotal > 0 ? asset.bytesDone / asset.bytesTotal : 1;
      if (asset.progress >= 1) {
        asset.state = 'ready';
        asset.progress = 1;
        clearInterval(timer);
      }
    }, 160);
    return this.getAssetStatus();
  }

  /**
   * A synthetic 30 Hz stream: every strip on its own slow LFO, honouring its
   * fader, pan and effective mute, and a bus lit by whatever sends into it --
   * so a send knob visibly does something in a plain browser.
   */
  async subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe> {
    const periodMs = 1000 / FRAME_HZ;
    const framesPerTick = Math.round(SAMPLE_RATE / FRAME_HZ);
    const start = performance.now();

    const timer = setInterval(() => {
      if (this.playing) this.samplePos += framesPerTick;
      const t = (performance.now() - start) / 1000;
      const env = this.playing ? 1 : 0;
      const state = this.mixerState();

      const level = (index: number, gainDb: number, silenced: boolean): number => {
        if (silenced || gainDb <= MIN_GAIN_DB) return 0;
        return env * 10 ** (gainDb / 20) * (0.5 + 0.45 * Math.abs(Math.sin(t * (0.7 + index * 0.31))));
      };
      const spread = (amount: number, pan: number) => {
        const theta = ((pan + 1) * Math.PI) / 4;
        return {
          peakL: amount * Math.SQRT2 * Math.cos(theta),
          peakR: amount * Math.SQRT2 * Math.sin(theta),
        };
      };

      const trackLevels = state.tracks.map((track, index) =>
        level(index, track.gainDb, track.silenced),
      );
      const tracks = state.tracks.map((track, index) => spread(trackLevels[index]!, track.pan));
      const buses = state.buses.map((bus, index) => {
        const fed = state.sends
          .filter((send) => send.dest === index && send.sourceKind === 'track')
          .reduce((sum, send) => sum + (trackLevels[send.sourceIndex] ?? 0) * send.level, 0);
        const amount = bus.silenced || bus.gainDb <= MIN_GAIN_DB
          ? 0
          : Math.min(1.4, fed * 10 ** (bus.gainDb / 20));
        return spread(amount, bus.pan);
      });

      const peakL = env * (0.55 + 0.4 * Math.abs(Math.sin(t * 1.1)));
      const peakR = env * (0.55 + 0.4 * Math.abs(Math.sin(t * 1.37 + 0.6)));
      onFrame({
        peakL,
        peakR,
        rmsL: peakL * 0.62,
        rmsR: peakR * 0.62,
        samplePos: this.samplePos,
        playing: this.playing,
        tracks,
        buses,
      });
    }, periodMs);

    return () => clearInterval(timer);
  }
}
