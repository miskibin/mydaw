// The browser mock of the update commands (app/src/updater.rs): same order of calls, same refusals,
// plus a fake newer release so the whole journey can be tried without Tauri.
import type { AppInfo, UpdateInfo } from './types';
import { cmpVersion } from '../lib/changelog';

/** The mock starts as this version and "finds" `MOCK_NEXT`; installing it restarts (reloads) as `MOCK_NEXT`. */
export const MOCK_START = '0.9.2';
export const MOCK_NEXT = '0.9.3';
const MOCK_NOTES = '### Added\n- **Updates inside the app.** A new version shows up in the status bar; it downloads in the background and restarts only when you say so.\n- **What\'s new after every update.** The notes open once the first time you start a new version.';

/** Where the mock keeps what Tauri keeps in settings.json and the installed app. */
export interface MockStore { get(key: string): string | null; set(key: string, value: string): void }

export const memoryStore = (): MockStore => {
  const m = new Map<string, string>();
  return { get: k => m.get(k) ?? null, set: (k, v) => { m.set(k, v); } };
};

export const browserStore = (): MockStore => {
  const mem = memoryStore();
  return {
    get: k => { try { return localStorage.getItem(k) ?? mem.get(k); } catch { return mem.get(k); } },
    set: (k, v) => { mem.set(k, v); try { localStorage.setItem(k, v); } catch { /* private window */ } },
  };
};

export interface MockUpdatesOpts { store?: MockStore; wait?: (ms: number) => Promise<void>; restart?: () => void; chunks?: number; offline?: () => boolean }

export class MockUpdates {
  private found: UpdateInfo | null = null;
  private downloaded = false;
  private busy = false;
  private readonly store: MockStore;
  private readonly wait: (ms: number) => Promise<void>;
  private readonly restart: () => void;
  private readonly chunks: number;
  private readonly offline: () => boolean;
  constructor(private emitProgress: (done: number, total: number) => void, o: MockUpdatesOpts = {}) {
    this.store = o.store ?? browserStore();
    this.wait = o.wait ?? (ms => new Promise<void>(r => setTimeout(r, ms)));
    this.restart = o.restart ?? (() => { try { location.reload(); } catch { /* not in a page */ } });
    this.chunks = o.chunks ?? 20;
    this.offline = o.offline ?? (() => { try { return new URLSearchParams(location.search).has('offline'); } catch { return false; } });
  }
  private get version() { return this.store.get('wave.mock.version') ?? MOCK_START; }

  async appInfo(): Promise<AppInfo> {
    return { version: this.version, seenVersion: this.store.get('wave.mock.seen'), autoUpdate: this.store.get('wave.mock.noauto') !== '1', pluginEditors: true };
  }
  async updateCheck(): Promise<UpdateInfo | null> {
    await this.wait(500);
    if (this.offline()) throw new Error('error sending request for url (https://github.com/miskibin/wave-DAW/releases/latest/download/latest.json)');
    this.downloaded = false;
    if (cmpVersion(MOCK_NEXT, this.version) <= 0) { this.found = null; return null; }
    this.found = { version: MOCK_NEXT, notes: MOCK_NOTES, date: '2026-11-10' };
    return this.found;
  }
  async updateDownload(): Promise<void> {
    if (!this.found) throw new Error('no update was found: check for updates first');
    if (this.busy) throw new Error('an update is already being downloaded or installed');
    if (this.downloaded) return;
    this.busy = true;
    try {
      const total = 96_000_000;
      for (let k = 1; k <= this.chunks; k++) { await this.wait(60); this.emitProgress(Math.round(total * k / this.chunks), total); }
      this.downloaded = true;
    } finally { this.busy = false; }
  }
  async updateInstall(): Promise<void> {
    if (!this.found) throw new Error('no update was found: check for updates first');
    if (!this.downloaded) throw new Error('the update is not downloaded yet');
    await this.wait(300);
    this.store.set('wave.mock.version', this.found.version);
    this.restart();
  }
  async updateSetAuto(on: boolean): Promise<void> { this.store.set('wave.mock.noauto', on ? '0' : '1'); }
  async updateMarkSeen(version: string): Promise<void> { this.store.set('wave.mock.seen', version); }
  async openReleasePage(version?: string | null): Promise<void> {
    const url = `https://github.com/miskibin/wave-DAW/releases${version ? `/tag/v${version}` : ''}`;
    try { window.open(url, '_blank', 'noopener'); } catch { /* no window */ }
  }
}
