// Add-effect picker (issue #18): opens at the slot it adds to, search-first and keyboard-complete
// (type, ↑↓←→, Enter adds, Shift+Enter / → on the preset column picks a preset, Esc closes).
// Category rail on the left, tiles with icon + one-line description + preset count, a preset
// column for the focused effect, Recent and Favourites at the top, built-in and VST3 separated.
// A tile can also be dragged onto any mixer strip or the effect rack to add it there.
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import type { BuiltinKind, Id, Vst3Info } from '../api/types';
import { be } from '../api/backend';
import { presetList } from '../lib/presets';
import * as A from '../state/actions';
import { S } from '../state/store';
import { say, setU, U } from '../state/ui';
import { FX_ORDER, FXMETA, FxTile, Icon, ICONS } from '../ui/icons';

/** What each built-in effect is for, in one line. */
const DESC: Record<BuiltinKind, string> = {
  denoise: 'Usuwa szum, brum i pogłos pomieszczenia z nagrań',
  tune: 'Strojenie wokalu na żywo — od naturalnego po twardy efekt',
  voice: 'Wysokość, formanty, robot i przester głosu',
  eq: '24 pasma, dynamiczne, M/S — rzeźbienie barwy',
  deess: 'Ścisza ostre „s” i „sz” bez gaszenia góry',
  comp: 'Wyrównuje dynamikę, klej dla śladu lub sumy',
  fet: 'Szybki kompresor w stylu 1176 — charakter i atak',
  sat: 'Ciepło taśmy i lampy, harmoniczne',
  delay: 'Echo zsynchronizowane z tempem, ping-pong',
  reverb: 'Przestrzeń: pokój, plate, hala',
  limit: 'Limiter z lookahead — głośność bez przesterów',
};

/** Extra search words (Polish and English names people type). */
const KW: Record<BuiltinKind, string> = {
  denoise: 'szum noise denoiser odszumianie brum hum', tune: 'autotune auto-tune pitch strojenie melodyne', voice: 'pitch formant robot vocoder głos',
  eq: 'korektor equalizer eq filtr filter', deess: 'deesser de-esser sybilanty', comp: 'kompresor compressor dynamika',
  fet: 'kompresor compressor 1176 fet', sat: 'saturacja saturator przester distortion drive taśma tape', delay: 'echo opóźnienie delay',
  reverb: 'pogłos reverb przestrzeń hall room plate', limit: 'limiter głośność loudness master',
};

type Cat = { id: string; label: string; types?: BuiltinKind[] };
const CATS: Cat[] = [
  { id: 'all', label: 'Wszystkie' },
  { id: 'recent', label: 'Ostatnie' },
  { id: 'fav', label: 'Ulubione' },
  { id: 'vocal', label: 'Wokal i strojenie', types: ['tune', 'voice', 'deess'] },
  { id: 'repair', label: 'Naprawa', types: ['denoise', 'deess'] },
  { id: 'dyn', label: 'Dynamika', types: ['comp', 'fet', 'limit'] },
  { id: 'tone', label: 'EQ i barwa', types: ['eq', 'sat'] },
  { id: 'space', label: 'Czas i przestrzeń', types: ['delay', 'reverb'] },
  { id: 'vst', label: 'VST3' },
];

/** An entry in the grid: a built-in effect or a scanned VST3 plugin. */
type Entry = { key: string; builtin?: BuiltinKind; vst?: Vst3Info; name: string; desc: string; presets: number };

// ---- per-user conveniences (best effort, never required)
const LS_RECENT = 'wave.fxpicker.recent', LS_FAV = 'wave.fxpicker.fav';
const load = (k: string): string[] => { try { const v = JSON.parse(localStorage.getItem(k) ?? '[]'); return Array.isArray(v) ? v.filter(x => typeof x === 'string') : []; } catch { return []; } };
const save = (k: string, v: string[]) => { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* storage unavailable */ } };
const [recent, setRecent] = createSignal<string[]>(load(LS_RECENT));
const [favs, setFavs] = createSignal<string[]>(load(LS_FAV));
const remember = (key: string) => { const r = [key, ...recent().filter(k => k !== key)].slice(0, 8); setRecent(r); save(LS_RECENT, r); };
const toggleFav = (key: string) => { const f = favs().includes(key) ? favs().filter(k => k !== key) : [...favs(), key]; setFavs(f); save(LS_FAV, f); };

