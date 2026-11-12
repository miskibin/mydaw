import { curPat, newPattern, openRack, patMode, setPatMode, stepPattern, toggleRack, toggleRoll } from '../state/pattern';
import { patternMenu } from './patternShared';
import { t, type TranslationKey } from '../lib/i18n';
import { setTypingOn, typingBase, typingOn } from '../state/typingKeys';
import { noteName } from '../lib/pitch';
import { prefs } from '../state/prefs';
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { baseName, clamp, fmtClock, fmtPos, TapTracker } from '../lib/format';
import { SNAPS, snapLabel } from '../lib/snap';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { S } from '../state/store';
import { curBeat, tele } from '../state/telemetry';
import { closeMenu, openMenu, say, setU, U, type MenuItem, type Tool } from '../state/ui';
import { checkUpdates, showWhatsNew } from '../state/updates';
import { midiFlash, midiStatus } from '../state/midi';
import { track } from '../ui/drag';
import { addFrameTask } from '../ui/frameLoop';
import { Icon, ICONS } from '../ui/icons';
import { UpdateChip } from './Updates';

export const TOOLS: [Tool, string, TranslationKey][] = [
  ['draw', 'P', 'a.tb.toolDraw'], ['paint', 'B', 'a.tb.toolPaint'], ['erase', 'D', 'a.tb.toolErase'], ['mute', 'T', 'a.tb.toolMute'], ['slice', 'C', 'a.tb.toolSlice'], ['select', 'E', 'a.tb.toolSelect'],
];
export const TOOL_HINT: Record<Tool, TranslationKey> = { draw: 'a.tb.hint_draw', paint: 'a.tb.hint_paint', erase: 'a.tb.hint_erase', mute: 'a.tb.hint_mute', slice: 'a.tb.hint_slice', select: 'a.tb.hint_select' };

const clampBpm = (v: number) => clamp(Math.round(v * 100) / 100, 20, 999);

