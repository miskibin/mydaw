// Typed wrappers over the Tauri commands/events in docs/architecture.md.
// When `window.__TAURI_INTERNALS__` is absent (plain browser) the in-browser mock is used.
import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AudioConfig, AudioDevices, AudioHost, AudioStatus, LatencyReport, Catalog, CliStatus, ExportResult, F0ModelStatus, Id, ImportResult, LibFile,
  KeysStatus, LibraryList, LibraryProgress, MidiDevice, MidiNoteEvent, MidiStatus, ModelProgress, AppInfo, UpdateInfo, UpdateProgress, MusicAnalysis, AutoVocalPlan, AutoVocalStyle, Op, OpenResult, PitchAnalysis, PitchTarget, RefTune, SaveResult, StateDto, TransportDto, Vst3Info, Vst3Param,
} from './types';

export interface OsDropEvent { type: 'over' | 'drop' | 'leave'; paths: string[]; x: number; y: number }

export interface EventMap {
  state: StateDto;
  'library-progress': LibraryProgress;
  /** WAVE Keys: the piano started loading, is ready, or is not on this computer. */
  'keys-status': KeysStatus;
  'model-progress': ModelProgress;
  /** The update installer is downloading (`updateDownload`). `total` is 0 when the size is unknown. */
  'update-progress': UpdateProgress;
  'audio-status': AudioStatus;
  /** Keyboard connected, unplugged or failed, or the choice changed. */
  'midi-status': MidiStatus;
  /** A key went down or up on the MIDI keyboard. */
  'midi-note': MidiNoteEvent;
}

