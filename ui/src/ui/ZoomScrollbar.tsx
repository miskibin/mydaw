// A scrollbar that also zooms (FL Studio / Ableton style): drag the thumb to scroll, drag either
// end of it to zoom — that end follows the pointer, the other one stays put. Pressing the track
// beside the thumb centres the view there (and keeps dragging); the wheel over the bar scrolls,
// Ctrl+wheel zooms. Works in content units; the owner maps them to its own scroll and zoom.
import { createSignal, onCleanup, onMount } from 'solid-js';
import { centreAt, clampPos, dragEdge, dragThumb, thumbOf, zoomAround, type ZLimits, type ZRange } from '../lib/zoomBar';
import { track } from './drag';

const MIN_THUMB = 26;

export function ZoomScrollbar(props: {
  orient: 'h' | 'v';
  range: () => ZRange;
  limits: () => ZLimits;
  onChange: (r: ZRange) => void;
  title?: string;
  class?: string;
}) {
  let el!: HTMLDivElement;
  const vert = props.orient === 'v';
  const [L, setL] = createSignal(0);
  onMount(() => {
    const measure = () => setL(vert ? el.clientHeight : el.clientWidth);
    const ro = new ResizeObserver(measure);
    ro.observe(el); measure();
    onCleanup(() => ro.disconnect());
  });
  const thumb = () => thumbOf(props.range(), props.limits().total, L(), MIN_THUMB);
  const at = (e: PointerEvent | WheelEvent) => { const r = el.getBoundingClientRect(); return vert ? e.clientY - r.top : e.clientX - r.left; };

  const down = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation();
    const part = (e.target as HTMLElement).dataset.z, lim = props.limits(), W = L();
    let r0 = props.range(), p0 = at(e);
    if (part === 'lo' || part === 'hi') {
      const edge = part;
      track(ev => props.onChange(dragEdge(r0, edge, lim, W, at(ev) - p0)), null, vert ? 'ns-resize' : 'ew-resize');
      return;
    }
    if (part !== 'thumb') { r0 = centreAt(r0, lim.total, W, p0); props.onChange(r0); p0 = at(e); }
    track(ev => props.onChange(dragThumb(r0, lim.total, W, at(ev) - p0, MIN_THUMB)), null, 'grabbing');
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault(); e.stopPropagation();
    const d = e.deltaY || e.deltaX, r = props.range(), lim = props.limits();
    if (e.ctrlKey || e.metaKey) {
      const th = thumb();
      // zoom around the content under the pointer when it is on the thumb, else around the middle
      const p = at(e), anchor = p >= th.x && p <= th.x + th.w && th.w > 0 ? r.pos + ((p - th.x) / th.w) * r.len : r.pos + r.len / 2;
      props.onChange(zoomAround(r, lim, anchor, Math.pow(1.0015, d)));
    } else props.onChange(clampPos(r, lim.total, r.pos + (d / 100) * r.len * 0.25));
  };
  const pos = () => { const th = thumb(); return vert ? { top: th.x + 'px', height: th.w + 'px' } : { left: th.x + 'px', width: th.w + 'px' }; };
  const r = () => props.range(), tot = () => props.limits().total;
  return (
    <div ref={el} class={'zbar ' + (vert ? 'v' : 'h') + (props.class ? ' ' + props.class : '')} title={props.title}
      role="scrollbar" aria-orientation={vert ? 'vertical' : 'horizontal'}
      aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(tot() - r().len > 0 ? (r().pos / (tot() - r().len)) * 100 : 0)}
      onPointerDown={down} onWheel={wheel}>
      <div class="zthumb" data-z="thumb" style={pos()}>
        <div class="zgrip lo" data-z="lo" />
        <div class="zgrip hi" data-z="hi" />
      </div>
    </div>
  );
}
