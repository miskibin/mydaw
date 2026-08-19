/**
 * A floating, resizable plugin window — the same contract as a hosted VST:
 * drag the title bar, resize the corner, close with the X or Escape.
 */

import { createSignal, onCleanup, type JSX } from 'solid-js';

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
  const [pos, setPos] = createSignal({
    x: 72 + props.offset * 28,
    y: 56 + props.offset * 24,
  });
  const [size, setSize] = createSignal({
    w: props.defaultWidth,
    h: props.defaultHeight,
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
          drag(event, (dx, dy) =>
            setPos({
              x: Math.max(8, Math.min(origin.x + dx, window.innerWidth - 96)),
              y: Math.max(8, Math.min(origin.y + dy, window.innerHeight - 64)),
            }),
          );
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
          drag(event, (dx, dy) =>
            setSize({
              w: Math.max(480, origin.w + dx),
              h: Math.max(320, origin.h + dy),
            }),
          );
        }}
      />
    </div>
  );
}