export interface Backend {
  readonly isMock: boolean;
  getState(): Promise<StateDto>;
  applyOps(ops: Op[], opts?: { label?: string; transient?: boolean }): Promise<{ revision: number }>;
  beginGesture(label?: string): Promise<void>;
  endGesture(label?: string): Promise<void>;
  undo(): Promise<StateDto>;
  redo(): Promise<StateDto>;
  transportPlay(fromBeat: number): Promise<void>;
  transportStop(): Promise<void>;
  transportSet(patch: Partial<Pick<TransportDto, 'metro' | 'countIn' | 'rec' | 'monitor'>>): Promise<TransportDto>;
  subscribeTelemetry(onFrame: (buf: ArrayBuffer) => void): Promise<void>;
  watchFx(ids: Id[]): Promise<void>;
  fxCatalog(): Promise<Catalog>;
  importAudio(args: { paths: string[]; track?: number; beat?: number }): Promise<ImportResult>;
  /** Native "open files" dialog for audio; null when cancelled. */
  pickAudioFiles(): Promise<string[] | null>;
  sourcePeaks(id: Id, norm?: boolean, rev?: boolean): Promise<Float32Array>;
  /** Like sourcePeaks, but of what the clip actually plays (norm/rev/pitch edits applied); whole source length. */
  clipPeaks(clipId: Id): Promise<Float32Array>;
  /** Pitch curve, levels and note segmentation of an audio clip's source (seconds of the source). */
  pitchAnalyze(clipId: Id): Promise<PitchAnalysis>;
  /** Tune to Reference target of an audio clip: with `reference` a preview for those parameters,
   *  without it the clip's own `tune.reference`. The alignment is cached, so changing parameters is cheap. */
  pitchTarget(clipId: Id, reference?: RefTune): Promise<PitchTarget>;
  /** Which pitch detector analyses (RMVPE when its model is installed, else YIN). */
  f0ModelStatus(): Promise<F0ModelStatus>;
  /** Downloads the RMVPE model (progress: `model-progress`), verifies it, starts using it. */
  f0ModelInstall(): Promise<F0ModelStatus>;
  /** Tempo (BPM) and musical key of an audio clip, or key/tempo of the project's active audio clips (excluding `excludeFx`'s insert). */
  musicAnalyze(args?: { clipId?: Id; excludeFx?: Id }): Promise<MusicAnalysis>;
  /** "Auto wokal": measures a vocal clip (key from `beatClipId`, else from the other tracks) and returns the calibrated
   *  insert chain. Read-only; takes a few seconds per minute of audio. `bakeRider`: the rider is left out of
   *  `chain` and returned as `riderBake` — pass its params to `bakeRider` (no live look-ahead delay on the insert). */
  autoVocalPlan(args: { clipId: Id; beatClipId?: Id; style?: AutoVocalStyle; bakeRider?: boolean }): Promise<AutoVocalPlan>;
  /** Renders the Vocal Rider (`params`: catalog ids, e.g. `plan.riderBake.params`) into an audio clip's audio
   *  (region, clip gain, norm/rev/pitch edits; fades stay) as a new source; the clip (same id) plays it with gain
   *  0 dB. One undo step; the original source stays in the project. Slow: seconds per minute of audio. */
  bakeRider(args: { clipId: Id; params: Record<string, number> }): Promise<{ sourceId: Id; clipId: Id }>;
  trimClip(clipId: Id): Promise<{ sourceId: Id }>;
  /** Renders a comp clip to a new source; the clip becomes an ordinary audio clip (one undo step). */
  flattenComp(clipId: Id): Promise<{ sourceId: Id }>;
  /** Plays one whole take instead of the comp (null = back to the comp). Live only, not undone. */
  compAudition(compId: Id, takeId: Id | null): Promise<void>;
  previewClip(clipId: Id): Promise<void>;
  previewFile(path: string): Promise<void>;
  previewStop(): Promise<void>;
  /** Play one note of a channel now (not an edit). `key` MIDI (default: the channel's root), `vel` 0..1, `pattern`: the one being edited (routing). */
  noteAudition(args: { ch: Id; key?: number; vel?: number; pattern?: Id }): Promise<void>;
  /** Computer keyboard as a piano: key down (`on`) / up on a channel. */
  typingNote(args: { ch: Id; key: number; vel?: number; on: boolean }): Promise<void>;
  /** Loop one pattern on its own (null stops). Exclusive with the song transport. */
  patternPlay(pattern: Id | null): Promise<void>;
  audioHosts(): Promise<AudioHost[]>;
  audioDevices(host: string): Promise<AudioDevices>;
  audioGetConfig(): Promise<AudioStatus>;
  audioSetConfig(cfg: AudioConfig): Promise<AudioStatus>;
  /** Plays 8 clicks on the output (instead of the mix) for ~5 s while recording the input
   *  (microphone at the speaker, or a cable from an output to an input) and finds them by
   *  cross-correlation. Transport must be stopped. Saves nothing: apply `suggestedOffsetMs`
   *  with `audioSetConfig({ ...cfg, recOffsetMs })`. Rejects when no click came back. */
  audioMeasureLatency(): Promise<LatencyReport>;
  /** MIDI input ports the OS lists right now. */
  midiListDevices(): Promise<MidiDevice[]>;
  /** Choose the MIDI keyboard (null = none) and open it. Saved with the audio settings and reopened at startup. */
  midiSelectDevice(id: string | null): Promise<MidiStatus>;
  midiStatus(): Promise<MidiStatus>;
  /** Which channel (instrument) the keyboard plays (null = the first pitched channel). With a track armed and transport recording, played notes land as a pattern clip on that track. */
  midiSetTarget(channel: Id | null): Promise<MidiStatus>;
  libraryList(): Promise<LibraryList>;
  libraryInstall(id: string): Promise<void>;
  /** WAVE Keys: whether the piano's recordings are on this computer and loaded. */
  keysStatus(): Promise<KeysStatus>;
  libraryFiles(q: { kit?: string; folder?: string }): Promise<LibFile[]>;
  libraryAddFolder(): Promise<string[]>;
  libraryRemoveFolder(path: string): Promise<string[]>;
  projectNew(): Promise<StateDto>;
  /** The file dialog when `path` is omitted. */
  projectOpen(path?: string): Promise<OpenResult>;
  /** The save dialog when the project has no path yet or `saveAs`. */
  projectSave(saveAs?: boolean): Promise<SaveResult>;
  /** Offline bounce of the master mix to a 32-bit float stereo WAV. */
  exportAudio(opts: { useLoop: boolean; tail: number; sampleRate: number; format?: 'wav' | 'mp3'; mp3Kbps?: number }): Promise<ExportResult>;
  /** Abort an in-flight `exportAudio` before a file is written. */
  exportCancel(): Promise<void>;
  vst3Scan(rescan: boolean): Promise<Vst3Info[]>;
  vst3Editor(fxId: Id, open: boolean): Promise<void>;
  vst3Params(fxId: Id): Promise<Vst3Param[]>;
  vst3SetParam(fxId: Id, id: number, value: number): Promise<void>;
  cliStatus(): Promise<CliStatus>;
  /** The running version, the version whose release notes were last shown, and whether updates are checked automatically. */
  appInfo(): Promise<AppInfo>;
  /** Asks GitHub Releases for a newer version (null = this is the newest). Rejects when offline or the release is malformed. */
  updateCheck(): Promise<UpdateInfo | null>;
  /** Downloads the update found by `updateCheck` (progress: `update-progress`) and verifies its signature. Nothing restarts. */
  updateDownload(): Promise<void>;
  /** Installs the downloaded update and restarts WAVE (the call does not return then). Rejects on a failure; the old version keeps running. */
  updateInstall(): Promise<void>;
  updateSetAuto(on: boolean): Promise<void>;
  /** Remembers that the release notes of `version` were shown. */
  updateMarkSeen(version: string): Promise<void>;
  /** Opens the release page of `version` (null = the releases list) in the system browser. */
  openReleasePage(version?: string | null): Promise<void>;
  on<K extends keyof EventMap>(ev: K, cb: (payload: EventMap[K]) => void): Promise<() => void>;
  /** OS file drag & drop with real paths (Tauri) or file names (mock). */
  onOsDrop(cb: (e: OsDropEvent) => void): Promise<() => void>;
  /** Window title (file name + dirty mark). */
  setTitle(title: string): void;
  /** Native window controls for the custom title bar; null in the browser mock (no window chrome to drive). */
  readonly win: WindowCtl | null;
}

