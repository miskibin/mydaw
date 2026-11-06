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

export type FxKind = 'eq' | 'comp' | 'sat' | 'delay' | 'reverb' | 'limit' | 'vst3';
export type BuiltinKind = Exclude<FxKind, 'vst3'>;
export type BandType = 'lowcut' | 'lowshelf' | 'peak' | 'highshelf' | 'highcut';

export interface EqBand { on: boolean; type: BandType; f: number; g: number; q: number }

export interface PluginRef {
  format: string; path: string; cid: string; name: string; vendor: string; state?: string;
}

export interface Fx {
  id: Id; type: FxKind; bypass: boolean;
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
export interface Note { inst: Inst; step: number; len: number; p?: number }
export interface Pattern { id: Id; name: string; color: string; len: number; notes: Note[] }

export interface Source {
  id: Id; name: string; color: string; path: string;
  dur: number; sr: number; ch: number; peakDb: number;
}

export type ClipKind = 'pat' | 'audio';
export interface Clip {
  id: Id; track: number; start: number; len: number; offset: number;
  kind: ClipKind; ref: Id; muted: boolean; gain: number;
  fadeIn: number; fadeOut: number; pan: number; norm: boolean; rev: boolean;
}

export interface Project {
  version: number; bpm: number;
  tracks: Track[]; inserts: Insert[]; patterns: Pattern[]; sources: Source[]; clips: Clip[];
  loop: LoopRange | null; nextId: Id;
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
  | { op: 'putSource'; source: Source }
  | { op: 'removeSource'; id: Id }
  | { op: 'setBpm'; bpm: number }
  | { op: 'setLoop'; loop: LoopRange | null };

export interface TransportDto {
  playing: boolean; rec: boolean; monitor: boolean; metro: boolean; countIn: boolean; startBeat: number;
}

export interface StateDto {
  project: Project; revision: number; canUndo: boolean; canRedo: boolean;
  transport: TransportDto; path: string | null; dirty: boolean;
}

export interface ParamDef { id: string; label: string; min: number; max: number; def: number; unit: string; log: boolean }
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

export interface ImportResult { sourceIds: Id[]; clipIds: Id[] }

export interface Vst3Info { path: string; cid: string; name: string; vendor: string; category: string }
export interface Vst3Param { id: number; name: string; value: number; display: string; def: number }

export interface CliStatus { installed: boolean; command: string; path: string | null; note?: string | null }
