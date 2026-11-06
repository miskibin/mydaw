// Pointer drag helper: window-level move/up listeners plus a body cursor while dragging.

export function track(move: (e: PointerEvent) => void, up?: ((e: PointerEvent) => void) | null, cursor?: string) {
  const u = (ev: PointerEvent) => {
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', u);
    window.removeEventListener('pointercancel', u);
    document.body.style.cursor = '';
    up?.(ev);
  };
  if (cursor) document.body.style.cursor = cursor;
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', u);
  window.addEventListener('pointercancel', u);
}

let lastTap: { k: string; t: number } | null = null;
/** True when this is the second tap on `key` within 320 ms (pointerdown-based double click). */
export function dblCheck(key: string): boolean {
  const now = performance.now();
  const d = !!lastTap && lastTap.k === key && now - lastTap.t < 320;
  lastTap = d ? null : { k: key, t: now };
  return d;
}
