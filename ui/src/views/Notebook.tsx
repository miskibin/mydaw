// The lyrics notebook ("Notatnik", F8): pages of lyrics where one line is one bar.
//
// A plain <textarea> does the editing (caret, selection, IME, paste, spell-free typing); a
// "mirror" of the same text sits under it and draws what a textarea cannot: headers, rhyme
// marks, the bar being played. The textarea's own glyphs are transparent so only the mirror's
// show. Both use the same font, size and fixed line height and never wrap (long lines scroll
// sideways), so the bar gutter, the highlight and the syllable column stay aligned per line.
import { createEffect, createMemo, createSignal, For, Index, on, onCleanup, onMount, Show, untrack } from 'solid-js';
import type { Id, LyricPage } from '../api/types';
import { clamp } from '../lib/format';
import { barOfBeat, beatOfBar, lastWord, lineBars, lineOfBar, pageBarRange, rhymeGroups, syllables } from '../lib/notebook';
import * as A from '../state/actions';
import {
  addPage, closeNotebook, curPage, deletePage, FONT_MAX, FONT_MIN, flushNotebook, markSaved, NB, pages, pendingText, renamePage, reorderPage,
  restoreBackup, saveGeom, savePrefs, selectPage, setNB, setStartBar, typeText, writeBackup,
} from '../state/notebook';
import { S } from '../state/store';
import { tele } from '../state/telemetry';
import { openMenu, say } from '../state/ui';
import { track } from '../ui/drag';
import { addFrameTask } from '../ui/frameLoop';
import { Icon } from '../ui/icons';
import { moveWindow } from './FxWindow';

export const NOTEBOOK_ICON = 'M6 3h11a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6z M6 3v18 M10 8h5 M10 12h5 M10 16h3';
/** Rhyme colours: mid-saturation hues that read on dark and light surfaces. */
const RHYME = ['#e0a44a', '#4f8ef7', '#d9678f', '#3fb5a3', '#9a8cf0', '#5fbf8a', '#ef7d52', '#5bb8d9'];
const GUTTER = 46, SYLL = 34, PAD_L = 12, PAD_R = 18, PAD_T = 10;
/** Teleprompter: don't fight the user — no auto-scroll this long after typing / scrolling. */
const HANDS_OFF_MS = 2000;

/** The bar the transport is playing (null when stopped / in the count-in). Updated per frame. */
const [liveBar, setLiveBar] = createSignal<number | null>(null);

/** Always mounted: the draft backup and F8 window (rendered only while open). */
export function NotebookHost() {
  // restore unsaved text once per loaded project, then track the saved baseline
  let lastPath: string | null | undefined;
  createEffect(() => {
    const path = S.path, dirty = S.dirty;
    if (path !== lastPath) {
      const first = lastPath === undefined;
      lastPath = path;
      if ((first || path != null) && untrack(restoreBackup)) return;
    }
    if (!dirty) untrack(markSaved);
  });
  onMount(() => {
    const stop = addFrameTask(() => {
      const b = tele.frame.playing ? barOfBeat(tele.frame.beat) : null;
      if (b !== liveBar()) setLiveBar(b);
    });
    // clicking anything in the effect-window layer puts those windows above the notebook
    const down = (e: PointerEvent) => {
      const t = e.target as HTMLElement | null;
      if (!t?.closest) return;
      if (t.closest('.nbwin')) { if (!NB.front) setNB('front', true); }
      else if (t.closest('.winlayer') && NB.front) setNB('front', false);
    };
    const keep = () => writeBackup();
    window.addEventListener('pointerdown', down, true);
    window.addEventListener('beforeunload', keep);
    onCleanup(() => { stop(); window.removeEventListener('pointerdown', down, true); window.removeEventListener('beforeunload', keep); });
  });
  return <Show when={NB.open}><NotebookWindow /></Show>;
}