export function Toolbar() {
  let clockEl!: HTMLSpanElement, inEl!: HTMLDivElement;
  let lastClock = '', inHold = 0;
  onMount(() => {
    const stop = addFrameTask(() => {
      const s = fmtClock(curBeat());
      if (s !== lastClock) { lastClock = s; clockEl.textContent = s; }
      if (inEl) {
        inHold = Math.max(tele.frame.inLevel, inHold * 0.9);
        inEl.style.transform = `scaleX(${Math.min(1, Math.sqrt(inHold))})`;
        inEl.style.background = inHold > 0.89 ? 'var(--red)' : inHold > 0.5 ? 'var(--amber)' : 'var(--green)';
      }
    });
    onCleanup(stop);
  });
  const bpmDown = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault();
    const g = new Gesture('Tempo');
    let v = S.project.bpm, lx = e.clientX, ly = e.clientY;
    track(ev => {
      const d = (ev.clientX - lx) + (ly - ev.clientY); lx = ev.clientX; ly = ev.clientY;
      v = clamp(v + d * (ev.shiftKey ? 0.05 : 0.5), 20, 999);
      const b = clampBpm(ev.shiftKey ? v : Math.round(v));
      if (b !== S.project.bpm) g.update([{ op: 'setBpm', bpm: b }]);
      say('Tempo ' + b.toFixed(2) + ' BPM');
    }, () => g.end(), 'ns-resize');
  };
  const bpmWheel = (e: WheelEvent) => {
    const d = e.deltaY || e.deltaX; if (!d) return;
    e.preventDefault();
    const b = clampBpm(S.project.bpm + (d < 0 ? 1 : -1) * (e.shiftKey ? 0.01 : 1));
    wheelEdit('bpm', 'Tempo', [{ op: 'setBpm', bpm: b }]);
    say('Tempo ' + b.toFixed(2) + ' BPM');
  };
  const tapper = new TapTracker();
  const [tapping, setTapping] = createSignal(false);
  let tapTimer = 0;
  onCleanup(() => clearTimeout(tapTimer));
  const doTap = (e?: MouseEvent) => {
    const r = tapper.tap(performance.now(), !e?.shiftKey);
    setTapping(true);
    clearTimeout(tapTimer);
    tapTimer = window.setTimeout(() => setTapping(false), 2000);
    if (r.bpm != null) {
      wheelEdit('tap-bpm', 'Tap tempo', [{ op: 'setBpm', bpm: r.bpm }]);
      say(`Tap tempo · ${r.bpm.toFixed(2)} BPM (${r.count}×)`);
    } else {
      say(t('a.tb.tapKeep'));
    }
  };
  const bpmMenu = (e: MouseEvent) => {
    e.preventDefault(); e.stopPropagation();
    const b = S.project.bpm;
    const setB = (nb: number, label = 'Tempo') => {
      const c = clampBpm(nb);
      commit([{ op: 'setBpm', bpm: c }], label);
      say('Tempo ' + c.toFixed(2) + ' BPM');
    };
    const hasAudio = S.project.clips.some(c => c.kind === 'audio');
    openMenu(e.clientX, e.clientY, [
      { t: 'title', label: 'Tempo · ' + b.toFixed(2) + ' BPM' },
      { t: 'item', label: t('a.tb.tapMenu'), kbd: t('a.tb.tapKbd'), act: () => doTap() },
      { t: 'item', label: t('a.tb.detectProj'), disabled: !hasAudio, act: () => A.openMusicAnalysis({}) },
      { t: 'sep' },
      { t: 'item', label: t('a.tb.halfTempo', { bpm: clampBpm(b / 2).toFixed(1) }), disabled: b / 2 < 20, act: () => setB(b / 2, t('a.tb.halfLabel')) },
      { t: 'item', label: t('a.tb.doubleTempo', { bpm: clampBpm(b * 2).toFixed(1) }), disabled: b * 2 > 999, act: () => setB(b * 2, t('a.tb.doubleLabel')) },
      { t: 'sep' },
      { t: 'chips', label: t('a.tb.quickPick'), options: ['80', '90', '100', '110', '120', '128', '140', '160'], active: [80, 90, 100, 110, 120, 128, 140, 160].indexOf(Math.round(b)), pick: i => setB([80, 90, 100, 110, 120, 128, 140, 160][i]) },
    ], 260);
  };
  const T = () => S.transport;
  const projName = () => (S.path ? baseName(S.path) : t('a.tb.untitled'));
  // custom title bar: empty toolbar areas and the project name drag the window, 2× click maximises
  const win = be().win;
  const [maxed, setMaxed] = createSignal(false);
  onMount(() => {
    if (!win) return;
    const upd = () => win.isMaximized().then(setMaxed).catch(() => {});
    upd();
    let off: (() => void) | null = null;
    win.onResized(upd).then(u => { off = u; });
    onCleanup(() => off?.());
  });
  let lastDown = 0;
  const dragDown = (e: MouseEvent) => {
    if (!win || e.button !== 0) return;
    const el = e.target as HTMLElement;
    // Drag only from the bar itself or an element marked data-drag, never from a
    // child of one (.tbmain holds every button: closest() swallowed their clicks).
    if (el !== e.currentTarget && !el.hasAttribute('data-drag')) return;
    const now = performance.now();
    if (now - lastDown < 350) { lastDown = 0; void win.toggleMaximize(); return; }
    lastDown = now;
    void win.startDragging();
  };
  const wins = () => U.plugs.length + (U.rackWin ? 1 : 0) + (U.rollWin ? 1 : 0);
  const hint = () => t('a.tb.statusHint', { tool: t(TOOLS.find(x => x[0] === U.tool)![2]), hint: t(TOOL_HINT[U.tool]), snap: U.alt ? 'off' : snapLabel(U.snap, U.bw).replace('Snap: ', '') });
  return (
    <div class="toolbar" classList={{ native: !!win }} onMouseDown={dragDown}>
      {/* left: the menus over the hint line (FL): what the last action did, and what the tool does */}
      <div class="tbleft" data-drag>
        <div class="tbmenu" data-drag>
          <MenuBar />
          <UpdateChip />
          <span data-drag class="projName" title={(S.path ?? t('a.tb.unsavedProj')) + (S.dirty ? t('a.tb.unsavedChanges') : '')}>{projName()}<Show when={S.dirty}><span data-drag class="dirty">•</span></Show></span>
        </div>
        <div class="tbhint" data-drag>
          <span class="hled" style={{ background: S.transport.rec && U.playing ? 'var(--red)' : U.playing ? 'var(--green)' : 'var(--fg6)' }} />
          <span data-drag class="readout" data-readout>{U.readout}</span>
          <Show when={U.alt}><span class="offgrid">OFF GRID</span></Show>
          <Show when={prefs().hints}><span data-drag class="hinttxt">{hint()}</span></Show>
          <div class="row inrow" title={t('a.tb.inLevel')} style={{ gap: '6px', display: T().rec || T().monitor ? 'flex' : 'none' }}>
            <span class="tag9">IN</span>
            <div class="inmeter"><div ref={inEl} /></div>
          </div>
        </div>
      </div>
      <div class="tbmid" data-drag>
      <div class="seg">
        <button title={patMode() ? t('a.tb.playPat') : t('a.tb.playSong')} data-act="play" classList={{ play: U.playing || U.patPlaying }} onClick={() => void A.togglePlay()}>
          <svg width="11" height="11" viewBox="0 0 14 14"><path d="M3 1.5v11l9-5.5z" fill="currentColor" /></svg>
        </button>
        <button title={t('a.tb.stopTip')} onClick={() => void A.stop(!(U.playing || U.patPlaying))}><svg width="10" height="10" viewBox="0 0 12 12"><rect x="1" y="1" width="10" height="10" rx="1" fill="currentColor" /></svg></button>
        <button title={t('a.tb.recTip')} class="rec" classList={{ armed: T().rec }} onClick={A.toggleRec}>
          <svg width="10" height="10" viewBox="0 0 12 12"><circle cx="6" cy="6" r="5" fill="currentColor" /></svg>
        </button>
        <button title={t('a.tb.monTip')} class="mon monbtn" classList={{ on: T().monitor }} onClick={A.toggleMonitor}>
          <Icon d="M4 15v-3a8 8 0 0 1 16 0v3 M3 14h4v7H3z M17 14h4v7h-4z" size={13} />
        </button>
        <button title={t('a.tb.metroTip')} class="metrobtn" classList={{ on: T().metro, dim: !T().metro }} onClick={() => void A.setTransportFlag('metro', !T().metro)}>
          <Icon d="M9 3h6l4 18H5z M12 16l6-9" size={13} />
        </button>
        <button class="count" classList={{ on: T().countIn }} title={t('a.tb.countTip')} onClick={() => void A.setTransportFlag('countIn', !T().countIn)}>1 BAR</button>
      </div>
      <div class="seg patsong" title={t('a.tb.patSongTip')}>
        <button class="pat" classList={{ on: patMode() }} onClick={() => setPatMode(true)}>PAT</button>
        <button class="song" classList={{ on: !patMode() }} onClick={() => setPatMode(false)}>SONG</button>
      </div>
      <PatSelector />
      <div class="clockbox" onContextMenu={bpmMenu}>
        <span ref={clockEl} class="clock">1:01:01</span>
        <div style={{ width: '1px', height: '14px', background: 'var(--line)' }} />
        <span class="bpm" title={t('a.tb.bpmTip')} onPointerDown={bpmDown} onWheel={bpmWheel}>{S.project.bpm.toFixed(2)}</span>
        <span class="tag9">BPM</span>
        <button class="tapbtn" classList={{ on: tapping() }} data-tap title={t('a.tb.tapTip')} onClick={e => doTap(e)}>TAP</button>
      </div>
      </div>
      <div class="tbright" data-drag>
      <span data-drag class="loopText" style={{ color: S.project.loop ? 'var(--green)' : 'var(--fg5)' }}>{S.project.loop ? 'LOOP ' + fmtPos(S.project.loop.a) + ' – ' + fmtPos(S.project.loop.b) : ''}</span>
      <div data-drag class="dragfill" />
      <button class="ibtn typkb" classList={{ on: typingOn() }} title={typingOn() ? t('a.tb.typOn', { z: noteName(typingBase()), q: noteName(typingBase() + 12) }) : t('a.tb.typOff')}
        onClick={() => setTypingOn(!typingOn())}>
        <Icon d="M3 6h18v12H3z M7 6v7 M11 6v7 M15 6v7 M19 6v7 M3 13h18" size={14} />
        <Show when={typingOn()}><span class="mono typoct">{noteName(typingBase())}</span></Show>
      </button>
      <div class="seg tools">
        <For each={TOOLS}>
          {([id, key, title]) => (
            <button classList={{ on: U.tool === id }} title={t(title) + ' (' + key + ') — ' + t(TOOL_HINT[id])} onClick={() => setU('tool', id)}>
              <Icon d={ICONS[id]} sw={1.9} />
            </button>
          )}
        </For>
      </div>
      <select class="sel snapsel" title={t('a.tb.snapTip')} value={String(U.snap)} onChange={e => A.setSnap(parseFloat(e.currentTarget.value))} data-snap>
        <For each={SNAPS}>{([label, v]) => <option value={String(v)}>{v < 0 ? snapLabel(-1, U.bw) : v === 0 ? t('a.snap.off') : label}</option>}</For>
      </select>
      <Show when={wins()}>
        <button class="tbtn" classList={{ on: !U.plugsHidden }} onClick={() => (U.plugsHidden ? A.showPlugs() : A.playlistFront())}
          title={U.plugsHidden ? t('a.tb.showWins', { n: wins() }) : t('a.tb.hideWins')}>
          <Icon d="M4 7h12v12H4z M8 3h12v12" size={13} /><span class="lbl">{t('a.tb.windows', { n: wins() })}</span>
        </button>
      </Show>
      <button class="tbtn" classList={{ on: U.mixOpen }} onClick={() => setU('mixOpen', !U.mixOpen)} title={t('a.tb.mixerTip')} aria-label="Mixer">
        <Icon d="M5 21v-7M5 10V3M12 21v-9M12 8V3M19 21v-5M19 12V3M2 14h6M9 8h6M16 16h6" size={13} /><span class="lbl">Mixer</span>
      </button>
      <Show when={midiStatus()?.selected}>
        <button class="ibtn midibtn" onClick={() => setU('settings', true)}
          title={midiStatus()?.connected ? t('a.tb.midiOn', { name: midiStatus()!.connected!.name }) : t('a.tb.midiOff')}>
          <span class="midiled" classList={{ on: !!midiStatus()?.connected, hit: midiFlash() }} /><span class="tag9">MIDI</span>
        </button>
      </Show>
      <Show when={win}>
        <div class="winbtns">
          <button title={t('a.tb.minimize')} aria-label={t('a.tb.minimize')} onClick={() => void win!.minimize()}><svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 5h10" stroke="currentColor" /></svg></button>
          <button title={maxed() ? t('a.tb.restore') : t('a.tb.maximize')} aria-label={maxed() ? t('a.tb.restore') : t('a.tb.maximize')} onClick={() => void win!.toggleMaximize()}>
            <Show when={maxed()} fallback={<svg width="10" height="10" viewBox="0 0 10 10"><rect x=".5" y=".5" width="9" height="9" fill="none" stroke="currentColor" /></svg>}>
              <svg width="10" height="10" viewBox="0 0 10 10"><path d="M2.5 2.5V.5h7v7h-2 M.5 2.5h7v7h-7z" fill="none" stroke="currentColor" /></svg>
            </Show>
          </button>
          <button class="close" title={t('a.tb.close')} aria-label={t('a.tb.close')} onClick={() => void win!.close()}><svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1.1" /></svg></button>
        </div>
      </Show>
      </div>
    </div>
  );
}

