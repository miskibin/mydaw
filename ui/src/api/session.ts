import type { PatternDto } from './types';

export interface SharedHistory {
  revision: number;
  undoLabel: string | null;
  redoLabel: string | null;
  undoCount: number;
  redoCount: number;
}
export interface SessionChange {
  revision: number;
  origin: string | null;
  command: string;
  history: SharedHistory;
}
interface ControlRequest {
  version: number;
  command: string;
  args: Record<string, unknown>;
  origin: 'gui';
  group?: string | undefined;
  label?: string | undefined;
  expectedPattern?: PatternDto;
}
interface ControlReply<T> {
  revision: number;
  data: T;
  error?: { code: string; message: string; details?: unknown } | null;
}
export type ControlInvoke = <T>(request: ControlRequest) => Promise<ControlReply<T>>;

/** Serializes GUI gestures and captures their base before an async invoke. */
export class SessionClient {
  private tail: Promise<unknown> = Promise.resolve();
  private group: { id: string; label: string } | undefined;
  private readonly accepted = new Map<string, PatternDto>();
  private readonly observed = new Map<number, PatternDto>();
  constructor(private readonly invoke: ControlInvoke) {}

  setGroup(label?: string): void {
    const previous = this.group;
    if (previous) {
      this.group = undefined;
      void this.call('gui_edit_end', { group: previous.id }).catch(console.error);
    }
    if (label) {
      this.group = { id: crypto.randomUUID(), label };
      void this.call('gui_edit_begin', { group: this.group.id }).catch(console.error);
    } else {
      this.group = undefined;
    }
  }
  observe(pattern: PatternDto): void {
    this.observed.set(pattern.id, structuredClone(pattern));
    if (this.observed.size > 64) this.observed.delete(this.observed.keys().next().value!);
  }

  call<T>(command: string, args: Record<string, unknown> = {}, expected?: PatternDto): Promise<T> {
    const group = this.group;
    const request: ControlRequest = { version: 1, command, args: structuredClone(args), origin: 'gui', group: group?.id, label: group?.label };
    const pattern = args.pattern as PatternDto | undefined;
    const baseline = expected ? structuredClone(expected) : (pattern ? this.observed.get(pattern.id) : undefined);
    const groupKey = group && pattern ? `${group.id}:${pattern.id}` : undefined;
    const perform = async (): Promise<T> => {
      if (command === 'set_pattern') {
        const base = (groupKey ? this.accepted.get(groupKey) : undefined) ?? baseline;
        if (!base) throw new Error('The pattern has not been read yet; refresh before editing.');
        request.expectedPattern = structuredClone(base);
      }
      const reply = await this.invoke<T>(request);
      if (reply.error) throw new Error(`${reply.error.code}: ${reply.error.message}`);
      if (pattern && command === 'set_pattern') {
        this.observe(pattern);
        if (groupKey) {
          this.accepted.set(groupKey, structuredClone(pattern));
          if (this.accepted.size > 64) this.accepted.delete(this.accepted.keys().next().value!);
        }
      }
      return reply.data;
    };
    if (/^(note_|transport_)/.test(command)) return perform();
    const result = this.tail.then(perform);
    this.tail = result.catch(() => {});
    return result;
  }
}