function NotebookWindow() {
  let ta!: HTMLTextAreaElement, scroller!: HTMLDivElement;
  const G = () => NB.geom;
  const page = () => curPage();
  const LH = () => Math.round(NB.font * 1.7);
  const [text, setText] = createSignal('');
  const [viewH, setViewH] = createSignal(400);
  let lastUser = 0;
  const touched = () => { lastUser = performance.now(); };

  // ------------------------------------------------ text in / out
  const scrollMemo = new Map<Id, number>();
  let shownId: Id | null = null;
  createEffect(() => {
    const pg = page();
    const id = pg?.id ?? null;
    const t = pg ? (pendingText(pg.id) ?? pg.text) : '';
    if (id !== shownId) {
      if (shownId != null) scrollMemo.set(shownId, scroller.scrollTop);
      shownId = id;
      setText(t); ta.value = t;
      queueMicrotask(() => { scroller.scrollTop = scrollMemo.get(id ?? -1) ?? 0; });
      return;
    }
    // changed outside the textarea (undo, CLI, restore): adopt it, keep the caret where it was
    if (pendingText(id ?? -1) == null && ta.value !== t) {
      const a = ta.selectionStart, b = ta.selectionEnd, focused = document.activeElement === ta;
      ta.value = t; setText(t);
      if (focused) ta.setSelectionRange(Math.min(a, t.length), Math.min(b, t.length));
    }
  });
  const onInput = () => {
    const pg = page(); if (!pg) return;
    touched();
    const v = ta.value;
    setText(v);
    typeText(pg.id, v);
    keepCaretVisible();
  };

  // ------------------------------------------------ derived per line
  const lines = createMemo(() => text().split('\n'));
  const bars = createMemo(() => lineBars(text(), page()?.startBar ?? 1));
  const groups = createMemo(() => (NB.rhymes ? rhymeGroups(lines()) : new Map<number, number>()));
  const curLine = createMemo(() => {
    const b = liveBar(), pg = page();
    return b == null || !pg ? -1 : lineOfBar(text(), pg.startBar, b);
  });
  const range = () => { const pg = page(); return pg ? pageBarRange({ text: text(), startBar: pg.startBar }) : [1, 0]; };

  // ------------------------------------------------ teleprompter
  createEffect(on(curLine, ln => {
    if (ln < 0 || performance.now() - lastUser < HANDS_OFF_MS) return;
    const top = PAD_T + ln * LH() - (scroller.clientHeight - LH()) / 2;
    // reading, not typing: line starts in view
    scroller.scrollTo({ top: Math.max(0, top), ...(document.activeElement === ta ? {} : { left: 0 }), behavior: 'smooth' });
  }));

  // ------------------------------------------------ caret follow (the textarea never scrolls itself)
  let measure: CanvasRenderingContext2D | null = null;
  function keepCaretVisible() {
    const pos = ta.selectionEnd, v = ta.value;
    const ln = v.slice(0, pos).split('\n').length - 1;
    const top = PAD_T + ln * LH(), bot = top + LH();
    if (top < scroller.scrollTop + LH() * 0.5) scroller.scrollTop = Math.max(0, top - LH());
    else if (bot > scroller.scrollTop + scroller.clientHeight - LH() * 0.5) scroller.scrollTop = bot - scroller.clientHeight + LH();
    measure ??= document.createElement('canvas').getContext('2d');
    if (!measure) return;
    measure.font = getComputedStyle(ta).font;
    const lineStart = v.lastIndexOf('\n', pos - 1) + 1;
    const x = GUTTER + PAD_L + measure.measureText(v.slice(lineStart, pos)).width;
    const left = scroller.scrollLeft + GUTTER + PAD_L, right = scroller.scrollLeft + scroller.clientWidth - SYLL - PAD_R;
    if (x < left) scroller.scrollLeft = Math.max(0, x - GUTTER - PAD_L - 40);
    else if (x > right) scroller.scrollLeft = x - scroller.clientWidth + SYLL + PAD_R + 40;
  }

  // ------------------------------------------------ keys, wheel
  const onKey = (e: KeyboardEvent) => {
    // AltGr (= Ctrl+Alt on Windows) types Polish letters: never a shortcut
    const m = (e.ctrlKey || e.metaKey) && !e.altKey, k = e.key.toLowerCase();
    touched();
    if (e.key === 'Escape') { e.preventDefault(); flushNotebook(); ta.blur(); scroller.scrollTo({ left: 0, behavior: 'smooth' }); return; }
    if (e.key === 'F8') { e.preventDefault(); if (!e.altKey) closeNotebook(); return; }
    // the project's undo, not the textarea's: the pending text first becomes its own step
    if (m && (k === 'z' || k === 'y')) { e.preventDefault(); flushNotebook(); void (k === 'y' || e.shiftKey ? A.redo() : A.undo()); return; }
    if (m && k === 's') { e.preventDefault(); flushNotebook(); void A.projectSave(e.shiftKey); return; }
    queueMicrotask(keepCaretVisible);
  };
  const onWheel = (e: WheelEvent) => {
    touched();
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    const f = clamp(NB.font + (e.deltaY < 0 ? 1 : -1), FONT_MIN, FONT_MAX);
    if (f !== NB.font) { setNB('font', f); savePrefs(); say('Notatnik: czcionka ' + f + ' px'); }
  };

  // ------------------------------------------------ window
  onMount(() => {
    const ro = new ResizeObserver(() => setViewH(scroller.clientHeight));
    ro.observe(scroller);
    onCleanup(() => ro.disconnect());
    onCleanup(flushNotebook);
  });
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, sy = e.clientY, o = { ...G() };
    track(ev => setNB('geom', { w: clamp(o.w + ev.clientX - sx, 300, window.innerWidth - 20), h: clamp(o.h + ev.clientY - sy, 240, window.innerHeight - 20) }), () => saveGeom(), 'nwse-resize');
  };
  const move = (e: PointerEvent) => moveWindow(e, () => G(), (x, y) => { setNB('geom', { x, y }); saveGeom(); });
  const goBar = (bar: number) => { A.seek(beatOfBar(bar)); say('Takt ' + bar); };

  return (
    <div class="win nbwin" style={{ left: G().x + 'px', top: G().y + 'px', width: G().w + 'px', height: G().h + 'px', 'z-index': NB.front ? 46 : 44, '--nb-font': NB.font + 'px', '--nb-lh': LH() + 'px' }}
      data-notebook>
      <div class="winhd" onPointerDown={move}>
        <span class="tile" style={{ width: '20px', height: '20px', background: 'color-mix(in srgb, var(--blue) 18%, transparent)', color: 'var(--blue)' }}><Icon d={NOTEBOOK_ICON} size={12} /></span>
        <span class="title">Notatnik</span>
        <Show when={page()}>
          <span class="sub">{range()[1] >= range()[0] ? `takty ${range()[0]}–${range()[1]}` : 'brak taktów'}</span>
        </Show>
        <div style={{ flex: 1 }} />
        <Show when={pages().length}><button class="nb-tog" classList={{ on: NB.rhymes }} title="Rymy: zakończenia wersów, które się rymują, mają wspólny kolor" onClick={() => { setNB('rhymes', !NB.rhymes); savePrefs(); }}>Rymy</button></Show>
        <button class="closebtn" title="Zamknij (F8)" onClick={closeNotebook}>×</button>
      </div>
      <Show when={pages().length}><Tabs /></Show>
      <Show when={page()} fallback={<Empty />}>
        <div class="nb-bar">
          <label class="nb-start" title="Numer taktu pierwszej linii tej strony (jak na linijce playlisty)">
            od taktu
            <input class="textin" type="number" min="1" max="9999" value={page()!.startBar}
              onChange={e => { const v = parseInt(e.currentTarget.value, 10); if (isFinite(v)) setStartBar(page()!.id, v); else e.currentTarget.value = String(page()!.startBar); }}
              onKeyDown={e => { if (e.key === 'Enter' || e.key === 'Escape') e.currentTarget.blur(); }} />
          </label>
          <span class="nb-hint">1 linia = 1 takt · [Nagłówek] nie liczy się</span>
        </div>
      </Show>
      <div ref={scroller} class="nb-scroll" classList={{ hidden: !page() }} onWheel={onWheel} onPointerDown={touched}>
        <div class="nb-grid" style={{ 'padding-bottom': Math.max(24, viewH() / 2) + 'px', 'padding-top': PAD_T + 'px' }}>
          <div class="nb-gutter">
            <Index each={bars()}>{(b, i) => (
              <div class="nb-row" classList={{ cur: i === curLine(), hdr: b() == null }}>
                <Show when={b() != null}>
                  <button class="nb-num" tabIndex={-1} title={'Głowica na początek taktu ' + b()} onClick={() => goBar(b()!)}>{b()}</button>
                </Show>
              </div>
            )}</Index>
          </div>
          <div class="nb-body">
            <div class="nb-mirror" aria-hidden="true">
              <Index each={lines()}>{(l, i) => <MirrorLine text={l()} hdr={bars()[i] == null} cur={i === curLine()} group={groups().get(i)} />}</Index>
            </div>
            <textarea ref={ta} class="nb-ta" wrap="off" maxLength={50000} spellcheck={false} autocapitalize="off" autocomplete="off"
              placeholder={'[Zwrotka]\nPierwszy takt…'}
              onInput={onInput} onKeyDown={onKey} onBlur={flushNotebook} onPointerUp={keepCaretVisible}
              onScroll={() => { if (ta.scrollTop || ta.scrollLeft) { ta.scrollTop = 0; ta.scrollLeft = 0; } }} />
          </div>
          <div class="nb-syll">
            <Index each={lines()}>{(l, i) => {
              const n = () => (bars()[i] == null ? 0 : syllables(l()));
              return <div class="nb-row" classList={{ cur: i === curLine(), hdr: bars()[i] == null }}><Show when={n() > 0}>{n()}</Show></div>;
            }}</Index>
          </div>
        </div>
      </div>
      <div class="resizer" onPointerDown={resize} title="Zmień rozmiar" />
    </div>
  );
}