/**
 * The application menus. A press opens one under its title; while one is open, moving over
 * another title switches to it (the bar is lifted above the menu backdrop for that).
 */
function MenuBar() {
  const [open, setOpen] = createSignal(-1);
  createEffect(() => { if (!U.menu) setOpen(-1); });
  const win = be().win;
  const none = () => !U.sel.length;
  const wins = () => U.plugs.length + (U.rackWin ? 1 : 0) + (U.rollWin ? 1 : 0);
  const menus: [TranslationKey, () => MenuItem[]][] = [
    ['a.mn.file', () => [
      { t: 'item', label: t('a.mn.new'), kbd: 'Ctrl+N', act: () => void A.projectNew() },
      { t: 'item', label: t('a.mn.open'), kbd: 'Ctrl+O', act: () => void A.projectOpen() },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.save'), kbd: 'Ctrl+S', act: () => void A.projectSave(false) },
      { t: 'item', label: t('a.mn.saveAs'), kbd: 'Ctrl+Shift+S', act: () => void A.projectSave(true) },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.export'), kbd: 'Ctrl+Shift+R', act: () => setU('exportOpen', true) },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.settings'), act: () => setU('settings', true) },
      ...(win ? [{ t: 'sep' } as MenuItem, { t: 'item', label: t('a.mn.exit'), kbd: 'Alt+F4', act: () => void win.close() } as MenuItem] : []),
    ]],
    ['a.mn.edit', () => [
      { t: 'item', label: t('a.mn.undo'), kbd: 'Ctrl+Z', disabled: !S.canUndo, act: () => void A.undo() },
      { t: 'item', label: t('a.mn.redo'), kbd: 'Ctrl+Shift+Z', disabled: !S.canRedo, act: () => void A.redo() },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.cut'), kbd: 'Ctrl+X', disabled: none(), act: A.cut },
      { t: 'item', label: t('a.mn.copy'), kbd: 'Ctrl+C', disabled: none(), act: A.copy },
      { t: 'item', label: t('a.mn.paste'), kbd: 'Ctrl+V', disabled: !A.hasClipboard(), act: () => A.paste() },
      { t: 'item', label: t('a.mn.dup'), kbd: 'Ctrl+B', disabled: none(), act: A.duplicate },
      { t: 'item', label: t('a.mn.del'), kbd: 'Del', disabled: none(), act: () => A.delIds([...U.sel]) },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.selAll'), kbd: 'Ctrl+A', disabled: !S.project.clips.length, act: A.selectAll },
    ]],
    ['a.mn.view', () => [
      { t: 'item', label: t('a.mn.browser'), kbd: 'Alt+F8', checked: U.pickOpen, act: () => setU('pickOpen', !U.pickOpen) },
      { t: 'item', label: t('a.mn.mixer'), kbd: 'F9', checked: U.mixOpen, act: () => setU('mixOpen', !U.mixOpen) },
      { t: 'item', label: t('a.mn.rack'), kbd: 'F6', checked: !!U.rackWin, act: toggleRack },
      { t: 'item', label: t('a.mn.roll'), kbd: 'F7', checked: !!U.rollWin, act: toggleRoll },
      { t: 'item', label: t(U.plugsHidden ? 'a.mn.winsShow' : 'a.mn.winsHide'), kbd: 'F5', disabled: !wins(), act: () => (U.plugsHidden ? A.showPlugs() : A.playlistFront()) },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.follow'), kbd: 'F', checked: U.follow, act: () => setU('follow', !U.follow) },
      { t: 'item', label: t('a.mn.zoomIn'), kbd: 'Ctrl+=', act: () => A.zoomAt(1.25) },
      { t: 'item', label: t('a.mn.zoomOut'), kbd: 'Ctrl+−', act: () => A.zoomAt(0.8) },
    ]],
    ['a.mn.help', () => [
      { t: 'item', label: t('a.mn.shortcuts'), kbd: '?', act: () => setU('help', true) },
      { t: 'sep' },
      { t: 'item', label: t('a.mn.updates'), act: () => void checkUpdates() },
      { t: 'item', label: t('a.mn.whatsNew'), act: () => void showWhatsNew() },
    ]],
  ];
  const show = (i: number, el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    closeMenu(); // remount: an open panel keeps the position it was mounted at
    openMenu(r.left, r.bottom + 3, menus[i][1](), 340);
    setOpen(i);
  };
  return (
    <div class="menubar" classList={{ open: open() >= 0 }} role="menubar">
      <For each={menus}>
        {([key], i) => (
          <button class="mbtn" role="menuitem" aria-haspopup="menu" aria-expanded={open() === i()} classList={{ on: open() === i() }}
            onPointerDown={e => { if (e.button !== 0) return; e.preventDefault(); if (open() === i()) closeMenu(); else show(i(), e.currentTarget); }}
            onPointerEnter={e => { if (open() >= 0 && open() !== i()) show(i(), e.currentTarget); }}>{t(key)}</button>
        )}
      </For>
    </div>
  );
}

