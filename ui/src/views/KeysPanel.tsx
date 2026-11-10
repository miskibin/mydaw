// WAVE Keys: the sampled grand piano's page in the channel window. Two knobs (how long a
// released note rings, how much the velocity changes the level) and the state of the
// recordings: shipped with the app, or downloaded once into the sample library.
import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { KEYS_DYNAMICS, KEYS_RELEASE, type Channel, type KeysStatus } from '../api/types';
import { clamp } from '../lib/format';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { chanById, chanOp } from '../state/pattern';
import { say } from '../state/ui';
import { InstIcon } from '../ui/instIcons';
import { Knob } from '../ui/Knob';

type KeysChannel = Extract<Channel, { kind: 'keys' }>;

/** Release knob travel is logarithmic: 40 ms .. 3 s. */
const relT = (ms: number) => Math.log(ms / KEYS_RELEASE.min) / Math.log(KEYS_RELEASE.max / KEYS_RELEASE.min);
const relMs = (t: number) => Math.round(KEYS_RELEASE.min * Math.pow(KEYS_RELEASE.max / KEYS_RELEASE.min, clamp(t, 0, 1)));
const relText = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + ' s' : Math.round(ms) + ' ms');

export function KeysPanel(props: { c: KeysChannel }) {
  const live = () => chanById(props.c.id) as KeysChannel;
  const [st, setSt] = createSignal<KeysStatus | null>(null);
  /** Download progress 0..1 while the piano is being fetched. */
  const [dl, setDl] = createSignal<number | null>(null);
  const [err, setErr] = createSignal<string | null>(null);

  onMount(() => {
    void be().keysStatus().then(setSt).catch(e => setErr(String(e)));
    const offs = [
      be().on('keys-status', setSt),
      be().on('library-progress', p => {
        if (p.id !== (st()?.kit ?? 'salamander-piano')) return;
        if (p.error) { setDl(null); setErr(p.error); return; }
        setDl(p.total ? p.done / p.total : null);
        if (p.total && p.done >= p.total) { setDl(null); void be().keysStatus().then(setSt); }
      }),
    ];
    onCleanup(() => offs.forEach(o => void o.then(un => un())));
  });

  const download = async () => {
    const kit = st()?.kit; if (!kit) return;
    setErr(null); setDl(0);
    try { await be().libraryInstall(kit); say('WAVE Keys: fortepian pobrany'); }
    catch (e) { setErr(String(e)); }
    finally { setDl(null); void be().keysStatus().then(setSt); }
  };

  const knob = (label: string, title: string, t: () => number, text: () => string, patch: (t: number) => KeysChannel['keys'], def: number) => {
    let g: Gesture | null = null;
    const op = (v: number) => [chanOp(live(), { keys: patch(v) } as Partial<Channel>)];
    return (
      <Knob key={'keys' + label + props.c.id} label={label} title={title + ' · przeciągnij ↕ · Shift = precyzyjnie · 2× klik = reset'} t={t} text={text}
        onStart={() => { g = new Gesture(label + ' · WAVE Keys'); }}
        onChange={v => { g?.update(op(v)); say(props.c.name + ' · ' + label.toLowerCase() + ' ' + text()); }}
        onEnd={() => { g?.end(); g = null; }}
        onReset={() => commit(op(def), label + ' · WAVE Keys')}
        onWheel={v => wheelEdit('keys' + label, label + ' · WAVE Keys', op(v))} />
    );
  };

  const k = () => live()?.keys ?? props.c.keys;
  const state = () => st()?.state ?? 'idle';
  return (
    <div class="keyspanel">
      <div class="keyshd">
        <span class="big" style={{ color: props.c.color }}><InstIcon id="piano" size={34} /></span>
        <div class="keysinfo">
          <div class="nm">WAVE KEYS</div>
          <div class="note">Fortepian koncertowy z próbek: 30 nagranych dźwięków × 3 dynamiki, do 9 s wybrzmienia. Siła uderzenia zmienia barwę, nie tylko głośność.</div>
        </div>
      </div>
      <Show when={state() === 'missing' || (state() === 'idle' && st() && !st()!.installed) || dl() != null}>
        <div class="keysmsg">
          <Show when={dl() != null} fallback={<>
            <span>Nagrania fortepianu nie są jeszcze na tym komputerze (138 MB, pobierane raz). Do tego czasu kanał milczy.</span>
            <button class="smallbtn primary" onClick={download}>Pobierz fortepian</button>
          </>}>
            <span>Pobieranie fortepianu… {Math.round((dl() ?? 0) * 100)} %</span>
            <div class="keysbar"><div style={{ width: Math.round((dl() ?? 0) * 100) + '%' }} /></div>
          </Show>
        </div>
      </Show>
      <Show when={state() === 'loading' && dl() == null}><div class="keysmsg"><span>Wczytywanie nagrań… kanał zagra za chwilę.</span></div></Show>
      <Show when={err() ?? (state() === 'error' ? st()?.error ?? 'Nie udało się wczytać nagrań.' : null)}>
        {m => <div class="keysmsg bad"><span>{m()}</span><button class="smallbtn" onClick={download}>Spróbuj ponownie</button></div>}
      </Show>
      <div class="keysknobs">
        {knob('Wybrzmienie', 'Jak długo dźwięk gaśnie po puszczeniu klawisza', () => relT(k().release), () => relText(k().release), t => ({ ...k(), release: relMs(t) }), relT(KEYS_RELEASE.def))}
        {knob('Dynamika', 'Jak mocno siła uderzenia zmienia głośność (0 % = każda nuta równie głośna, barwa nadal zależy od siły)', () => k().dynamics / 100, () => Math.round(k().dynamics) + ' %', t => ({ ...k(), dynamics: Math.round(clamp(t, 0, 1) * 100) }), KEYS_DYNAMICS.def / 100)}
        <div class="keyscredit" title="Salamander Grand Piano V3, Alexander Holm, CC-BY-3.0 — w utworze z tym fortepianem podaj autora nagrań.">
          Nagrania: Salamander Grand Piano V3 · Alexander Holm · CC-BY 3.0
        </div>
      </div>
    </div>
  );
}
