// TypeScript mirror of crates/engine/src/model.rs, ops.rs, catalog.rs and the DTOs in docs/architecture.md.
// JSON is camelCase; enum tags are lowercase.

export type Id = number;

/** Fader position that gives unity gain (gain = 2·t³). */
export const VOL_UNITY = 0.7937;
/** Beats shown on the playlist; the arrangement may not go past this. */
export const TOTAL_BEATS = 512;

export interface LoopRange { a: number; b: number }

export interface Track {
  id: Id; name: string; color: string;
  muted: boolean; solo: boolean; arm: boolean;
  vol: number; h: number; insert: Id;
}

export type FxKind = 'eq' | 'comp' | 'sat' | 'delay' | 'reverb' | 'limit' | 'fet' | 'deess' | 'tune' | 'denoise' | 'voice' | 'vst3';
export type BuiltinKind = Exclude<FxKind, 'vst3'>;
export type BandType = 'lowcut' | 'lowshelf' | 'peak' | 'highshelf' | 'highcut' | 'notch' | 'bandpass' | 'tilt';
/** Which part of the stereo image a band works on. */
export type Placement = 'stereo' | 'left' | 'right' | 'mid' | 'side';
/** Dynamic EQ: the band's gain moves by up to `range` dB as its own level goes over `thr` dBFS. */
export interface DynBand { range: number; thr: number }

/** Up to 24 bands (MAX_BANDS). `slope` (cuts only): 12/24/36/48 dB/oct. Missing fields take the engine defaults. */
export interface EqBand {
  on: boolean; type: BandType; f: number; g: number; q: number;
  slope?: number; place?: Placement; dynamic?: DynBand | null;
}
export const MAX_BANDS = 24;

export interface PluginRef {
  format: string; path: string; cid: string; name: string; vendor: string; state?: string;
}

export interface Fx {
  id: Id; type: FxKind; bypass: boolean;
  /** Dry/wet 0..1 (1 = fully processed; missing = 1). */
  mix?: number;
  preset?: number | null;
  params: Record<string, number>;
  bands?: EqBand[];
  plugin?: PluginRef;
}

export interface Insert {
  id: Id; name: string; color: string;
  vol: number; pan: number; muted: boolean; solo: boolean;
  fx: Fx[];
}

export type Inst = 'kick' | 'clap' | 'hat' | 'bass';
/** MIDI key at which a sampler plays its sample unpitched (C5 in FL Studio's naming, C4 here). */
export const SAMPLE_ROOT = 60;
/** What a channel plays: a built-in instrument, or a one-shot sample of a project source. */
export type ChannelSrc = { kind: 'inst'; inst: Inst } | { kind: 'sample'; source: Id };
/**
 * A Channel Rack row, global to the project (every pattern can have notes for it).
 * `vol` fader position (0.7937 = 0 dB), `pan` −1..1, `insert` null = the clip's track insert,
 * `root` = the key a step plays (and the sampler's original pitch).
 */
export type Channel = {
  id: Id; name: string; color: string;
  vol: number; pan: number; mute: boolean; solo: boolean; insert: Id | null; root: number;
} & ChannelSrc;
/**
 * A pattern note: channel `ch`, `step`/`len` in 16ths, `p` MIDI key (absent = the channel's root),
 * `v` velocity 0..1 (absent = 1). `inst` only in old files / scripts (the engine turns it into `ch`).
 */
export interface Note { ch: Id; step: number; len: number; p?: number; v?: number; inst?: Inst }
/** `len` in beats; `swing` 0..1 (absent = 0). Notes at or past `len` are kept but hidden and silent. */
export interface Pattern { id: Id; name: string; color: string; len: number; notes: Note[]; swing?: number }

export interface Source {
  id: Id; name: string; color: string; path: string;
  dur: number; sr: number; ch: number; peakDb: number;
}

/** `comp`: plays a take group's comp (`ref` = comp id, `offset` in comp beats). */
export type ClipKind = 'pat' | 'audio' | 'auto' | 'comp';

/** One recorded pass. `start`/`len` in comp beats, `offset` beats into the source. Takes are never cut. */
export interface Take { id: Id; name: string; ref: Id; start: number; len: number; offset: number; color?: string | null }
/** `take` plays over [from, to) (comp beats). `xfade`: crossfade at `from`, seconds (comp default when absent). */
export interface CompRegion { take: Id; from: number; to: number; xfade?: number | null }
/**
 * A take group and its comp. Comp beat = clip.offset + (timeline beat − clip.start).
 * `regions` sorted, non-overlapping, gaps silent. `xfade`: default boundary crossfade, seconds.
 */
export interface Comp { id: Id; name: string; takes: Take[]; regions: CompRegion[]; xfade: number }
/** Default comp crossfade (s) and the choices offered on a boundary. */
export const DEFAULT_XFADE = 0.01;
export const MAX_XFADE = 0.5;
export interface Clip {
  id: Id; track: number; start: number; len: number; offset: number;
  kind: ClipKind; ref: Id; muted: boolean; gain: number;
  fadeIn: number; fadeOut: number; pan: number; norm: boolean; rev: boolean;
  /** Note-level pitch edits (the pitch editor); rendered offline by the engine. */
  tune?: ClipTune | null;
}

/** Times in seconds of the source; `shift`/`formant` in cents, `drift` 0..1, `gain` dB. */
export interface NoteEdit { start: number; end: number; shift: number; drift: number; formant: number; gain: number }
/** `pencil`: hand-drawn `[seconds, midi]` points, sorted; gaps > 30 ms end a run. */
export interface ClipTune { notes: NoteEdit[]; pencil?: [number, number][] }

