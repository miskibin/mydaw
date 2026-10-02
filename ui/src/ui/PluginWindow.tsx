/**
 * A floating, resizable plugin window — the same contract as a hosted VST:
 * drag the title bar, resize the corner, close with the X or Escape.
 */

import { Show, createSignal, onCleanup, onMount, type JSX } from 'solid-js';
import { fitWindow, type WindowGeometry } from './windowGeometry';
import { bindPointerActivation } from './pointerActivation';

export function PluginWindow(props: {
  title: string;
  color?: string | undefined;
  z: number;
  focused: boolean;
  defaultWidth: number;
  defaultHeight: number;
  offset: number;
  class?: string;
  /** An editor's own toolbar can serve as its drag handle without a second row. */
  embeddedHeader?: boolean;
  minHeight?: number;
  geometry?: WindowGeometry | undefined;
  onGeometryChange?: (geometry: WindowGeometry) => void;
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
  const [pos, setPos] = createSignal(props.geometry ? { x: props.geometry.x, y: props.geometry.y } : {
    x: Math.max(8, Math.min(72 + props.offset * 28, window.innerWidth - initialWidth - 8)),
    y: Math.max(8, Math.min(24 + props.offset * 24, window.innerHeight - initialHeight - 8)),
  });
  const [size, setSize] = createSignal({
    // An editor must fit on first open. Its content can scroll, but the title
    // bar and resize handle must never start outside a small laptop viewport.
    w: props.geometry?.w ?? initialWidth,
    h: props.geometry?.h ?? initialHeight,
  });

  function update(held: WindowGeometry): void {
    const next = fitWindow(held, host(), props.minHeight);
    setPos({ x: next.x, y: next.y });
    setSize({ w: next.w, h: next.h });
    props.onGeometryChange?.(next);
  }

  onMount(() => {
    const fit = () => update({ ...pos(), ...size() });
    fit();
    const observer = new ResizeObserver(fit);
    const parent = root?.offsetParent;
    if (parent instanceof HTMLElement) observer.observe(parent);
    onCleanup(() => observer.disconnect());
    if (props.embeddedHeader && props.focused) root?.focus({ preventScroll: true });
  });

  let cancelDrag: (() => void) | undefined;
  onCleanup(() => cancelDrag?.());

  function drag(event: PointerEvent, move: (dx: number, dy: number) => void): void {
    if (event.button !== 0) return;
    cancelDrag?.();
    event.preventDefault();
    event.stopPropagation();
    props.onFocus();
    root?.focus({ preventScroll: true });
    const startX = event.clientX;
    const startY = event.clientY;
    const onMove = (next: PointerEvent) => move(next.clientX - startX, next.clientY - startY);
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      cancelDrag = undefined;
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
    cancelDrag = onUp;
  }

  function dragTitle(event: PointerEvent): void {
    origin.x = pos().x;
    origin.y = pos().y;
    drag(event, (dx, dy) => update({ x: origin.x + dx, y: origin.y + dy, ...size() }));
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (!props.focused || event.key !== 'Escape') return;
    // stopPropagation cannot silence sibling listeners on the same target, so
    // preventDefault is the claim flag: the first window to act marks the
    // event and every other open window stands down — one Escape, one close.
    if (event.defaultPrevented) return;
    const target = event.target as HTMLElement | null;
    // The rack must not consume Escape meant for a plugin or a popup above it.
    const overlay = target?.closest('.wv-plug, .wv-menu, .wv-modal');
    if (props.embeddedHeader && overlay && overlay !== root) return;
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
      ref={(element) => {
        root = element;
        bindPointerActivation(element, () => props.onFocus());
      }}
      class={`wv-plug ${props.class ?? ''}`}
      role="dialog"
      aria-label={props.title}
      tabindex={-1}
      classList={{ 'is-focused': props.focused }}
      style={{
        left: `${pos().x}px`,
        top: `${pos().y}px`,
        width: `${size().w}px`,
        height: `${size().h}px`,
        'z-index': props.z,
      }}
      onPointerDown={(event) => {
        const target = event.target as HTMLElement;
        const control = target.closest('button, input, textarea, select, a, [tabindex], [contenteditable="true"]');
        if (!control || control === root) {
          root?.focus({ preventScroll: true });
          if (props.embeddedHeader && target.closest('[data-window-drag]')) dragTitle(event);
        }
      }}
      onFocusIn={() => props.onFocus()}
    >
      <Show when={!props.embeddedHeader}>
        <div
          class="wv-plug__bar"
          onPointerDown={(event) => {
            if ((event.target as HTMLElement).closest('button')) return;
            dragTitle(event);
          }}
        >
          <span class="wv-swatch" style={{ background: props.color }} />
          <span class="wv-plug__title">{props.title}</span>
          <span class="wv-spacer" />
          <button type="button" class="wv-chip" title="Close" onClick={() => props.onClose()}>
            ×
          </button>
        </div>
      </Show>
      <div class="wv-plug__body">{props.children}</div>
      <div
        class="wv-plug__resize"
        onPointerDown={(event) => {
          origin.w = size().w;
          origin.h = size().h;
          drag(event, (dx, dy) => {
            update({ ...pos(), w: origin.w + dx, h: origin.h + dy });
          });
        }}
      />
    </div>
  );
}
