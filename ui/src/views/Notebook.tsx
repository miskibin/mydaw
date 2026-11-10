import { t as tl } from '../lib/i18n';
// The lyrics notebook ("Notatnik", F8): pages of lyrics where one line is one bar.
//
// A plain <textarea> does the editing (caret, selection, IME, paste, spell-free typing); a
// "mirror" of the same text sits under it and draws what a textarea cannot: headers, rhyme
// marks, the bar being played. The textarea's own glyphs are transparent so only the mirror's
// show. Both use the same font and line height. Without wrap long lines scroll sideways; with
// wrap (Alt+Z) both wrap identically and the gutter / syllable rows take each line's measured height.
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
  let ta!: HTMLTextAreaElement, scroller!: HTMLDivElement, mirror!: HTMLDivElement;
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
  // wrap: measured height of every mirror line (gutter and syllable rows copy it)
  const [rowH, setRowH] = createSignal<number[] | null>(null);
  const measureRows = () => {
    if (!NB.wrap || !mirror) { if (rowH()) setRowH(null); return; }
    setRowH(Array.from(mirror.children, c => (c as HTMLElement).offsetHeight));
  };
  createEffect(() => { lines(); void NB.wrap; void NB.font; void G().w; queueMicrotask(measureRows); });
  const lineTop = (ln: number) => {
    const el = NB.wrap ? mirror?.children[ln] as HTMLElement | undefined : undefined;
    return PAD_T + (el ? el.offsetTop : ln * LH());
  };
  const lineHt = (ln: number) => rowH()?.[ln] ?? LH();
  const rowStyle = (i: number) => (rowH() ? { height: (rowH()![i] ?? LH()) + 'px' } : {});
  const toggleWrap = () => { setNB('wrap', !NB.wrap); savePrefs(); say(NB.wrap ? tl('a.nb.wrapOn') : tl('a.nb.wrapOff')); queueMicrotask(keepCaretVisible); };
  const range = () => { const pg = page(); return pg ? pageBarRange({ text: text(), startBar: pg.startBar }) : [1, 0]; };

  // ------------------------------------------------ teleprompter
  createEffect(on(curLine, ln => {
    if (ln < 0 || performance.now() - lastUser < HANDS_OFF_MS) return;
    const top = lineTop(ln) - (scroller.clientHeight - lineHt(ln)) / 2;
    // reading, not typing: line starts in view
    scroller.scrollTo({ top: Math.max(0, top), ...(document.activeElement === ta ? {} : { left: 0 }), behavior: 'smooth' });
  }));

  // ------------------------------------------------ caret follow (the textarea never scrolls itself)
  let measure: CanvasRenderingContext2D | null = null;
  function keepCaretVisible() {
    const pos = ta.selectionEnd, v = ta.value;
    const ln = v.slice(0, pos).split('\n').length - 1;
    const top = lineTop(ln), bot = top + lineHt(ln);
    if (top < scroller.scrollTop + LH() * 0.5) scroller.scrollTop = Math.max(0, top - LH());
    else if (bot > scroller.scrollTop + scroller.clientHeight - LH() * 0.5) scroller.scrollTop = bot - scroller.clientHeight + LH();
    if (NB.wrap) return; // no sideways scrolling when lines wrap
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
    if (e.altKey && !e.ctrlKey && k === 'z') { e.preventDefault(); toggleWrap(); return; }
    queueMicrotask(keepCaretVisible);
  };
  const onWheel = (e: WheelEvent) => {
    touched();
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    const f = clamp(NB.font + (e.deltaY < 0 ? 1 : -1), FONT_MIN, FONT_MAX);
    if (f !== NB.font) { setNB('font', f); savePrefs(); say(tl('a.nb.font', { px: f })); }
  };

  // ------------------------------------------------ window
  onMount(() => {
    const ro = new ResizeObserver(() => { setViewH(scroller.clientHeight); measureRows(); });
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
  const goBar = (bar: number) => { A.seek(beatOfBar(bar)); say(tl('a.nb.bar', { n: bar })); };

  return (
    <div class="win nbwin" classList={{ wrap: NB.wrap }} style={{ left: G().x + 'px', top: G().y + 'px', width: G().w + 'px', height: G().h + 'px', 'z-index': NB.front ? 46 : 44, '--nb-font': NB.font + 'px', '--nb-lh': LH() + 'px' }}
      data-notebook>
      <div class="winhd" onPointerDown={move}>
        <span class="tile" style={{ width: '20px', height: '20px', background: 'color-mix(in srgb, var(--blue) 18%, transparent)', color: 'var(--blue)' }}><Icon d={NOTEBOOK_ICON} size={12} /></span>
        <span class="title">{tl('a.nb.title')}</span>
        <Show when={page()}>
          <span class="sub">{range()[1] >= range()[0] ? tl('a.nb.bars', { a: range()[0], b: range()[1] }) : tl('a.nb.noBars')}</span>
        </Show>
        <div style={{ flex: 1 }} />
        <Show when={pages().length}><button class="nb-tog" classList={{ on: NB.rhymes }} title={tl('a.nb.rhymesTip')} onClick={() => { setNB('rhymes', !NB.rhymes); savePrefs(); }}>{tl('a.nb.rhymes')}</button></Show>
        <Show when={pages().length}><button class="nb-tog" classList={{ on: NB.wrap }} title={tl('a.nb.wrapTip')} onClick={toggleWrap}>{tl('a.nb.wrap')}</button></Show>
        <button class="closebtn" title={tl('a.nb.close')} onClick={closeNotebook}>×</button>
      </div>
      <div class="nb-top">
      <Show when={pages().length}><Tabs /></Show>
      <Show when={page()}>
          <label class="nb-start" title={tl('a.nb.startTip')}>
            {tl('a.nb.fromBar')}
            <input class="textin" type="number" min="1" max="9999" value={page()!.startBar}
              onChange={e => { const v = parseInt(e.currentTarget.value, 10); if (isFinite(v)) setStartBar(page()!.id, v); else e.currentTarget.value = String(page()!.startBar); }}
              onKeyDown={e => { if (e.key === 'Enter' || e.key === 'Escape') e.currentTarget.blur(); }} />
          </label>
      </Show>
      </div>
      <Show when={!page()}><Empty /></Show>
      <div ref={scroller} class="nb-scroll" classList={{ hidden: !page() }} onWheel={onWheel} onPointerDown={touched}>
        <div class="nb-grid" style={{ 'padding-bottom': Math.max(24, viewH() / 2) + 'px', 'padding-top': PAD_T + 'px' }}>
          <div class="nb-gutter">
            <Index each={bars()}>{(b, i) => (
              <div class="nb-row" classList={{ cur: i === curLine(), hdr: b() == null }} style={rowStyle(i)}>
                <Show when={b() != null}>
                  <button class="nb-num" tabIndex={-1} title={tl('a.nb.playheadTo', { n: b()! })} onClick={() => goBar(b()!)}>{b()}</button>
                </Show>
              </div>
            )}</Index>
          </div>
          <div class="nb-body">
            <div ref={mirror} class="nb-mirror" aria-hidden="true">
              <Index each={lines()}>{(l, i) => <MirrorLine text={l()} hdr={bars()[i] == null} cur={i === curLine()} group={groups().get(i)} />}</Index>
            </div>
            <textarea ref={ta} class="nb-ta" wrap={NB.wrap ? 'soft' : 'off'} maxLength={50000} spellcheck={false} autocapitalize="off" autocomplete="off"
              placeholder={tl('a.nb.placeholder')}
              onInput={onInput} onKeyDown={onKey} onBlur={flushNotebook} onPointerUp={keepCaretVisible}
              onScroll={() => { if (ta.scrollTop || ta.scrollLeft) { ta.scrollTop = 0; ta.scrollLeft = 0; } }} />
          </div>
          <div class="nb-syll">
            <Index each={lines()}>{(l, i) => {
              const n = () => (bars()[i] == null ? 0 : syllables(l()));
              return <div class="nb-row" classList={{ cur: i === curLine(), hdr: bars()[i] == null }} style={rowStyle(i)}><Show when={n() > 0}>{n()}</Show></div>;
            }}</Index>
          </div>
        </div>
      </div>
      <div class="resizer" onPointerDown={resize} title={tl('a.nb.resize')} />
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
      <div class="nb-empty-t">{tl('a.nb.emptyTitle')}</div>
      <div class="hint">{tl('a.nb.emptyHint')}</div>
      <button class="smallbtn" onClick={() => addPage()}>{tl('a.nb.addPage')}</button>
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
      { t: 'item', label: tl('a.nb.rename'), kbd: tl('a.nb.dbl'), act: () => setEditing(pg.id) },
      { t: 'item', label: tl('a.nb.newPage'), act: () => addPage() },
      { t: 'sep' },
      { t: 'item', label: tl('a.nb.deletePage'), danger: true, act: () => deletePage(pg.id) },
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
          title={tl('a.nb.tabTip', { name: pg.name, bar: pg.startBar })}>
          <Show when={live(pg)}><span class="nb-live" /></Show>
          <Show when={editing() === pg.id} fallback={<span class="nb-tabname">{pg.name}</span>}>
            <input class="nb-rename" value={pg.name} maxLength={80} ref={el => queueMicrotask(() => { el.focus(); el.select(); })}
              onPointerDown={e => e.stopPropagation()}
              onKeyDown={e => { if (e.key === 'Enter') e.currentTarget.blur(); if (e.key === 'Escape') { e.currentTarget.value = pg.name; e.currentTarget.blur(); } }}
              onBlur={e => { renamePage(pg.id, e.currentTarget.value); setEditing(null); }} />
          </Show>
          <Show when={pg.id === curPage()?.id && editing() !== pg.id}>
            <button class="nb-tabx" title={tl('a.nb.deletePageTip')} onPointerDown={e => e.stopPropagation()} onClick={() => deletePage(pg.id)}>×</button>
          </Show>
        </div>
      )}</For>
      <button class="nb-add" title={tl('a.nb.newPage')} onClick={() => addPage()}><Icon d="M12 5v14 M5 12h14" size={12} /></button>
    </div>
  );
}
