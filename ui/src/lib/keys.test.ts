import { describe, expect, it } from 'vitest';
import { typingIn } from './keys';

const el = (tagName: string, type?: string, editable = false) => ({ tagName, isContentEditable: editable, getAttribute: (n: string) => (n === 'type' ? type ?? null : null) });

describe('typingIn — which focused elements keep Space and letter shortcuts', () => {
  it('text fields keep their keys', () => {
    expect(typingIn(el('INPUT'))).toBe(true);
    expect(typingIn(el('INPUT', 'text'))).toBe(true);
    expect(typingIn(el('INPUT', 'search'))).toBe(true);
    expect(typingIn(el('INPUT', 'number'))).toBe(true);
    expect(typingIn(el('TEXTAREA'))).toBe(true);
    expect(typingIn(el('DIV', undefined, true))).toBe(true);
  });
  it('a dropdown or slider that merely has focus does not swallow Space (stopping a take)', () => {
    expect(typingIn(el('SELECT'))).toBe(false);
    expect(typingIn(el('INPUT', 'range'))).toBe(false);
    expect(typingIn(el('INPUT', 'checkbox'))).toBe(false);
    expect(typingIn(el('BUTTON'))).toBe(false);
    expect(typingIn(null)).toBe(false);
  });
});
