import { describe, expect, it } from 'vitest';
import { SessionClient, type ControlInvoke } from './session';
import type { PatternDto } from './types';

const pattern = (key: number): PatternDto => ({ id: 1, name: 'Melody', steps: 16, channels: [{ channel: 0, mode: 'roll', notes: [{ key, start: 0, length: 240, velocity: 0.8 }] }] });
describe('shared session client', () => {
  it('ends a replaced gesture so it cannot leave a stale CLI exclusion lease', async () => {
    const requests: Parameters<ControlInvoke>[0][] = [];
    const invoke: ControlInvoke = async <T,>(request: Parameters<ControlInvoke>[0]) => {
      requests.push(request);
      return { revision: 0, data: null as T };
    };
    const client = new SessionClient(invoke);
    client.setGroup('Move notes');
    client.setGroup('Resize notes');
    client.setGroup();
    await client.call('get_pattern');
    expect(requests.slice(0, 4).map((r) => r.command)).toEqual(['gui_edit_begin', 'gui_edit_end', 'gui_edit_begin', 'gui_edit_end']);
    expect(requests[0]!.args.group).toEqual(requests[1]!.args.group);
    expect(requests[2]!.args.group).toEqual(requests[3]!.args.group);
  });
  it('sends keyboard notes immediately while a slow edit is pending', async () => {
    const calls: string[] = [];
    let finish!: () => void;
    const gate = new Promise<void>((resolve) => { finish = resolve; });
    const invoke: ControlInvoke = async <T,>(request: Parameters<ControlInvoke>[0]) => {
      calls.push(request.command);
      if (request.command === 'set_track_source') await gate;
      return { revision: 1, data: null as T };
    };
    const client = new SessionClient(invoke);
    const edit = client.call('set_track_source');
    await Promise.resolve();
    await client.call('note_on');
    expect(calls).toEqual(['set_track_source', 'note_on']);
    finish();
    await edit;
    expect(calls.filter((c) => c === 'note_on')).toHaveLength(1);
  });
  it('uses the last acknowledged pattern for consecutive edits in one gesture', async () => {
    const requests: Parameters<ControlInvoke>[0][] = [];
    let actual = pattern(60);
    const invoke: ControlInvoke = async <T,>(request: Parameters<ControlInvoke>[0]) => {
      requests.push(request);
      if (request.command === 'set_pattern') {
        expect(request.expectedPattern).toEqual(actual);
        actual = request.args.pattern as PatternDto;
      }
      return { revision: 1, data: null as T };
    };
    const client = new SessionClient(invoke);
    client.observe(actual);
    client.setGroup('Move notes');
    const first = client.call('set_pattern', { pattern: pattern(62) }, pattern(60));
    const second = client.call('set_pattern', { pattern: pattern(64) }, pattern(62));
    await Promise.all([first, second]);
    client.setGroup();
    await client.call('get_pattern');
    expect(requests.filter((r) => r.command === 'set_pattern')).toHaveLength(2);
  });
  it('does not replace a gesture base when an external update arrives', async () => {
    let actual = pattern(60);
    const invoke: ControlInvoke = async <T,>(request: Parameters<ControlInvoke>[0]) => {
      if (request.command === 'set_pattern' && JSON.stringify(request.expectedPattern) !== JSON.stringify(actual)) {
        return { revision: 3, data: null as T, error: { code: 'PATTERN_CONFLICT', message: 'changed' } };
      }
      if (request.command === 'set_pattern') actual = request.args.pattern as PatternDto;
      return { revision: 2, data: null as T };
    };
    const client = new SessionClient(invoke);
    client.observe(actual);
    client.setGroup('Move notes');
    await client.call('set_pattern', { pattern: pattern(62) }, actual);
    actual = pattern(67);
    client.observe(actual);
    await expect(client.call('set_pattern', { pattern: pattern(64) }, pattern(62))).rejects.toThrow('PATTERN_CONFLICT');
    expect(actual).toEqual(pattern(67));
    client.setGroup();
  });
});
