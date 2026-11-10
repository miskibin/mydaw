// A piano keyboard you can click: plays the channel's notes like the keyboard at the bottom
// of a sampler in FL Studio. A key sounds while it is held; lower on a key = harder hit; drag
// across keys to slide. Keys held on the MIDI keyboard or the typing keyboard light up too.
import { createSignal, For, onCleanup, onMount } from 'solid-js';
import { be } from '../api/backend';
import type { Id } from '../api/types';
import { isBlack, noteName } from '../lib/pitch';
import { typingHeld } from '../state/typingKeys';
import { say } from '../state/ui';

export function PianoKeys(props: { ch: Id; lo: number; hi: number; mark?: number }) {
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
  const send = (key: number, vel: number, on: boolean) => be().typingNote({ ch: props.ch, key, vel, on }).catch(err => { if (on) say('Nie zagrano nuty: ' + String(err)); });
  const release = () => { const k = down(); if (k != null) { setDown(null); void send(k, 0, false); } };
  const play = (e: PointerEvent) => {
    const h = hit(e);
    if (!h || h.key === down()) return;
    release();
    setDown(h.key);
    void send(h.key, h.vel, true);
  };

  onMount(() => {
    const off = be().on('midi-note', ev => setHeld(s => { const n = new Set(s); if (ev.note.on) n.add(ev.note.key); else n.delete(ev.note.key); return n; }));
    onCleanup(() => { release(); void off.then(un => un()); });
  });

  const lit = (k: number) => down() === k || held().has(k) || typingHeld().includes(k);
  return (
    <div ref={el} class="pkeys" role="group" aria-label="Klawiatura: kliknij, żeby zagrać" title="Kliknij i trzymaj klawisz, żeby zagrać · niżej = mocniej · przeciągnij = glissando"
      onPointerDown={e => { if (e.button !== 0) return; el.setPointerCapture(e.pointerId); play(e); }}
      onPointerMove={e => { if (down() != null) play(e); }}
      onPointerUp={release} onPointerCancel={release} onLostPointerCapture={release}>
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
