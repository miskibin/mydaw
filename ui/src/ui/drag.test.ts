import { beforeEach, describe, expect, it, vi } from 'vitest';
import { dragCursor, track } from './drag';

// a minimal DOM: the root element's classes and custom properties, and window listeners
const cls = new Set<string>(), props = new Map<string, string>();
const listeners = new Map<string, (e: unknown) => void>();
vi.stubGlobal('document', {
  documentElement: {
    classList: { add: (c: string) => cls.add(c), remove: (c: string) => cls.delete(c) },
    style: { setProperty: (k: string, v: string) => props.set(k, v), removeProperty: (k: string) => props.delete(k) },
  },
});
vi.stubGlobal('window', {
  addEventListener: (k: string, f: (e: unknown) => void) => listeners.set(k, f),
  removeEventListener: (k: string) => listeners.delete(k),
});
beforeEach(() => { cls.clear(); props.clear(); listeners.clear(); });

describe('drag cursor', () => {
  it('forces the drag cursor on the whole page, and releases it', () => {
    dragCursor('grabbing');
    expect(cls.has('dragcur')).toBe(true);
    expect(props.get('--drag-cursor')).toBe('grabbing');
    dragCursor(null);
    expect(cls.has('dragcur')).toBe(false);
    expect(props.has('--drag-cursor')).toBe(false);
  });
  it('shows a closed hand for the whole of a grab drag, released on pointerup', () => {
    const up = vi.fn();
    track(() => {}, up, 'grabbing');
    expect(props.get('--drag-cursor')).toBe('grabbing');
    listeners.get('pointerup')!({});
    expect(cls.has('dragcur')).toBe(false);
    expect(up).toHaveBeenCalledOnce();
  });
  it('releases it when the pointer is cancelled', () => {
    track(() => {}, null, 'grabbing');
    listeners.get('pointercancel')!({});
    expect(cls.has('dragcur')).toBe(false);
  });
  it('leaves the cursor alone for a drag that names none', () => {
    track(() => {});
    expect(cls.has('dragcur')).toBe(false);
  });
});