function MirrorLine(props: { text: string; hdr: boolean; cur: boolean; group: number | undefined }) {
  const parts = () => {
    const t = props.text;
    if (props.group == null) return null;
    const w = lastWord(t); if (!w) return null;
    return [t.slice(0, w.from), t.slice(w.from, w.to), t.slice(w.to)] as const;
  };
  return (
    <div class="nb-row" classList={{ cur: props.cur, hdr: props.hdr }}>
      <Show when={parts()} fallback={props.text || '​'}>
        {p => <>{p()[0]}<span class="nb-rhyme" style={{ '--rc': RHYME[props.group! % RHYME.length] }}>{p()[1]}</span>{p()[2]}</>}
      </Show>
    </div>
  );
}

function Empty() {
  return (
    <div class="nb-empty">
      <div class="nb-empty-t">Tekst utworu, takt po takcie</div>
      <div class="hint">Każda linia to jeden takt — numer obok pokazuje, gdzie wypada na playliście. Pusta linia to takt przerwy, a linia w nawiasach, np. [Refren], to nagłówek. Podczas odtwarzania bieżący takt się podświetla.</div>
      <button class="smallbtn" onClick={() => addPage()}>+ Nowa strona</button>
    </div>
  );
}

function Tabs() {
  const [editing, setEditing] = createSignal<Id | null>(null);
  const [drop, setDrop] = createSignal<number | null>(null);
  let strip!: HTMLDivElement;
  const live = (pg: LyricPage) => {
    const b = liveBar(); if (b == null) return false;
    const [a, z] = pageBarRange(pg);
    return b >= a && b <= z;
  };
  const menu = (e: MouseEvent, pg: LyricPage) => {
    e.preventDefault(); e.stopPropagation();
    selectPage(pg.id);
    openMenu(e.clientX, e.clientY, [
      { t: 'title', label: pg.name },
      { t: 'item', label: 'Zmień nazwę', kbd: '2× klik', act: () => setEditing(pg.id) },
      { t: 'item', label: 'Nowa strona', act: () => addPage() },
      { t: 'sep' },
      { t: 'item', label: 'Usuń stronę', danger: true, act: () => deletePage(pg.id) },
    ], 140);
  };
  /** Press on a tab: click selects, a drag sideways reorders. */
  const down = (e: PointerEvent, pg: LyricPage) => {
    if (e.button !== 0 || editing() === pg.id) return;
    selectPage(pg.id);
    const sx = e.clientX;
    let dragging = false;
    const slot = (x: number) => {
      const tabs = [...strip.querySelectorAll<HTMLElement>('.nb-tab')];
      let i = tabs.length;
      for (let k = 0; k < tabs.length; k++) { const r = tabs[k].getBoundingClientRect(); if (x < r.left + r.width / 2) { i = k; break; } }
      return i;
    };
    track(ev => {
      if (!dragging && Math.abs(ev.clientX - sx) < 5) return;
      dragging = true;
      setDrop(slot(ev.clientX));
    }, ev => {
      const to = drop(); setDrop(null);
      if (!dragging || to == null) return;
      const from = pages().findIndex(p => p.id === pg.id);
      reorderPage(pg.id, to > from ? to - 1 : to);
      void ev;
    }, undefined);
  };
  return (
    <div class="nb-tabs" ref={strip}>
      <For each={pages()}>{(pg, i) => (
        <div class="nb-tab" classList={{ on: pg.id === curPage()?.id, dropL: drop() === i(), dropR: drop() === pages().length && i() === pages().length - 1 }}
          onPointerDown={e => down(e, pg)} onDblClick={() => setEditing(pg.id)} onContextMenu={e => menu(e, pg)}
          title={pg.name + ' — od taktu ' + pg.startBar + ' · 2× klik = zmień nazwę · przeciągnij = kolejność · PPM = menu'}>
          <Show when={live(pg)}><span class="nb-live" /></Show>
          <Show when={editing() === pg.id} fallback={<span class="nb-tabname">{pg.name}</span>}>
            <input class="nb-rename" value={pg.name} maxLength={80} ref={el => queueMicrotask(() => { el.focus(); el.select(); })}
              onPointerDown={e => e.stopPropagation()}
              onKeyDown={e => { if (e.key === 'Enter') e.currentTarget.blur(); if (e.key === 'Escape') { e.currentTarget.value = pg.name; e.currentTarget.blur(); } }}
              onBlur={e => { renamePage(pg.id, e.currentTarget.value); setEditing(null); }} />
          </Show>
          <Show when={pg.id === curPage()?.id && editing() !== pg.id}>
            <button class="nb-tabx" title="Usuń stronę (Ctrl+Z przywraca)" onPointerDown={e => e.stopPropagation()} onClick={() => deletePage(pg.id)}>×</button>
          </Show>
        </div>
      )}</For>
      <button class="nb-add" title="Nowa strona" onClick={() => addPage()}><Icon d="M12 5v14 M5 12h14" size={12} /></button>
    </div>
  );
}