// VST3 list is scanned once per session (rescan lives in Settings)
const [vsts, setVsts] = createSignal<Vst3Info[] | null>(null);
let scanning = false;
function ensureVsts() {
  if (vsts() || scanning) return;
  scanning = true;
  be().vst3Scan(false).then(setVsts).catch(() => setVsts([])).finally(() => { scanning = false; });
}

const vstKey = (p: Vst3Info) => 'vst:' + p.cid;

/** Opens the picker for an insert, anchored at (x, y) — or centred when no point is given (F8). */
export function openFxPicker(insId: Id, x?: number, y?: number) {
  setU('fxPicker', { insId, x: x ?? -1, y: y ?? -1 });
}

export function FxPickerHost() {
  return <Show when={U.fxPicker}>{p => <FxPicker insId={p().insId} x={p().x} y={p().y} />}</Show>;
}

const W = 900, H = 520;

function FxPicker(props: { insId: Id; x: number; y: number }) {
  const [q, setQ] = createSignal('');
  const [cat, setCat] = createSignal('all');
  const [focus, setFocus] = createSignal(0);
  const [presetFocus, setPresetFocus] = createSignal(-1); // -1: grid has the keyboard
  let input!: HTMLInputElement, grid!: HTMLDivElement;
  const close = () => setU('fxPicker', null);
  onMount(() => { ensureVsts(); setTimeout(() => input?.focus(), 0); });

  const insName = () => { const m = S.project.inserts.find(i => i.id === props.insId); return m ? (m.id === 0 ? 'Master' : m.name) : ''; };

  const builtins = createMemo<Entry[]>(() => FX_ORDER.filter(t => S.catalog?.types[t]).map(t => ({
    key: t, builtin: t, name: S.catalog!.types[t]!.name, desc: DESC[t] ?? FXMETA[t].tag, presets: presetList(S.catalog, t).length,
  })));
  const plugins = createMemo<Entry[]>(() => (vsts() ?? []).map(p => ({ key: vstKey(p), vst: p, name: p.name, desc: [p.vendor, p.category].filter(Boolean).join(' · '), presets: 0 })));

  const shown = createMemo(() => {
    const s = q().trim().toLowerCase();
    const c = CATS.find(x => x.id === cat())!;
    const all = [...builtins(), ...plugins()];
    let list: Entry[];
    if (s) list = all; // search always covers everything; the active category is shown as a chip
    else if (c.id === 'recent') list = recent().map(k => all.find(e => e.key === k)).filter((e): e is Entry => !!e);
    else if (c.id === 'fav') list = favs().map(k => all.find(e => e.key === k)).filter((e): e is Entry => !!e);
    else if (c.id === 'vst') list = plugins();
    else if (c.types) list = builtins().filter(e => c.types!.includes(e.builtin!));
    else list = all;
    if (!s) return list;
    const words = s.split(/\s+/);
    // rank: name, then keywords/tag, then description
    const score = (e: Entry) => {
      const name = e.name.toLowerCase(), kw = (e.builtin ? FXMETA[e.builtin].tag + ' ' + KW[e.builtin] : 'vst3 plugin wtyczka').toLowerCase(), desc = e.desc.toLowerCase();
      let sc = 0;
      for (const w of words) {
        if (name.startsWith(w)) sc += 0; else if (name.includes(w)) sc += 1; else if (kw.includes(w)) sc += 2; else if (desc.includes(w)) sc += 4; else return -1;
      }
      return sc;
    };
    return list.map(e => [e, score(e)] as const).filter(([, sc]) => sc >= 0).sort((a, b) => a[1] - b[1]).map(([e]) => e);
  });
  createEffect(() => { shown(); setFocus(0); setPresetFocus(-1); });
  const cur = () => shown()[Math.min(focus(), shown().length - 1)];
  const curPresets = () => { const e = cur(); return e?.builtin ? presetList(S.catalog, e.builtin) : []; };

  const add = (e: Entry | undefined, preset?: number) => {
    if (!e) return;
    if (e.builtin) A.addFx(props.insId, e.builtin, preset);
    else if (e.vst) { A.addVst(props.insId, e.vst); say('Dodano ' + e.vst.name); }
    remember(e.key);
    close();
  };

  // grid geometry for arrow keys
  const cols = () => { const w = grid?.clientWidth ?? 400; return Math.max(1, Math.floor((w + 8) / (200 + 8))); };
  const keep = (i: number) => { setFocus(i); queueMicrotask(() => grid?.querySelector(`[data-i="${i}"]`)?.scrollIntoView({ block: 'nearest' })); };
  const onKey = (e: KeyboardEvent) => {
    e.stopPropagation();
    const n = shown().length;
    if (e.key === 'Escape') { e.preventDefault(); if (presetFocus() >= 0) return setPresetFocus(-1); return close(); }
    if (presetFocus() >= 0) {
      const np = curPresets().length;
      if (e.key === 'ArrowDown') { e.preventDefault(); setPresetFocus(Math.min(np - 1, presetFocus() + 1)); }
      else if (e.key === 'ArrowUp') { e.preventDefault(); setPresetFocus(Math.max(0, presetFocus() - 1)); }
      else if (e.key === 'ArrowLeft') { e.preventDefault(); setPresetFocus(-1); }
      else if (e.key === 'Enter') { e.preventDefault(); add(cur(), presetFocus()); }
      return;
    }
    if (!n) return;
    if (e.key === 'ArrowRight') {
      // at the end of a row, → steps into the preset column
      if ((focus() % cols() === cols() - 1 || focus() === n - 1) && curPresets().length) { e.preventDefault(); return setPresetFocus(0); }
      if (input.selectionStart === input.value.length) { e.preventDefault(); keep(Math.min(n - 1, focus() + 1)); }
    } else if (e.key === 'ArrowLeft') {
      if (input.selectionStart === 0 || !q()) { e.preventDefault(); keep(Math.max(0, focus() - 1)); }
    } else if (e.key === 'ArrowDown') { e.preventDefault(); keep(Math.min(n - 1, focus() + cols())); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); keep(Math.max(0, focus() - cols())); }
    else if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey && curPresets().length) setPresetFocus(0); else add(cur());
    } else if (e.key === 'Tab') {
      e.preventDefault();
      const i = CATS.findIndex(c => c.id === cat()); setCat(CATS[(i + (e.shiftKey ? CATS.length - 1 : 1)) % CATS.length].id); setQ('');
    }
  };

  // drag a tile onto a mixer strip / effect rack
  const [drag, setDrag] = createSignal<{ e: Entry; x: number; y: number; over: Id | null } | null>(null);
  const insAt = (x: number, y: number): Id | null => {
    const el = document.elementFromPoint(x, y)?.closest('[data-ins]') as HTMLElement | null;
    return el ? Number(el.dataset.ins) : null;
  };
  const tileDown = (ev: PointerEvent, e: Entry, i: number) => {
    if (ev.button !== 0) return;
    setFocus(i); setPresetFocus(-1);
    const x0 = ev.clientX, y0 = ev.clientY;
    let dragging = false;
    const mv = (m: PointerEvent) => {
      if (!dragging && Math.hypot(m.clientX - x0, m.clientY - y0) < 6) return;
      dragging = true;
      setDrag({ e, x: m.clientX, y: m.clientY, over: insAt(m.clientX, m.clientY) });
    };
    const up = (u: PointerEvent) => {
      window.removeEventListener('pointermove', mv); window.removeEventListener('pointerup', up);
      // find the drop target while the picker is still click-through, then end the drag
      const target = dragging ? insAt(u.clientX, u.clientY) : null;
      setDrag(null);
      if (!dragging) return add(e);
      if (target != null) {
        if (e.builtin) A.addFx(target, e.builtin); else if (e.vst) A.addVst(target, e.vst);
        remember(e.key); close();
      }
    };
    window.addEventListener('pointermove', mv); window.addEventListener('pointerup', up);
  };
  onCleanup(() => setDrag(null));

  // anchored at the slot, always fully on screen
  const pos = () => {
    const vw = window.innerWidth, vh = window.innerHeight, m = 12;
    const w = Math.min(W, vw - 2 * m), h = Math.min(H, vh - 2 * m);
    if (props.x < 0) return { left: (vw - w) / 2, top: Math.max(m, (vh - h) / 2.4), w, h };
    return { left: Math.min(Math.max(m, props.x - w / 2), vw - w - m), top: Math.min(Math.max(m, props.y - h - 10 > m ? props.y - h - 10 : props.y + 10), vh - h - m), w, h };
  };
  const activeCat = () => CATS.find(c => c.id === cat())!;

  return (
    <div class="fxpick-veil" style={{ 'pointer-events': drag() ? 'none' : 'auto' }} onPointerDown={close}>
      <div class="fxpick" style={{ left: pos().left + 'px', top: pos().top + 'px', width: pos().w + 'px', height: pos().h + 'px', 'pointer-events': drag() ? 'none' : 'auto', opacity: drag() ? 0.35 : 1 }}
        onPointerDown={e => e.stopPropagation()}>
        <div class="fxp-hd">
          <span class="fxp-search">
            <Icon d="M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15z M21 21l-5.2-5.2" size={14} />
            <input ref={input} placeholder="Szukaj efektu lub wtyczki…" value={q()} onInput={e => setQ(e.currentTarget.value)} onKeyDown={onKey} spellcheck={false} />
            <Show when={q()}><button class="fxp-clear" title="Wyczyść (Esc)" onClick={() => { setQ(''); input.focus(); }}>×</button></Show>
          </span>
          <span class="fxp-dest">do <b>{insName()}</b></span>
          <button class="fxp-x" onClick={close} title="Zamknij (Esc)">×</button>
        </div>
        <div class="fxp-body">
          <nav class="fxp-rail">
            <For each={CATS}>
              {(c, i) => (
                <>
                  <Show when={i() === 3 || i() === 8}><div class="fxp-sep" /></Show>
                  <button classList={{ on: cat() === c.id && !q() }} onClick={() => { setCat(c.id); setQ(''); input.focus(); }}>
                    <span>{c.label}</span>
                    <span class="n">{c.id === 'recent' ? recent().length || '' : c.id === 'fav' ? favs().length || '' : c.id === 'vst' ? (vsts() ? vsts()!.length : '…') : c.types ? c.types.length : builtins().length + plugins().length}</span>
                  </button>
                </>
              )}
            </For>
          </nav>
          <div class="fxp-main">
            <div class="fxp-title">
              <Show when={q()} fallback={<span>{activeCat().label}</span>}>
                <span>Wyniki dla „{q()}”</span><span class="fxp-chip">{shown().length}</span>
              </Show>
            </div>
            <div class="fxp-grid" ref={grid}>
              <For each={shown()} fallback={<div class="fxp-empty">{cat() === 'fav' && !q() ? 'Kliknij ☆ na kafelku, żeby dodać efekt do ulubionych.' : cat() === 'recent' && !q() ? 'Tu pojawią się ostatnio dodane efekty.' : cat() === 'vst' && !vsts() ? 'Skanowanie wtyczek VST3…' : 'Nic nie pasuje.'}</div>}>
                {(e, i) => (
                  <div class="fxp-tile" data-i={i()} classList={{ focus: i() === focus() && presetFocus() < 0, cur: i() === focus() }}
                    onPointerDown={ev => tileDown(ev, e, i())} onPointerEnter={() => presetFocus() < 0 && setFocus(i())} title={e.vst?.path ?? 'Klik lub Enter: dodaj · przeciągnij na kanał miksera'}>
                    <Show when={e.builtin} fallback={<span class="tile" style={{ width: '40px', height: '40px', background: FXMETA.vst3.color + '26', color: FXMETA.vst3.color }}><Icon d={ICONS.vst} size={20} /></span>}>
                      <FxTile type={e.builtin!} size={40} icon={20} />
                    </Show>
                    <div class="fxp-txt">
                      <div class="fxp-name"><span class="ell">{e.name}</span>{e.vst && <span class="fxp-badge">VST3</span>}</div>
                      <div class="fxp-desc">{e.desc}</div>
                      <Show when={e.presets}><div class="fxp-meta">{e.presets} {e.presets === 1 ? 'preset' : e.presets < 5 ? 'presety' : 'presetów'}</div></Show>
                    </div>
                    <button class="fxp-star" classList={{ on: favs().includes(e.key) }} title={favs().includes(e.key) ? 'Usuń z ulubionych' : 'Dodaj do ulubionych'}
                      onPointerDown={ev => { ev.stopPropagation(); }} onClick={ev => { ev.stopPropagation(); toggleFav(e.key); }}>{favs().includes(e.key) ? '★' : '☆'}</button>
                  </div>
                )}
              </For>
            </div>
          </div>
          <aside class="fxp-presets">
            <Show when={cur()} fallback={<div class="fxp-empty">—</div>}>
              {e => (
                <>
                  <div class="fxp-ph">
                    <Show when={e().builtin}><FxTile type={e().builtin!} size={22} icon={12} /></Show>
                    <span class="ell">{e().name}</span>
                  </div>
                  <Show when={curPresets().length} fallback={<div class="fxp-note">{e().vst ? 'Presety wtyczki są w jej własnym oknie.' : 'Brak presetów.'}</div>}>
                    <div class="fxp-plabel">Dodaj z presetem</div>
                    <For each={curPresets()}>
                      {(p, pi) => (
                        <button class="fxp-pi" classList={{ focus: presetFocus() === pi() }} onClick={() => add(e(), pi())} onPointerEnter={() => setPresetFocus(pi())} onPointerLeave={() => setPresetFocus(-1)}>
                          <span class="ell">{p.name}</span>{pi() === 0 && <span class="fxp-def">domyślny</span>}
                        </button>
                      )}
                    </For>
                  </Show>
                </>
              )}
            </Show>
          </aside>
        </div>
        <div class="fxp-ft">
          <span><kbd>↑↓←→</kbd> wybór</span><span><kbd>Enter</kbd> dodaj</span><span><kbd>⇧ Enter</kbd> / <kbd>→</kbd> presety</span><span><kbd>Tab</kbd> kategoria</span><span><kbd>Esc</kbd> zamknij</span>
          <span class="grow" /><span>przeciągnij kafelek na kanał miksera</span>
        </div>
      </div>
      <Show when={drag()}>
        {d => (
          <div class="fxp-ghost" style={{ left: d().x + 12 + 'px', top: d().y + 8 + 'px' }}>
            <Show when={d().e.builtin} fallback={<Icon d={ICONS.vst} size={14} />}><FxTile type={d().e.builtin!} size={20} icon={11} /></Show>
            <span>{d().e.name}</span>
            <span class="to">{d().over != null ? '→ ' + (d().over === 0 ? 'Master' : S.project.inserts.find(i => i.id === d().over)?.name ?? '') : 'upuść na kanale'}</span>
          </div>
        )}
      </Show>
    </div>
  );
}