/**
 * FL's pattern selector: the current pattern (number, colour, name). Click = the patterns menu
 * (pick, new, clone, rename, split, delete); wheel = previous / next; + = new pattern.
 */
function PatSelector() {
  const n = () => { const p = curPat(); return p ? S.project.patterns.indexOf(p) + 1 : 0; };
  let acc = 0;
  const wheel = (e: WheelEvent) => {
    e.preventDefault();
    acc += e.deltaY || e.deltaX;
    if (Math.abs(acc) < 40) return;
    stepPattern(acc > 0 ? 1 : -1); acc = 0;
  };
  return (
    <div class="patselbox" data-patsel>
      <button class="patselbtn" title={t('b.ps.title')} onWheel={wheel}
        onClick={e => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); patternMenu(r.left, r.bottom + 4, true); }}>
        <Show when={curPat()} fallback={<span class="ell" style={{ color: 'var(--fg5)' }}>—</span>}>
          {p => <>
            <span class="num mono">{n()}</span>
            <span class="sw" style={{ background: p().color }} />
            <span class="ell">{p().name}</span>
          </>}
        </Show>
        <svg width="8" height="8" viewBox="0 0 8 8"><path d="M1 2.5l3 3 3-3" fill="none" stroke="currentColor" stroke-width="1.3" /></svg>
      </button>
      <button class="patselnew" title={t('b.pat.new')} onClick={() => { newPattern(); openRack(); }}><Icon d="M12 5v14 M5 12h14" size={11} /></button>
    </div>
  );
}

