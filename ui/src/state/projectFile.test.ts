import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MockBackend } from '../api/mock';

// The real actions against the mock backend, which replies like the engine's commands.
let answer: 'save' | 'discard' | 'cancel' | 'busy' = 'discard';
let recAnswer: 'restore' | 'discard' | 'later' = 'restore';
let recAsked = 0;
let recPath: string | null = null;
let editDuringConfirm: (() => Promise<void>) | undefined;
vi.mock('../ui/Confirm', () => ({
  confirmUnsaved: async () => {
    const choice = (await import('./store')).S.dirty ? answer : 'discard';
    await editDuringConfirm?.();
    return choice;
  },
  confirmRecover: async (_ms: number, openPath: string | null = null) => { recAsked++; recPath = openPath; return recAnswer; },
}));
beforeAll(() => {
  const noop = () => {};
  vi.stubGlobal('window', { addEventListener: noop, removeEventListener: noop, innerWidth: 1600, innerHeight: 900, setTimeout });
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
  recPath = null;
  editDuringConfirm = undefined;
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
  it('opens an Explorer path directly, including spaces and Unicode', async () => {
    const path = 'C:\\Muzyka\\Nowy utwór.WAVE';
    mock.dismissNextDialog = true;
    await A.projectOpen(path);
    expect(store.S.path).toBe(path);
    expect(mock.dismissNextDialog).toBe(true);
  });
  it('keeps unsaved edits when an Explorer open is cancelled', async () => {
    const current = 'C:\\Muzyka\\First.wave';
    await A.projectOpen(current);
    await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
    store.replaceState(await mock.getState());
    answer = 'cancel';
    await A.projectOpen('C:\\Muzyka\\Second.wave');
    expect(store.S.path).toBe(current);
    expect(store.S.project.bpm).toBe(137);
    expect(store.S.dirty).toBe(true);
  });
  it('guards notebook text typed just before an Explorer open', async () => {
    const current = 'C:\\Muzyka\\First.wave';
    await A.projectOpen(current);
    await mock.applyOps([{ op: 'putNotebook', notebook: { pages: [{ id: 8000, name: 'Verse', text: '', startBar: 1 }] } }]);
    await mock.projectSave();
    store.replaceState(await mock.getState());
    const notebook = await import('./notebook');
    notebook.typeText(8000, 'A line I just typed');
    expect(store.S.dirty).toBe(false);
    answer = 'cancel';
    await A.projectOpen('C:\\Muzyka\\Second.wave');
    expect(store.S.path).toBe(current);
    expect(store.S.project.notebook?.pages[0].text).toBe('A line I just typed');
    expect(store.S.dirty).toBe(true);
  });
  it('keeps unsaved edits when the same project is opened again from Explorer', async () => {
    const path = 'C:\\Muzyka\\First.wave';
    await A.projectOpen(path);
    await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
    store.replaceState(await mock.getState());
    await A.projectOpen(path);
    expect(store.S.project.bpm).toBe(137);
    expect(store.S.dirty).toBe(true);
  });
  it('recognizes the open project spelled differently by Explorer or a terminal', async () => {
    const path = 'C:\\Muzyka\\First.wave';
    await A.projectOpen(path);
    await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
    store.replaceState(await mock.getState());
    answer = 'discard'; // a prompt would be answered with Discard: the edit must not reach it
    await A.projectOpen('c:/muzyka/FIRST.WAVE');
    expect(store.S.path).toBe(path);
    expect(store.S.project.bpm).toBe(137);
    expect(store.S.dirty).toBe(true);
  });
  it('refuses to open while a take is recording, like the engine', async () => {
    const current = 'C:\\Muzyka\\First.wave';
    await A.projectOpen(current);
    const track = store.S.project.tracks[0];
    await mock.applyOps([{ op: 'putTrack', track: { ...track, arm: true } }]);
    await mock.transportSet({ rec: true });
    await mock.transportPlay(0);
    store.replaceState(await mock.getState());
    await expect(mock.projectOpen('C:\\Muzyka\\Second.wave', store.S.revision)).rejects.toThrow('recording: stop it first');
    // checked before the revision, in the engine's order
    await expect(mock.projectOpen('C:\\Muzyka\\Second.wave', -1)).rejects.toThrow('recording: stop it first');
    await A.projectOpen('C:\\Muzyka\\Second.wave');
    expect(store.S.path).toBe(current);
    expect((await import('./ui')).U.readout).toContain('recording: stop it first');
    await mock.transportStop();
  });
  it('explains how to retry an Explorer open while another confirmation is active', async () => {
    await A.projectOpen('C:\\Muzyka\\First.wave');
    await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
    store.replaceState(await mock.getState());
    answer = 'busy';
    await A.projectOpen('C:\\Muzyka\\Second.wave');
    expect(store.S.path).toBe('C:\\Muzyka\\First.wave');
    expect((await import('./ui')).U.readout).toContain('try opening the project again');
  });
  it('keeps an edit made after the revision approved for opening', async () => {
    const current = 'C:\\Muzyka\\First.wave';
    await A.projectOpen(current);
    editDuringConfirm = async () => {
      await mock.applyOps([{ op: 'setBpm', bpm: 137 }]);
      store.replaceState(await mock.getState());
    };
    await A.projectOpen('C:\\Muzyka\\Second.wave');
    expect(store.S.path).toBe(current);
    expect(store.S.project.bpm).toBe(137);
    expect(store.S.dirty).toBe(true);
  });
});

describe('other actions while a project is opening', () => {
  it('refuse with feedback that fits the action', async () => {
    const ui = await import('./ui');
    ui.setU('openingProject', true);
    expect(await A.closeWindowGuarded()).toBe(false);
    expect(ui.U.readout).toContain('still opening');
    expect(ui.U.readout).not.toContain('opening the project again');
    ui.setU('openingProject', false);
  });
  it('word the busy message for the action when another question is open', async () => {
    const ui = await import('./ui');
    store.replaceState({ ...(await mock.getState()), dirty: true });
    answer = 'busy';
    expect(await A.closeWindowGuarded()).toBe(false);
    expect(ui.U.readout).not.toContain('opening the project');
    await A.projectOpen('C:\\Muzyka\\Second.wave');
    expect(ui.U.readout).toContain('try opening the project again');
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
  it('offers the copy after a launch that opened a file, naming that project', async () => {
    await crashWithUnsavedWork();
    const opened = 'C:\\Muzyka\\Opened.wave';
    await A.projectOpen(opened); // the Explorer double-click that started WAVE
    recAnswer = 'later';
    await A.offerRecovery();
    expect(recAsked).toBe(1);
    expect(recPath).toBe(opened);
    expect(store.S.path).toBe(opened);
  });
  it('keeps edits to the launched project when its Save / Discard question is cancelled', async () => {
    await crashWithUnsavedWork();
    await A.projectOpen('C:\\Muzyka\\Opened.wave');
    await mock.applyOps([{ op: 'setBpm', bpm: 99 }]);
    store.replaceState(await mock.getState());
    answer = 'cancel';
    await A.offerRecovery();
    expect(store.S.project.bpm).toBe(99);
    expect(store.S.dirty).toBe(true);
    expect((await mock.recoveryList()).length).toBe(1); // still offered at the next start
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