export interface WindowCtl {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  isMaximized(): Promise<boolean>;
  startDragging(): Promise<void>;
  /** Called on every window resize (maximise state may have changed). */
  onResized(cb: () => void): Promise<() => void>;
}

function tauriWindow(): WindowCtl {
  const w = () => import('@tauri-apps/api/window').then(m => m.getCurrentWindow());
  return {
    minimize: async () => (await w()).minimize(),
    toggleMaximize: async () => (await w()).toggleMaximize(),
    close: async () => (await w()).close(),
    isMaximized: async () => (await w()).isMaximized(),
    startDragging: async () => (await w()).startDragging(),
    onResized: async cb => (await w()).onResized(() => cb()),
  };
}

const toF32 = (r: unknown): Float32Array => {
  if (r instanceof ArrayBuffer) return new Float32Array(r.slice(0, r.byteLength - (r.byteLength % 4)));
  if (ArrayBuffer.isView(r)) { const b = r as ArrayBufferView; return new Float32Array(b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength - (b.byteLength % 4))); }
  if (Array.isArray(r)) {
    // A Vec<u8> serialised as JSON numbers: reinterpret as little-endian bytes.
    const u8 = Uint8Array.from(r as number[]);
    return new Float32Array(u8.buffer, 0, u8.byteLength >> 2);
  }
  return new Float32Array(0);
};

