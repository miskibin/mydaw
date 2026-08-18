import { onCleanup, onMount, type JSX } from 'solid-js';
import { useAppState } from '../../state';
import type { PanelProps } from '../types';
import { MeterRenderer, readMeterColors } from './draw';

/**
 * Stereo peak/RMS meter.
 *
 * Reactivity stops at the panel boundary on purpose: meter frames land in the
 * `MeterBus` (a plain mutable holder) and this component polls the latest one
 * from its own requestAnimationFrame loop. That keeps a 30-60 Hz stream from
 * driving Solid's scheduler, and makes this panel the measurement vehicle for
 * the Phase 1 IPC perf spike.
 */
export function MetersPanel(_props: PanelProps): JSX.Element {
  const app = useAppState();
  let canvas: HTMLCanvasElement | undefined;
  let host: HTMLDivElement | undefined;

  onMount(() => {
    if (!canvas || !host) return;
    const renderer = new MeterRenderer(canvas, readMeterColors(host));

    const applySize = () => {
      if (!host) return;
      const rect = host.getBoundingClientRect();
      renderer.resize(rect.width, rect.height, window.devicePixelRatio || 1);
    };
    applySize();

    const observer = new ResizeObserver(applySize);
    observer.observe(host);

    let raf = 0;
    const tick = (nowMs: number) => {
      renderer.draw(app.meters.latest, nowMs);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    onCleanup(() => {
      cancelAnimationFrame(raf);
      observer.disconnect();
    });
  });

  return (
    <div class="meters">
      <div class="meters__canvas-host" ref={host}>
        <canvas ref={canvas} class="meters__canvas" aria-label="Stereo output level meter" />
      </div>
      <dl class="meters__legend">
        <div>
          <dt>Peak</dt>
          <dd>bar tip</dd>
        </div>
        <div>
          <dt>RMS</dt>
          <dd>inner fill</dd>
        </div>
        <div>
          <dt>Hold</dt>
          <dd>0.9 s</dd>
        </div>
      </dl>
    </div>
  );
}
