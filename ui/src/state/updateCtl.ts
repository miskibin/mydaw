// The update journey as a plain state machine (no UI framework, so it is tested with a fake backend):
//
//   idle ──check──▶ available ──start──▶ downloading ──▶ ready ──install──▶ installing ──▶ (restart)
//
// It never restarts by itself: `install` runs only from a click, after the unsaved-changes question.
// The "What's new" dialog is separate from the update: it shows once per new version, from the bundled
// CHANGELOG, and covers every version since the last one whose notes were shown.
import type { AppInfo, UpdateInfo, UpdateProgress } from '../api/types';
import { releaseOf, releasesSince, type Release } from '../lib/changelog';

export interface UpdatesApi {
  appInfo(): Promise<AppInfo>;
  updateCheck(): Promise<UpdateInfo | null>;
  updateDownload(): Promise<void>;
  updateInstall(): Promise<void>;
  updateSetAuto(on: boolean): Promise<void>;
  updateMarkSeen(version: string): Promise<void>;
  openReleasePage(version?: string | null): Promise<void>;
  on(ev: 'update-progress', cb: (p: UpdateProgress) => void): Promise<() => void>;
}

export type UpdPhase = 'idle' | 'checking' | 'available' | 'downloading' | 'ready' | 'installing';

export interface UpdState {
  current: string;
  autoUpdate: boolean;
  phase: UpdPhase;
  /** The release found by the last check. */
  info: UpdateInfo | null;
  done: number;
  total: number;
  /** Why the last check, download or install failed (shown where the user acted). */
  error: string | null;
  /** When the last check finished without finding anything (ms): "You have the newest version". */
  upToDateAt: number | null;
  /** The update dialog is open. */
  dialog: boolean;
  /** The What's new dialog's releases (null = closed). */
  news: Release[] | null;
}

export const EMPTY_UPD: UpdState = { current: '', autoUpdate: true, phase: 'idle', info: null, done: 0, total: 0, error: null, upToDateAt: null, dialog: false, news: null };

export interface UpdateHooks {
  /** Asks about unsaved changes before the restart; false = the user cancelled. */
  confirmRestart(): Promise<boolean>;
  now?(): number;
}

/** The message of a failure: Tauri rejects with a string, a thrown Error has `message`. */
const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));

export class UpdateController {
  state: UpdState = EMPTY_UPD;
  private off: (() => void) | null = null;
  /** The release notes to mark as seen when the What's new dialog closes. */
  private pendingSeen: string | null = null;

  constructor(private api: UpdatesApi, private log: Release[], private hooks: UpdateHooks, private onChange: (s: UpdState) => void) {}

  private set(p: Partial<UpdState>) { this.state = { ...this.state, ...p }; this.onChange(this.state); }

  /** Reads the version, listens for download progress and decides whether to show What's new. */
  async init(): Promise<void> {
    try {
      const info = await this.api.appInfo();
      this.set({ current: info.version, autoUpdate: info.autoUpdate });
      this.off = await this.api.on('update-progress', p => { if (this.state.phase === 'downloading') this.set({ done: p.done, total: p.total }); });
      if (info.seenVersion == null) await this.api.updateMarkSeen(info.version); // fresh install: nothing is "new"
      else if (info.seenVersion !== info.version) {
        const news = releasesSince(this.log, info.seenVersion, info.version);
        if (news.length) { this.pendingSeen = info.version; this.set({ news }); } else await this.api.updateMarkSeen(info.version); // no notes, or a downgrade
      }
    } catch (e) { console.warn('updates: init failed', e); }
  }

  dispose() { this.off?.(); this.off = null; }

  /** `manual`: the user asked, so an answer is always shown (the dialog, "up to date" or the error). */
  async check(manual: boolean): Promise<void> {
    const { phase } = this.state;
    if (phase === 'available' && manual) { this.set({ dialog: true, error: null }); return; }
    if (phase !== 'idle') return;
    this.set({ phase: 'checking', error: manual ? null : this.state.error });
    try {
      const info = await this.api.updateCheck();
      if (info) this.set({ phase: 'available', info, error: null, upToDateAt: null, dialog: manual || this.state.dialog });
      else this.set({ phase: 'idle', info: null, error: null, upToDateAt: (this.hooks.now ?? Date.now)() });
    } catch (e) {
      // a silent check that fails (offline, GitHub down) says nothing; the next one tries again
      this.set({ phase: 'idle', error: manual ? msg(e) : this.state.error });
    }
  }

  /** Downloads the installer; the user keeps working. Ends in `ready` (or back in `available` with the error). */
  async start(): Promise<void> {
    if (this.state.phase !== 'available') return;
    this.set({ phase: 'downloading', done: 0, total: 0, error: null });
    try {
      await this.api.updateDownload();
      this.set({ phase: 'ready' });
    } catch (e) { this.set({ phase: 'available', error: msg(e) }); }
  }

  /** Restarts into the new version after the unsaved-changes question. */
  async install(): Promise<void> {
    if (this.state.phase !== 'ready') return;
    this.set({ error: null });
    if (!(await this.hooks.confirmRestart())) return;
    this.set({ phase: 'installing' });
    try { await this.api.updateInstall(); } catch (e) { this.set({ phase: 'ready', error: msg(e) }); }
  }

  openDialog() { if (this.state.phase !== 'idle' && this.state.phase !== 'checking') this.set({ dialog: true }); }
  closeDialog() { this.set({ dialog: false }); }

  async setAuto(on: boolean): Promise<void> {
    const was = this.state.autoUpdate;
    this.set({ autoUpdate: on });
    try { await this.api.updateSetAuto(on); } catch (e) { this.set({ autoUpdate: was }); console.warn(e); }
  }

  /** Settings → "What's new": the notes of the running version, once more. */
  showNotes() {
    const r = releaseOf(this.log, this.state.current);
    if (r) { this.pendingSeen = null; this.set({ news: [r] }); }
  }

  /** Closing What's new marks the version as seen, so it opens once per update. */
  async closeNews(): Promise<void> {
    this.set({ news: null });
    const v = this.pendingSeen;
    if (!v) return;
    this.pendingSeen = null;
    try { await this.api.updateMarkSeen(v); } catch (e) { console.warn(e); }
  }

  openReleasePage(version?: string | null) { return this.api.openReleasePage(version ?? this.state.info?.version ?? null); }
}
