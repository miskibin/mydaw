// A piano keyboard you can click: plays the channel's notes like the keyboard at the bottom
// of a sampler in FL Studio. Lower on a key = harder hit; drag across keys to slide.
// Keys held on the MIDI keyboard light up too.
import { createSignal, For, onCleanup, onMount } from 'solid-js';
import { be } from '../api/backend';
import type { Id } from '../api/types';
import { isBlack, noteName } from '../lib/pitch';
import { say } from '../state/ui';

/** Seconds a clicked key sounds (then the instrument's own release). */
const CLICK_LEN = 1.2;

export function PianoKeys(props: { ch: Id; lo: number; hi: number; mark?: number; pattern?: Id | null }) {
  let el!: HTMLDivElement;
  const [down, setDown] = createSignal<number | null>(null);
  const [held, setHeld] = createSignal<ReadonlySet<number>>(new Set());
  // whole white keys at both ends
  const lo = () => (isBlack(props.lo) ? props.lo - 1 : props.lo);
  const hi = () => (isBlack(props.hi) ? props.hi + 1 : props.hi);
  const whites = () => { const w: number[] = []; for (let k = lo(); k <= hi(); k++) if (!isBlack(k)) w.push(k); return w; };
  const blacks = () => { const b: { key: number; at: number }[] = []; whites().forEach((k, i) => { if (k + 1 <= hi() && isBlack(k + 1)) b.push({ key: k + 1, at: i + 1 }); }); return b; };

  /** Key and velocity under a pointer. */
  const hit = (e: PointerEvent): { key: number; vel: number } | null => {
    const r = el.getBoundingClientRect(), ws = whites();
    const x = e.clientX - r.left, y = e.clientY - r.top;
    if (x < 0 || x >= r.width || !ws.length) return null;
    const ww = r.width / ws.length, i = Math.min(ws.length - 1, Math.floor(x / ww));
    const vel = (h: number) => Math.round((0.35 + 0.65 * Math.min(1, Math.max(0, y / h))) * 100) / 100;
    const bh = r.height * 0.6;
    if (y < bh) {
      const near = Math.round(x / ww), b = blacks().find(q => q.at === near);
      if (b && Math.abs(x - near * ww) < ww * 0.32) return { key: b.key, vel: vel(bh) };
    }
    return { key: ws[i], vel: vel(r.height) };
  };
  const play = (e: PointerEvent) => {
    const h = hit(e);
    if (!h || h.key === down()) return;
    setDown(h.key);
    be().noteAudition({ ch: props.ch, key: h.key, vel: h.vel, pattern: props.pattern ?? undefined, len: CLICK_LEN }).catch(err => say('Nie zagrano nuty: ' + String(err)));
  };

  onMount(() => {
    const off = be().on('midi-note', ev => setHeld(s => { const n = new Set(s); if (ev.note.on) n.add(ev.note.key); else n.delete(ev.note.key); return n; }));
    onCleanup(() => void off.then(un => un()));
  });

  const lit = (k: number) => down() === k || held().has(k);
  return (
    <div ref={el} class="pkeys" role="group" aria-label="Klawiatura: kliknij, żeby zagrać" title="Kliknij klawisz, żeby zagrać · niżej = mocniej · przeciągnij = glissando"
      onPointerDown={e => { if (e.button !== 0) return; el.setPointerCapture(e.pointerId); play(e); }}
      onPointerMove={e => { if (down() != null) play(e); }}
      onPointerUp={() => setDown(null)} onPointerCancel={() => setDown(null)}>
      <For each={whites()}>{(k, i) => (
        <div class="w" classList={{ on: lit(k) }} style={{ left: (i() / whites().length) * 100 + '%', width: 100 / whites().length + '%' }}>
          {k % 12 === 0 && <span class="lbl">{noteName(k)}</span>}
          {props.mark === k && <span class="mark" title="Nuta bazowa" />}
        </div>
      )}</For>
      <For each={blacks()}>{b => (
        <div class="b" classList={{ on: lit(b.key) }} style={{ left: 'calc(' + (b.at / whites().length) * 100 + '% - ' + (32 / whites().length) + '%)', width: 64 / whites().length + '%' }}>
          {props.mark === b.key && <span class="mark" title="Nuta bazowa" />}
        </div>
      )}</For>
    </div>
  );
}