export function tauriBackend(): Backend {
  return {
    isMock: false,
    win: tauriWindow(),
    getState: () => invoke('get_state'),
    applyOps: (ops, opts) => invoke('apply_ops', { ops, label: opts?.label ?? null, transient: opts?.transient ?? false }),
    beginGesture: label => invoke('begin_gesture', { label: label ?? null }),
    endGesture: label => invoke('end_gesture', { label: label ?? null }),
    undo: () => invoke('undo'),
    redo: () => invoke('redo'),
    transportPlay: fromBeat => invoke('transport_play', { fromBeat }),
    transportStop: () => invoke('transport_stop'),
    transportSet: patch => invoke('transport_set', patch),
    async subscribeTelemetry(onFrame) {
      const ch = new Channel<ArrayBuffer>();
      ch.onmessage = m => onFrame(m instanceof ArrayBuffer ? m : (toF32(m).buffer as ArrayBuffer));
      await invoke('telemetry_subscribe', { channel: ch });
    },
    watchFx: ids => invoke('watch_fx', { ids }),
    fxCatalog: () => invoke('fx_catalog'),
    importAudio: a => invoke('import_audio', { paths: a.paths, track: a.track ?? null, beat: a.beat ?? null }),
    async pickAudioFiles() {
      // tauri-plugin-dialog's command, invoked directly so the UI needs no extra npm dependency.
      const r = await invoke<string | string[] | null>('plugin:dialog|open', {
        options: { multiple: true, directory: false, filters: [{ name: 'Audio', extensions: ['wav', 'mp3', 'flac', 'ogg', 'aif', 'aiff', 'm4a'] }] },
      });
      return r == null ? null : Array.isArray(r) ? r : [r];
    },
    sourcePeaks: async (id, norm, rev) => toF32(await invoke('source_peaks', { id, norm: !!norm, rev: !!rev })),
    clipPeaks: async clipId => toF32(await invoke('clip_peaks', { clipId })),
    pitchAnalyze: clipId => invoke('pitch_analyze', { clipId }),
    pitchTarget: (clipId, reference) => invoke('pitch_target', { clipId, reference: reference ?? null }),
    f0ModelStatus: () => invoke('f0_model_status'),
    f0ModelInstall: () => invoke('f0_model_install'),
    musicAnalyze: a => invoke('music_analyze', { clipId: a?.clipId ?? null, excludeFx: a?.excludeFx ?? null }),
    autoVocalPlan: a => invoke('auto_vocal_plan', { clipId: a.clipId, beatClipId: a.beatClipId ?? null, style: a.style ?? null, bakeRider: a.bakeRider ?? null }),
    bakeRider: a => invoke('bake_rider', { clipId: a.clipId, params: a.params }),
    trimClip: clipId => invoke('trim_clip', { clipId }),
    flattenComp: clipId => invoke('flatten_comp', { clipId }),
    compAudition: (compId, takeId) => invoke('comp_audition', { compId, takeId }),
    previewClip: clipId => invoke('preview_clip', { clipId }),
    previewFile: path => invoke('preview_file', { path }),
    previewStop: () => invoke('preview_stop'),
    typingNote: a => invoke('typing_note', { ch: a.ch, key: a.key, vel: a.vel ?? null, on: a.on }),
    noteAudition: a => invoke('note_audition', { ch: a.ch, key: a.key ?? null, vel: a.vel ?? null, pattern: a.pattern ?? null }),
    patternPlay: pattern => invoke('pattern_play', { pattern }),
    audioHosts: () => invoke('audio_hosts'),
    audioDevices: host => invoke('audio_devices', { host }),
    audioGetConfig: () => invoke('audio_get_config'),
    // Sent both as `config` and flattened, so either command signature deserialises it.
    audioSetConfig: cfg => invoke('audio_set_config', { config: cfg, ...cfg }),
    audioMeasureLatency: () => invoke('audio_measure_latency'),
    midiListDevices: () => invoke('midi_list_devices'),
    midiSelectDevice: id => invoke('midi_select_device', { id }),
    midiStatus: () => invoke('midi_status'),
    midiSetTarget: channel => invoke('midi_set_target', { channel }),
    libraryList: () => invoke('library_list'),
    libraryInstall: id => invoke('library_install', { id }),
    keysStatus: () => invoke('keys_status'),
    libraryFiles: q => invoke('library_files', { kit: q.kit ?? null, folder: q.folder ?? null }),
    libraryAddFolder: () => invoke('library_add_folder'),
    libraryRemoveFolder: path => invoke('library_remove_folder', { path }),
    projectNew: () => invoke('project_new'),
    projectOpen: path => invoke('project_open', { path: path ?? null }),
    projectSave: saveAs => invoke('project_save', { saveAs: !!saveAs }),
    exportAudio: opts => invoke('export_audio', { useLoop: opts.useLoop, tail: opts.tail, sampleRate: opts.sampleRate, format: opts.format, mp3Kbps: opts.mp3Kbps }),
    exportCancel: () => invoke('export_cancel'),
    vst3Scan: rescan => invoke('vst3_scan', { rescan }),
    vst3Editor: (fxId, open) => invoke('vst3_editor', { fxId, open }),
    vst3Params: fxId => invoke('vst3_params', { fxId }),
    vst3SetParam: (fxId, id, value) => invoke('vst3_set_param', { fxId, id, value }),
    cliStatus: () => invoke('cli_status'),
    appInfo: () => invoke('app_info'),
    updateCheck: () => invoke('update_check'),
    updateDownload: () => invoke('update_download'),
    updateInstall: () => invoke('update_install'),
    updateSetAuto: on => invoke('update_set_auto', { on }),
    updateMarkSeen: version => invoke('update_mark_seen', { version }),
    openReleasePage: version => invoke('open_release_page', { version: version ?? null }),
    on: async (ev, cb) => listen(ev, e => cb(e.payload as never)),
    async onOsDrop(cb) {
      const { getCurrentWebview } = await import('@tauri-apps/api/webview');
      return getCurrentWebview().onDragDropEvent(e => {
        const p = e.payload as { type: string; paths?: string[]; position?: { x: number; y: number } };
        const dpr = window.devicePixelRatio || 1;
        const x = (p.position?.x ?? 0) / dpr, y = (p.position?.y ?? 0) / dpr;
        if (p.type === 'over' || p.type === 'enter') cb({ type: 'over', paths: p.paths ?? [], x, y });
        else if (p.type === 'drop') cb({ type: 'drop', paths: p.paths ?? [], x, y });
        else cb({ type: 'leave', paths: [], x, y });
      });
    },
    setTitle(title) {
      document.title = title;
      import('@tauri-apps/api/window').then(m => m.getCurrentWindow().setTitle(title)).catch(() => {});
    },
  };
}

export const isTauri = () => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

let instance: Backend | null = null;
export async function getBackend(): Promise<Backend> {
  if (instance) return instance;
  if (isTauri()) instance = tauriBackend();
  else {
    const { MockBackend } = await import('./mock');
    instance = new MockBackend();
  }
  return instance;
}
/** Synchronous access after `getBackend()` resolved once (at startup). */
export const be = (): Backend => {
  if (!instance) throw new Error('backend not initialised');
  return instance;
};
