// Typed wrappers over the Tauri commands/events in docs/architecture.md.
// When `window.__TAURI_INTERNALS__` is absent (plain browser) the in-browser mock is used.
import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AudioConfig, AudioDevices, AudioHost, AudioStatus, Catalog, CliStatus, Id, ImportResult, LibFile,
  LibraryList, LibraryProgress, Op, StateDto, TransportDto, Vst3Info, Vst3Param,
} from './types';

export interface OsDropEvent { type: 'over' | 'drop' | 'leave'; paths: string[]; x: number; y: number }

export interface EventMap {
  state: StateDto;
  'library-progress': LibraryProgress;
  'audio-status': AudioStatus;
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
  trimClip(clipId: Id): Promise<{ sourceId: Id }>;
  previewClip(clipId: Id): Promise<void>;
  previewFile(path: string): Promise<void>;
  previewStop(): Promise<void>;
  audioHosts(): Promise<AudioHost[]>;
  audioDevices(host: string): Promise<AudioDevices>;
  audioGetConfig(): Promise<AudioStatus>;
  audioSetConfig(cfg: AudioConfig): Promise<AudioStatus>;
  asioControlPanel(): Promise<void>;
  libraryList(): Promise<LibraryList>;
  libraryInstall(id: string): Promise<void>;
  libraryFiles(q: { kit?: string; folder?: string }): Promise<LibFile[]>;
  libraryAddFolder(): Promise<string[]>;
  libraryRemoveFolder(path: string): Promise<string[]>;
  projectNew(): Promise<StateDto>;
  projectOpen(path?: string): Promise<StateDto>;
  projectSave(saveAs?: boolean): Promise<{ path: string }>;
  vst3Scan(rescan: boolean): Promise<Vst3Info[]>;
  vst3Editor(fxId: Id, open: boolean): Promise<void>;
  vst3Params(fxId: Id): Promise<Vst3Param[]>;
  vst3SetParam(fxId: Id, id: number, value: number): Promise<void>;
  cliStatus(): Promise<CliStatus>;
  on<K extends keyof EventMap>(ev: K, cb: (payload: EventMap[K]) => void): Promise<() => void>;
  /** OS file drag & drop with real paths (Tauri) or file names (mock). */
  onOsDrop(cb: (e: OsDropEvent) => void): Promise<() => void>;
  /** Window title (file name + dirty mark). */
  setTitle(title: string): void;
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
    trimClip: clipId => invoke('trim_clip', { clipId }),
    previewClip: clipId => invoke('preview_clip', { clipId }),
    previewFile: path => invoke('preview_file', { path }),
    previewStop: () => invoke('preview_stop'),
    audioHosts: () => invoke('audio_hosts'),
    audioDevices: host => invoke('audio_devices', { host }),
    audioGetConfig: () => invoke('audio_get_config'),
    // Sent both as `config` and flattened, so either command signature deserialises it.
    audioSetConfig: cfg => invoke('audio_set_config', { config: cfg, ...cfg }),
    asioControlPanel: () => invoke('asio_control_panel'),
    libraryList: () => invoke('library_list'),
    libraryInstall: id => invoke('library_install', { id }),
    libraryFiles: q => invoke('library_files', { kit: q.kit ?? null, folder: q.folder ?? null }),
    libraryAddFolder: () => invoke('library_add_folder'),
    libraryRemoveFolder: path => invoke('library_remove_folder', { path }),
    projectNew: () => invoke('project_new'),
    projectOpen: path => invoke('project_open', { path: path ?? null }),
    projectSave: saveAs => invoke('project_save', { saveAs: !!saveAs }),
    vst3Scan: rescan => invoke('vst3_scan', { rescan }),
    vst3Editor: (fxId, open) => invoke('vst3_editor', { fxId, open }),
    vst3Params: fxId => invoke('vst3_params', { fxId }),
    vst3SetParam: (fxId, id, value) => invoke('vst3_set_param', { fxId, id, value }),
    cliStatus: () => invoke('cli_status'),
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
