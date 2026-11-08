/** Fields that take typed text keep their keys; a dropdown, slider or checkbox that merely
 * has focus after a click must not swallow Space (stopping a take) or other shortcuts. */
const TEXT_TYPES = new Set(['text', 'search', 'number', 'email', 'password', 'url', 'tel', '']);

export function typingIn(el: { tagName: string; isContentEditable?: boolean; getAttribute?(n: string): string | null } | null): boolean {
  if (!el) return false;
  if (el.tagName === 'TEXTAREA' || el.isContentEditable) return true;
  if (el.tagName === 'INPUT') return TEXT_TYPES.has((el.getAttribute?.('type') ?? '').toLowerCase());
  return false;
}
