/**
 * Stereo goniometer: L/R hops as a cloud, grab to set pan and width.
 */

import { onCleanup, onMount, type JSX } from 'solid-js';

import { goniometerFromPoint } from '../lib/goniometer';
import { onFrame } from './frameLoop';

export function Goniometer(props: {
  left: () => readonly number[];
  right: () => readonly number[];
  pan: number;
  widthPct: number;
  muted?: boolean;
  onDrag: (next: { pan: number; widthPct: number }) => void;
  onCommit: () => void;
}): JSX.Element {
  let canvas: HTMLCanvasElement | undefined;

  function paint(): void {
    const el = canvas;
    if (!el) return;
    const ctx = el.getContext('2d');
    if (!ctx) return;
    const w = el.clientWidth;
    const h = el.clientHeight;
    if (w < 2 || h < 2) return;
    const dpr = window.devicePixelRatio || 1;
    if (el.width !== Math.round(w * dpr) || el.height !== Math.round(h * dpr)) {
      el.width = Math.round(w * dpr);
      el.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const cx = w / 2;
    const cy = h / 2;
    ctx.strokeStyle = css('--grid2');
    ctx.beginPath();
    ctx.moveTo(cx, 0);
    ctx.lineTo(cx, h);
    ctx.moveTo(0, cy);
    ctx.lineTo(w, cy);
    ctx.stroke();

    const left = props.left();
    const right = props.right();
    const n = Math.min(left.length, right.length);
    ctx.fillStyle = props.muted ? css('--dim') : css('--acc');
    for (let i = 0; i < n; i++) {
      const l = left[i] ?? 0;
      const r = right[i] ?? 0;
      const x = cx + ((r - l) * 0.5) * (w * 0.45);
      const y = cy - ((l + r) * 0.5) * (h * 0.45);
      ctx.fillRect(x, y, 1.5, 1.5);
    }

    const scale = (props.widthPct / 100) * Math.min(w, h) * 0.28;
    const panX = cx + props.pan * (w * 0.4);
    ctx.strokeStyle = props.muted ? css('--dim') : css('--acc');
    ctx.beginPath();
    ctx.moveTo(panX, cy - scale);
    ctx.lineTo(panX + scale * 0.7, cy);
    ctx.lineTo(panX, cy + scale);
    ctx.lineTo(panX - scale * 0.7, cy);
    ctx.closePath();
    ctx.stroke();
  }

  onFrame(paint);

  onMount(() => {
    const el = canvas;
    if (!el) return;
    const down = (event: PointerEvent) => {
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      const move = (moveEvent: PointerEvent) => {
        const rect = el.getBoundingClientRect();
        props.onDrag(
          goniometerFromPoint(
            moveEvent.clientX - rect.left,
            moveEvent.clientY - rect.top,
            rect.width,
            rect.height,
          ),
        );
      };
      const up = () => {
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', up);
        window.removeEventListener('pointercancel', up);
        props.onCommit();
      };
      move(event);
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', up);
      window.addEventListener('pointercancel', up);
    };
    el.addEventListener('pointerdown', down);
    onCleanup(() => el.removeEventListener('pointerdown', down));
  });

  return (
    <canvas
      ref={canvas}
      class="wv-gonio"
      classList={{ 'is-muted': props.muted }}
      aria-label="Stereo goniometer. Drag to set pan and width."
    />
  );
}

function css(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || 'currentColor';
}
