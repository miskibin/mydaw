/**
 * A floating, resizable plugin window — the same contract as a hosted VST:
 * drag the title bar, resize the corner, close with the X or Escape.
 */

import { createSignal, onCleanup, onMount, type JSX } from 'solid-js';

export function PluginWindow(props: {
  title: string;
  color?: string | undefined;
  z: number;
  focused: boolean;
  defaultWidth: number;
  defaultHeight: number;
  offset: number;
  onFocus: () => void;
  onClose: () => void;
  children: JSX.Element;
}): JSX.Element {
  let root: HTMLDivElement | undefined;

  // The window is absolutely positioned inside `.wv-body`, which sits below
  // the top bar — so the viewport is the wrong frame of reference: a window
  // clamped to `innerHeight` hangs its bottom edge below the screen by the
  // top bar's height. Everything clamps against the positioned ancestor.
  const host = (): { w: number; h: number } => {
    const el = root?.offsetParent as HTMLElement | null;
    return {
      w: el?.clientWidth ?? window.innerWidth,
      h: el?.clientHeight ?? window.innerHeight,
    };
  };

  const initialWidth = Math.min(props.defaultWidth, Math.max(320, window.innerWidth - 16));
  const initialHeight = Math.min(props.defaultHeight, Math.max(260, window.innerHeight - 16));
  const [pos, setPos] = createSignal({
    x: Math.max(8, Math.min(72 + props.offset * 28, window.innerWidth - initialWidth - 8)),
    y: Math.max(8, Math.min(24 + props.offset * 24, window.innerHeight - initialHeight - 8)),
  });
  const [size, setSize] = createSignal({
    // An editor must fit on first open. Its content can scroll, but the title
    // bar and resize handle must never start outside a small laptop viewport.
    w: initialWidth,
    h: initialHeight,
  });

  onMount(() => {
    const bounds = host();
    const w = Math.min(size().w, Math.max(320, bounds.w - 16));
    const h = Math.min(size().h, Math.max(260, bounds.h - 16));
    setSize({ w, h });
    setPos((held) => ({
      x: Math.max(8, Math.min(held.x, bounds.w - w - 8)),
      y: Math.max(8, Math.min(held.y, bounds.h - h - 8)),
    }));
  });

  function drag(event: PointerEvent, move: (dx: number, dy: number) => void): void {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    props.onFocus();
    const startX = event.clientX;
    const startY = event.clientY;
    const onMove = (next: PointerEvent) => move(next.clientX - startX, next.clientY - startY);
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (!props.focused || event.key !== 'Escape') return;
    // stopPropagation cannot silence sibling listeners on the same target, so
    // preventDefault is the claim flag: the first window to act marks the
    // event and every other open window stands down — one Escape, one close.
    if (event.defaultPrevented) return;
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.isContentEditable)) return;
    event.preventDefault();
    event.stopPropagation();
    props.onClose();
  }

  window.addEventListener('keydown', onKeyDown, true);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown, true));

  const origin = { x: 0, y: 0, w: 0, h: 0 };

  return (
    <div
      ref={root}
      class="wv-plug"
      classList={{ 'is-focused': props.focused }}
      style={{
        left: `${pos().x}px`,
        top: `${pos().y}px`,
        width: `${size().w}px`,
        height: `${size().h}px`,
        'z-index': props.z,
      }}
      onPointerDown={() => props.onFocus()}
    >
      <div
        class="wv-plug__bar"
        onPointerDown={(event) => {
          origin.x = pos().x;
          origin.y = pos().y;
          drag(event, (dx, dy) => {
            const bounds = host();
            setPos({
              x: Math.max(8, Math.min(origin.x + dx, bounds.w - 96)),
              y: Math.max(8, Math.min(origin.y + dy, bounds.h - 64)),
            });
          });
        }}
      >
        <span class="wv-swatch" style={{ background: props.color }} />
        <span class="wv-plug__title">{props.title}</span>
        <span class="wv-spacer" />
        <button type="button" class="wv-chip" title="Close" onClick={() => props.onClose()}>
          ×
        </button>
      </div>
      <div class="wv-plug__body">{props.children}</div>
      <div
        class="wv-plug__resize"
        onPointerDown={(event) => {
          origin.w = size().w;
          origin.h = size().h;
          drag(event, (dx, dy) => {
            const bounds = host();
            setSize({
              w: Math.max(Math.min(480, bounds.w - 16), origin.w + dx),
              h: Math.max(Math.min(320, bounds.h - 16), origin.h + dy),
            });
          });
        }}
      />
    </div>
  );
}