const shortcuts = (): [string, string][] => [
  ['Play / Stop', t('a.sc.1')],
  [t('a.sc.2'), 'F9'],
  [t('a.sc.3'), 'F5'],
  [t('a.sc.4'), t('a.sc.5')],
  [t('a.sc.107'), 'F7'],
  [t('a.sc.108'), 'F4 · F2'],
  [t('a.sc.109'), 'Num 1–9 · Num + −'],
  [t('a.sc.110'), 'Shift+Ctrl+C'],
  [t('a.sc.111'), 'G'],
  [t('a.sc.6'), 'Shift+F9'],
  [t('a.sc.7'), t('a.sc.8')],
  [t('a.sc.9'), 'R'],
  [t('a.sc.164'), 'Ctrl+T'],
  [t('a.sc.160'), t('a.sc.161')],
  [t('a.sc.162'), t('a.sc.163')],
  [t('a.sc.10'), 'F'],
  [t('a.sc.11'), 'Home'],
  ['Browser', 'Alt+F8'],
  [t('a.sc.12'), 'F8'],
  [t('a.sc.13'), t('a.sc.14')],
  [t('a.sc.15'), t('a.sc.16')],
  [t('a.sc.17'), 'Esc'],
  [t('a.sc.18'), 'P · B · D'],
  [t('a.sc.19'), 'T · C · E'],
  [t('a.sc.20'), t('a.sc.21')],
  [t('a.sc.22'), t('a.sc.23')],
  [t('a.sc.24'), 'Ctrl+A'],
  [t('a.sc.25'), 'Esc'],
  [t('a.sc.26'), t('a.sc.27')],
  [t('a.sc.28'), t('a.sc.29')],
  [t('a.sc.30'), 'Ctrl+B · Ctrl+D'],
  [t('a.sc.31'), 'Ctrl+C · X · V'],
  [t('a.sc.32'), 'Del'],
  [t('a.sc.33'), 'Ctrl+M'],
  [t('a.sc.stretchMode'), 'Shift+M'],
  [t('a.sc.34'), 'Shift + ← →'],
  [t('a.sc.35'), 'Shift + ↑ ↓'],
  [t('a.sc.36'), 'Ctrl+Z · Ctrl+Shift+Z'],
  [t('a.sc.37'), t('a.sc.38')],
  [t('a.sc.39'), t('a.sc.40')],
  [t('a.sc.41'), t('a.sc.14')],
  [t('a.sc.42'), t('a.sc.43')],
  [t('a.sc.44'), t('a.sc.45')],
  [t('a.sc.46'), t('a.sc.47')],
  [t('a.sc.48'), t('a.sc.49')],
  [t('a.sc.50'), t('a.sc.51')],
  [t('a.sc.52'), t('a.sc.53')],
  [t('a.sc.54'), t('a.sc.55')],
  [t('a.sc.56'), 'Ctrl+N · Ctrl+O'],
  [t('a.sc.57'), 'Ctrl+S · Ctrl+Shift+S'],
  [t('a.sc.58'), 'Ctrl+Shift+R'],
  [t('a.sc.59'), t('a.sc.60')],
  [t('a.sc.61'), t('a.sc.62')],
  ['Comping (swipe)', t('a.sc.63')],
  [t('a.sc.64'), t('a.sc.65')],
  [t('a.sc.66'), 'Ctrl + ↑ ↓'],
  [t('a.sc.67'), t('a.sc.68')],
  [t('a.sc.69'), t('a.sc.70')],
  [t('a.sc.71'), t('a.sc.72')],
  [t('a.sc.73'), t('a.sc.74')],
  [t('a.sc.75'), '1–6'],
  [t('a.sc.76'), 'F'],
  [t('a.sc.77'), t('a.sc.78')],
  [t('a.sc.79'), t('a.sc.80')],
  [t('a.sc.81'), t('a.sc.82')],
  [t('a.sc.83'), t('a.sc.84')],
  [t('a.sc.85'), t('a.sc.86')],
  ['Velocity', t('a.sc.87')],
  [t('a.sc.88'), 'P · B · E · D'],
  [t('a.sc.89'), '↑ ↓ · Ctrl + ↑ ↓'],
  [t('a.sc.90'), 'Ctrl+B · Ctrl+D'],
  [t('a.sc.91'), 'Ctrl+C · X · V'],
  [t('a.sc.112'), t('a.sc.113')],
  [t('a.sc.170'), t('a.sc.171')],
  [t('a.sc.172'), t('a.sc.173')],
  [t('a.sc.174'), t('a.sc.175')],
  [t('a.sc.176'), t('a.sc.177')],
  [t('a.sc.178'), t('a.sc.179')],
  [t('a.sc.180'), t('a.sc.181')],
  [t('a.sc.182'), t('a.sc.183')],
  [t('a.sc.184'), t('a.sc.185')],
  [t('a.sc.186'), t('a.sc.187')],
  [t('a.sc.188'), t('a.sc.189')],
  [t('a.sc.114'), t('a.sc.115')],
  [t('a.sc.140'), t('a.sc.141')],
  [t('a.sc.142'), 'PgUp · PgDn'],
  [t('a.sc.143'), 'Shift+1 2 3 · Shift+4'],
  [t('a.sc.144'), t('a.sc.145')],
  [t('a.sc.146'), t('a.sc.147')],
  [t('a.sc.148'), t('a.sc.149')],
  [t('a.sc.150'), 'Alt + ↑ ↓'],
  [t('a.sc.151'), 'Ctrl+C · X · V'],
  [t('a.sc.152'), t('a.sc.153')],
  [t('a.sc.92'), t('a.sc.93')],
  [t('a.sc.94'), t('a.sc.95')],
  [t('a.sc.96'), t('a.sc.97')],
  [t('a.sc.98'), t('a.sc.29')],
  [t('a.sc.99'), t('a.sc.100')],
  [t('a.sc.101'), t('a.sc.102')],
  [t('a.sc.103'), t('a.sc.104')],
  [t('a.sc.105'), t('a.sc.106')],
];

export function Help() {
  return (
    <Show when={U.help}>
      <div class="overlay" onPointerDown={() => setU('help', false)}>
        <div class="dialog" style={{ width: 'min(680px,92vw)', 'max-height': '90vh', 'overflow-y': 'auto' }} onPointerDown={e => e.stopPropagation()}>
          <div class="hd"><span>{t('a.tb.helpTitle')}</span><button onClick={() => setU('help', false)}>×</button></div>
          <div style={{ display: 'grid', 'grid-template-columns': 'repeat(auto-fit,minmax(280px,1fr))', 'column-gap': '24px' }}>
            <For each={shortcuts()}>{([a, k]) => <div class="sc"><span class="a">{a}</span><span class="k">{k}</span></div>}</For>
          </div>
        </div>
      </div>
    </Show>
  );
}
