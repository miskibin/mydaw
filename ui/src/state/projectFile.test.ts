import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MockBackend } from '../api/mock';

// The real actions against the mock backend, which replies like the engine's commands.
let answer: 'save' | 'discard' | 'cancel' = 'discard';
let recAnswer: 'restore' | 'discard' | 'later' = 'restore';
let recAsked = 0;
vi.mock('../ui/Confirm', () => ({ confirmUnsaved: async () => answer, confirmRecover: async () => { recAsked++; return recAnswer; } }));
beforeAll(() => {
  const noop = () => {};
  vi.stubGlobal('window', { addEventListener: noop, removeEventListener: noop, innerWidth: 1600, innerHeight: 900 });
  vi.stubGlobal('localStorage', { getItem: () => null, setItem: noop, removeItem: noop });
});

let mock: MockBackend;
let A: typeof import('./actions');
let store: typeof import('./store');
beforeEach(async () => {
  vi.resetModules();
  const backend = await import('../api/backend');
  mock = (await backend.getBackend()) as MockBackend;
  A = await import('./actions');
  store = await import('./store');
  store.replaceState(await mock.getState());
  answer = 'discard';
  recAnswer = 'restore';
  recAsked = 0;
});

describe('opening a project', () => {
  it('loads the state the open command replies with', async () => {
    await A.projectOpen();
    expect(store.S.path).toContain('Demo.wave');
    expect(store.S.project.clips.length).toBeGreaterThan(0);
    expect(store.S.dirty).toBe(false);
  });
  it('does nothing when the file dialog is dismissed', async () => {
    const before = store.S.revision;
    mock.dismissNextDialog = true;
    await A.projectOpen();
    expect(store.S.revision).toBe(before);
    expect(store.S.path).toBeNull();
  });
});

describe('saving a project', () => {
  it('reports a dismissed save dialog as not saved', async () => {
    mock.dismissNextDialog = true;
    expect(await A.projectSave(false)).toBe(false);
    expect(store.S.path).toBeNull();
  });
  it('keeps unsaved work when "Save" is chosen and the save dialog is dismissed', async () => {
    store.replaceState({ ...(await mock.getState()), dirty: true });
    answer = 'save';
    mock.dismissNextDialog = true;
    const rev = store.S.revision;
    await A.projectOpen();
    expect(store.S.revision).toBe(rev); // the open was abandoned with the save
    expect((await mock.getState()).path).toBeNull(); // and the engine never replaced the project
  });
  it('saves and returns true when a path is chosen', async () => {
    expect(await A.projectSave(false)).toBe(true);
    expect(store.S.path).toMatch(/\.wave$/);
  });
});

describe('crash recovery of a project that was never saved', () => {
  /** An unsaved edit, the autosave tick, then the app dies and starts again. */
  async function crashWithUnsavedWork() {
    await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
    store.replaceState(await mock.getState());
    expect(store.S.path).toBeNull();
    await A.recoveryTick();
    mock.simulateCrash();
    store.replaceState(await mock.getState());
    expect(store.S.project.bpm).not.toBe(137);
  }

  it('brings the work back as an unsaved project', async () => {
    await crashWithUnsavedWork();
    await A.offerRecovery();
    expect(recAsked).toBe(1);
    expect(store.S.project.bpm).toBe(137);
    expect(store.S.path).toBeNull();
    expect(store.S.dirty).toBe(true);
    expect(await mock.recoveryList()).toEqual([]);
    // Save asks where to put it, like any new project
    mock.dismissNextDialog = true;
    expect(await A.projectSave(false)).toBe(false);
  });
  it('keeps the copy for the next start on "Not now", and deletes it on "Delete copy"', async () => {
    await crashWithUnsavedWork();
    recAnswer = 'later';
    await A.offerRecovery();
    expect(store.S.project.bpm).not.toBe(137);
    expect((await mock.recoveryList()).length).toBe(1);
    recAnswer = 'discard';
    await A.offerRecovery();
    expect(await mock.recoveryList()).toEqual([]);
  });
  it('asks nothing after a normal session', async () => {
    await A.offerRecovery();
    expect(recAsked).toBe(0);
  });
  it('writes no copy for a saved project, and drops the copy once the project is saved', async () => {
    await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
    store.replaceState(await mock.getState());
    expect(await mock.recoveryWrite()).toBe(true);
    await A.projectSave(false);
    mock.simulateCrash();
    expect(await mock.recoveryList()).toEqual([]);
  });
});
