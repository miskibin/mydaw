import { describe, expect, it } from 'vitest';
import { MockUpdates, memoryStore, MOCK_NEXT, MOCK_START, type MockUpdatesOpts } from '../api/mockUpdates';
import type { UpdateProgress } from '../api/types';
import { parseChangelog } from '../lib/changelog';
import { UpdateController, type UpdatesApi, type UpdState } from './updateCtl';

const LOG = parseChangelog('## 0.9.3 — 2026-11-10\n- three\n## 0.9.2 — 2026-11-09\n- two\n## 0.9.1\n## 0.9.0\n- zero\n');

/** The mock update backend with a progress bus, as MockBackend wires it. */
function api(o: MockUpdatesOpts = {}) {
  const subs = new Set<(p: UpdateProgress) => void>();
  const store = o.store ?? memoryStore();
  const m = new MockUpdates((done, total) => subs.forEach(f => f({ done, total })), { wait: async () => {}, restart: () => {}, offline: () => false, ...o, store });
  const a: UpdatesApi = {
    appInfo: () => m.appInfo(), updateCheck: () => m.updateCheck(), updateDownload: () => m.updateDownload(), updateInstall: () => m.updateInstall(),
    updateSetAuto: on => m.updateSetAuto(on), updateMarkSeen: v => m.updateMarkSeen(v), openReleasePage: v => m.openReleasePage(v),
    on: async (_ev, cb) => { subs.add(cb); return () => subs.delete(cb); },
  };
  return { a, m, store };
}
const make = (a: UpdatesApi, confirm = async () => true) => {
  const seen: UpdState[] = [];
  const c = new UpdateController(a, LOG, { confirmRestart: confirm, now: () => 1234 }, s => seen.push(s));
  return { c, seen };
};

describe('What\'s new', () => {
  it('shows nothing on a fresh install, and remembers the version', async () => {
    const { a, store } = api();
    const { c } = make(a);
    await c.init();
    expect(c.state.news).toBeNull();
    expect(store.get('wave.mock.seen')).toBe(MOCK_START);
  });

  it('opens once after an update, with every version since the last one seen', async () => {
    const { a, store } = api();
    store.set('wave.mock.version', '0.9.3'); store.set('wave.mock.seen', '0.9.0');
    const { c } = make(a);
    await c.init();
    expect(c.state.news?.map(r => r.version)).toEqual(['0.9.3', '0.9.2']);
    expect(store.get('wave.mock.seen')).toBe('0.9.0'); // not before it was shown
    await c.closeNews();
    expect(c.state.news).toBeNull();
    expect(store.get('wave.mock.seen')).toBe('0.9.3');
    const again = make(a).c; await again.init();
    expect(again.state.news).toBeNull(); // not shown a second time
  });

  it('stays closed when the new version has no notes, or after a downgrade, and updates the marker', async () => {
    const { a, store } = api();
    store.set('wave.mock.version', '0.9.1'); store.set('wave.mock.seen', '0.9.0');
    const c1 = make(a).c; await c1.init();
    expect(c1.state.news).toBeNull();
    expect(store.get('wave.mock.seen')).toBe('0.9.1');
    store.set('wave.mock.version', '0.9.0');
    const c2 = make(a).c; await c2.init();
    expect(c2.state.news).toBeNull();
    expect(store.get('wave.mock.seen')).toBe('0.9.0');
  });

  it('can be reopened from Settings without touching the marker', async () => {
    const { a, store } = api();
    store.set('wave.mock.version', '0.9.3'); store.set('wave.mock.seen', '0.9.3');
    const { c } = make(a); await c.init();
    c.showNotes();
    expect(c.state.news?.map(r => r.version)).toEqual(['0.9.3']);
    await c.closeNews();
    expect(store.get('wave.mock.seen')).toBe('0.9.3');
  });
});