/** `pitch_analyze`: pitch every 1/rate s (null = unvoiced), levels 0..1, segmented notes (seconds of the source). */
export interface NoteBlob { start: number; end: number; pitch: number; level: number }
export interface PitchAnalysis { rate: number; curve: (number | null)[]; levels: number[]; notes: NoteBlob[] }

export interface TempoCandidate { bpm: number; score: number }
export interface TempoEstimate { bpm: number; confidence: number; candidates: TempoCandidate[]; firstBeat: number }
export interface KeyCandidate { root: number; minor: boolean; score: number }
export interface KeyEstimate {
  root: number; minor: boolean; confidence: number;
  candidates: KeyCandidate[]; chroma: number[]; tuningCents: number;
}
export interface MusicAnalysis { tempo: TempoEstimate | null; key: KeyEstimate | null }

/** What an automation moves. `param`: catalog param id, or `band:<i>:f|g|q` of an EQ. */
export type AutoTarget =
  | { kind: 'fx'; fxId: Id; param: string }
  | { kind: 'mix'; fxId: Id }
  | { kind: 'insertVol'; id: Id }
  | { kind: 'insertPan'; id: Id }
  | { kind: 'trackVol'; id: Id };
/** `t` beats from the start of the automation, `v` normalised 0..1. */
export interface AutoPoint { t: number; v: number }
export interface Automation { id: Id; name: string; color: string; target: AutoTarget; points: AutoPoint[] }

export interface Project {
  version: number; bpm: number;
  tracks: Track[]; inserts: Insert[]; patterns: Pattern[]; sources: Source[]; clips: Clip[];
  loop: LoopRange | null; nextId: Id;
  /** Automation envelopes; played by clips of kind `auto` (`ref` = automation id). */
  automations?: Automation[];
  /** Take groups with their comps; played by clips of kind `comp`. */
  comps?: Comp[];
  /** Channel Rack rows shared by every pattern. */
  channels?: Channel[];
}

export type Op =
  | { op: 'putClips'; clips: Clip[] }
  | { op: 'removeClips'; ids: Id[] }
  | { op: 'putTrack'; track: Track }
  | { op: 'putInsert'; insert: Insert }
  | { op: 'removeTrack'; id: Id }
  | { op: 'removeInsert'; id: Id }
  | { op: 'putFx'; insertId: Id; fx: Fx; index?: number }
  | { op: 'removeFx'; fxId: Id }
  | { op: 'moveFx'; fxId: Id; to: number }
  | { op: 'putPattern'; pattern: Pattern }
  | { op: 'putChannel'; channel: Channel; index?: number | null }
  | { op: 'removeChannel'; id: Id }
  | { op: 'putSource'; source: Source }
  | { op: 'removeSource'; id: Id }
  | { op: 'setBpm'; bpm: number }
  | { op: 'setLoop'; loop: LoopRange | null }
  | { op: 'putAutomation'; automation: Automation }
  | { op: 'removeAutomation'; id: Id }
  | { op: 'putComp'; comp: Comp }
  | { op: 'removeComp'; id: Id };

export interface TransportDto {
  playing: boolean; rec: boolean; monitor: boolean; metro: boolean; countIn: boolean; startBeat: number;
}

export interface StateDto {
  project: Project; revision: number; canUndo: boolean; canRedo: boolean;
  transport: TransportDto; path: string | null; dirty: boolean;
  /** Comp id (as a string key) → take played instead of the comp. Live only, not undone. */
  audition?: Record<string, Id>;
}

/** `options`: a discrete choice (value = index), shown as buttons instead of a knob. */
export interface ParamDef { id: string; label: string; min: number; max: number; def: number; unit: string; log: boolean; options?: string[] }
export interface TypeDef { name: string; params: ParamDef[] }
export interface Preset { name: string; params: Record<string, number>; bands?: EqBand[]; beats?: number }
export interface Catalog {
  types: Partial<Record<FxKind, TypeDef>>;
  presets: Partial<Record<FxKind, Preset[]>>;
}

export interface AudioHost { id: string; name: string }
export interface AudioDevices { outputs: string[]; inputs: string[] }
export interface AudioConfig {
  host: string; output: string | null; input: string | null; sampleRate: number | null; bufferSize: number | null;
}
export interface AudioStatus extends AudioConfig {
  running: boolean; actualRate: number; actualBuffer: number; latencyMs: number;
  inputChannels: number; error: string | null; note: string | null; asioCompiled: boolean;
}

export interface Kit { id: string; name: string; description: string; bytes: number; count: number; installed: boolean }
export interface LibraryList { kits: Kit[]; folders: string[] }
export interface LibFile { name: string; path: string; group: string }
export interface LibraryProgress { id: string; done: number; total: number; error?: string }

/** Level stats returned by an offline master bounce. */
export interface ExportStats {
  seconds: number; sampleRate: number; peakDbfs: number; rmsDbfs: number;
  crestDb: number; clippedSamples: number; silent: boolean;
}
/** Result of `exportAudio`; `{ cancelled: true }` when the save dialog or render was aborted. */
export type ExportResult =
  | { cancelled: true }
  | { cancelled?: false; out: string; fromBeat: number; toBeat: number; tail: number; stats: ExportStats };

export interface ImportResult { sourceIds: Id[]; clipIds: Id[] }

export interface Vst3Info { path: string; cid: string; name: string; vendor: string; category: string }
export interface Vst3Param { id: number; name: string; value: number; display: string; def: number }

export interface CliStatus { installed: boolean; command: string; path: string | null; note?: string | null }