describe('update journey', () => {
  it('check → download with progress → restart only on install', async () => {
    const store = memoryStore();
    let restarted = 0;
    const o = api({ store, restart: () => { restarted++; } });
    const { c, seen } = make(o.a);
    await c.init();
    await c.check(false);
    expect(c.state.phase).toBe('available');
    expect(c.state.info?.version).toBe(MOCK_NEXT);
    expect(c.state.dialog).toBe(false); // an automatic check never interrupts
    await c.start();
    expect(c.state.phase).toBe('ready');
    expect(c.state.total).toBeGreaterThan(0);
    expect(seen.some(s => s.phase === 'downloading' && s.done > 0 && s.done < s.total)).toBe(true);
    expect(restarted).toBe(0);
    expect(store.get('wave.mock.version')).toBeNull();
    await c.install();
    expect(c.state.phase).toBe('installing');
    expect(restarted).toBe(1);
    expect(store.get('wave.mock.version')).toBe(MOCK_NEXT);
  });

  it('after the restart the new version asks to show its notes', async () => {
    const { a } = api();
    const first = make(a).c; await first.init(); await first.check(false); await first.start(); await first.install();
    const { c } = make(a); await c.init();
    expect(c.state.current).toBe(MOCK_NEXT);
    expect(c.state.news?.map(r => r.version)).toEqual(['0.9.3']);
    await c.check(false);
    expect(c.state.phase).toBe('idle'); // nothing newer
  });

  it('a manual check opens the dialog, reports "up to date" and shows errors; a silent one stays quiet', async () => {
    const { a, store } = api();
    const { c } = make(a); await c.init();
    await c.check(true);
    expect(c.state.dialog).toBe(true);
    c.closeDialog();
    store.set('wave.mock.version', MOCK_NEXT);
    const u = make(a).c; await u.init();
    await u.check(true);
    expect(u.state.phase).toBe('idle'); expect(u.state.upToDateAt).toBe(1234); expect(u.state.dialog).toBe(false);

    const off = api({ offline: () => true });
    const quiet = make(off.a).c; await quiet.init();
    await quiet.check(false);
    expect(quiet.state.error).toBeNull(); expect(quiet.state.phase).toBe('idle');
    await quiet.check(true);
    expect(quiet.state.error).toMatch(/error sending request/);
    expect(quiet.state.phase).toBe('idle'); // can try again
  });

  it('cancelling the unsaved-changes question keeps the downloaded update waiting', async () => {
    const { a } = api();
    let ask = 0;
    const { c } = make(a, async () => { ask++; return false; });
    await c.init(); await c.check(false); await c.start();
    await c.install();
    expect(ask).toBe(1);
    expect(c.state.phase).toBe('ready');
  });

  it('a failed download or install leaves the old version running and can be retried', async () => {
    const { a } = api();
    let fail = true;
    const flaky: UpdatesApi = { ...a, updateDownload: async () => { if (fail) throw new Error('signature mismatch'); return a.updateDownload(); }, updateInstall: async () => { if (fail) throw new Error('installer failed'); } };
    const { c } = make(flaky);
    await c.init(); await c.check(false);
    await c.start();
    expect(c.state.phase).toBe('available'); expect(c.state.error).toMatch(/signature mismatch/);
    fail = false;
    await c.start();
    expect(c.state.phase).toBe('ready'); expect(c.state.error).toBeNull();
    fail = true;
    await c.install();
    expect(c.state.phase).toBe('ready'); expect(c.state.error).toMatch(/installer failed/);
  });

  it('ignores calls that do not fit the phase (double clicks, install before download)', async () => {
    const { a } = api();
    const { c } = make(a); await c.init();
    await c.install(); await c.start();
    expect(c.state.phase).toBe('idle');
    await c.check(false);
    await Promise.all([c.start(), c.start()]);
    expect(c.state.phase).toBe('ready');
  });

  it('the mock refuses what the engine refuses', async () => {
    const { m } = api();
    await expect(m.updateDownload()).rejects.toThrow(/check for updates first/);
    await expect(m.updateInstall()).rejects.toThrow(/check for updates first/);
    await m.updateCheck();
    await expect(m.updateInstall()).rejects.toThrow(/not downloaded/);
  });

  it('remembers the automatic-check switch', async () => {
    const { a, store } = api();
    const { c } = make(a); await c.init();
    expect(c.state.autoUpdate).toBe(true);
    await c.setAuto(false);
    expect(c.state.autoUpdate).toBe(false);
    expect(store.get('wave.mock.noauto')).toBe('1');
    const again = make(a).c; await again.init();
    expect(again.state.autoUpdate).toBe(false);
  });
});
