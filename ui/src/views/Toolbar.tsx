import { For, onCleanup, onMount, Show } from 'solid-js';
import { baseName, clamp, fmtClock, fmtPos } from '../lib/format';
import { SNAPS, snapLabel } from '../lib/snap';
import * as A from '../state/actions';
import { Gesture, wheelEdit } from '../state/edit';
import { S } from '../state/store';
import { curBeat, tele } from '../state/telemetry';
import { say, setU, U, type Tool } from '../state/ui';
import { track } from '../ui/drag';
import { addFrameTask } from '../ui/frameLoop';
import { Icon, ICONS } from '../ui/icons';

export const TOOLS: [Tool, string, string][] = [
  ['draw', 'P', 'Rysuj'], ['paint', 'B', 'Maluj'], ['erase', 'D', 'Usuń'], ['mute', 'T', 'Wycisz'], ['slice', 'C', 'Tnij'], ['select', 'E', 'Zaznacz'],
];
export const TOOL_HINT: Record<Tool, string> = { draw: 'klik wstawia · przeciągnij przesuwa · krawędzie przycinają', paint: 'przeciągnij, aby powielać', erase: 'klik / przeciągnij usuwa', mute: 'klik przełącza mute klipu', slice: 'klik tnie na siatce · Alt = swobodnie', select: 'przeciągnij prostokąt' };

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
        inEl.style.background = inHold > 0.89 ? '#e05263' : inHold > 0.5 ? '#f0c25a' : '#4ec08a';
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
  const T = () => S.transport;
  const projName = () => (S.path ? baseName(S.path) : 'Bez nazwy') + (S.dirty ? ' •' : '');
  return (
    <div class="toolbar">
      <button class="ibtn" classList={{ on: U.pickOpen }} onClick={() => setU('pickOpen', !U.pickOpen)} title="Browser (F8)">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9 4v16" /></svg>
      </button>
      <div class="row" style={{ gap: '1px' }}>
        <button class="ibtn" style={{ color: S.canUndo ? '#d5dae2' : '#4a5360' }} onClick={() => void A.undo()} title="Cofnij (Ctrl+Z)"><Icon d="M9 14L4 9l5-5 M4 9h10a6 6 0 0 1 0 12h-3" /></button>
        <button class="ibtn" style={{ color: S.canRedo ? '#d5dae2' : '#4a5360' }} onClick={() => void A.redo()} title="Ponów (Ctrl+Shift+Z)"><Icon d="M15 14l5-5-5-5 M20 9H10a6 6 0 0 0 0 12h3" /></button>
      </div>
      <div class="vsep" />
      <div class="seg">
        <button title="Play / Stop (Spacja)" data-act="play" style={{ background: U.playing ? '#4ec08a' : 'transparent', color: U.playing ? '#07231a' : '#d5dae2' }} onClick={() => void A.togglePlay()}>
          <svg width="11" height="11" viewBox="0 0 14 14"><path d="M3 1.5v11l9-5.5z" fill="currentColor" /></svg>
        </button>
        <button title="Stop" onClick={() => void A.stop()}><svg width="10" height="10" viewBox="0 0 12 12"><rect x="1" y="1" width="10" height="10" rx="1" fill="currentColor" /></svg></button>
        <button title="Nagrywanie (R)" style={{ background: T().rec ? '#e05263' : 'transparent', color: T().rec ? '#12151a' : '#e05263' }} onClick={A.toggleRec}>
          <svg width="10" height="10" viewBox="0 0 12 12"><circle cx="6" cy="6" r="5" fill="currentColor" /></svg>
        </button>
        <button title="Odsłuch wejścia" style={{ background: T().monitor ? '#2c333d' : 'transparent', color: T().monitor ? '#4ec08a' : '#7d8592' }} onClick={A.toggleMonitor}>
          <Icon d="M4 15v-3a8 8 0 0 1 16 0v3 M3 14h4v7H3z M17 14h4v7h-4z" size={13} />
        </button>
        <button title="Metronom" classList={{ on: T().metro }} style={{ color: T().metro ? undefined : '#7d8592' }} onClick={() => void A.setTransportFlag('metro', !T().metro)}>
          <Icon d="M9 3h6l4 18H5z M12 16l6-9" size={13} />
        </button>
        <button class="count" classList={{ on: T().countIn }} title="Count-in: 1 takt przed nagrywaniem" onClick={() => void A.setTransportFlag('countIn', !T().countIn)}>1 BAR</button>
      </div>
      <Show when={T().rec}>
        <div class="row" title="Poziom wejścia" style={{ gap: '6px' }}>
          <span class="tag9">IN</span>
          <div class="inmeter"><div ref={inEl} /></div>
        </div>
      </Show>
      <div class="clockbox">
        <span ref={clockEl} class="clock">1:01:01</span>
        <div style={{ width: '1px', height: '14px', background: 'var(--line)' }} />
        <span class="bpm" title="Tempo — przeciągnij, kółko, Shift = setne" onPointerDown={bpmDown} onWheel={bpmWheel}>{S.project.bpm.toFixed(2)}</span>
        <span class="tag9">BPM</span>
      </div>
      <div class="vsep" />
      <div class="seg tools">
        <For each={TOOLS}>
          {([id, key, title]) => (
            <button classList={{ on: U.tool === id }} title={title + ' (' + key + ') — ' + TOOL_HINT[id]} onClick={() => setU('tool', id)}>
              <Icon d={ICONS[id]} sw={1.9} />
            </button>
          )}
        </For>
      </div>
      <select class="sel" title="Przyciąganie do siatki" value={String(U.snap)} onChange={e => A.setSnap(parseFloat(e.currentTarget.value))} data-snap>
        <For each={SNAPS}>{([label, v]) => <option value={String(v)}>{v < 0 ? snapLabel(-1, U.bw) : label}</option>}</For>
      </select>
      <div style={{ flex: '1 1 0', 'min-width': 0 }} />
      <span class="projName" title={S.path ?? 'Projekt niezapisany'}>{projName()}</span>
      <span class="loopText" style={{ color: S.project.loop ? '#4ec08a' : '#5c6573' }}>{S.project.loop ? 'LOOP ' + fmtPos(S.project.loop.a) + ' – ' + fmtPos(S.project.loop.b) : 'SONG'}</span>
      <button class="tbtn" classList={{ on: U.mixOpen }} onClick={() => setU('mixOpen', !U.mixOpen)} title="Mikser (F9)">
        <Icon d="M5 21v-7M5 10V3M12 21v-9M12 8V3M19 21v-5M19 12V3M2 14h6M9 8h6M16 16h6" size={13} />Mixer
      </button>
      <button class="tbtn" classList={{ on: U.follow }} onClick={() => setU('follow', !U.follow)} title="Podążaj za głowicą (F)">
        <Icon d="M4 12h11 M11 7l5 5-5 5 M20 4v16" size={13} />Follow
      </button>
      <div class="row" style={{ gap: '1px' }}>
        <button class="zbtn" onClick={() => A.zoomAt(0.8)} title="Oddal (Ctrl+−)">−</button>
        <button class="zbtn" onClick={() => A.zoomAt(1.25)} title="Przybliż (Ctrl+=)">+</button>
      </div>
      <button class="ibtn" classList={{ on: U.settings }} style={{ width: '26px', height: '26px' }} onClick={() => setU('settings', !U.settings)} title="Ustawienia audio"><Icon d={ICONS.gear} size={14} sw={1.8} /></button>
      <button class="zbtn" style={{ font: '600 12px var(--mono)', background: U.help ? '#2c333d' : undefined }} onClick={() => setU('help', !U.help)} title="Skróty klawiszowe (?)">?</button>
    </div>
  );
}

export function StatusBar() {
  const hint = () => TOOLS.find(t => t[0] === U.tool)![2] + ': ' + TOOL_HINT[U.tool] + ' · snap ' + (U.alt ? 'off' : snapLabel(U.snap, U.bw).replace('Snap: ', '')) + ' · ? skróty';
  return (
    <div class="status">
      <span style={{ color: S.transport.rec && U.playing ? '#e05263' : U.playing ? '#4ec08a' : '#4a5360' }}>●</span>
      <span style={{ color: '#d5dae2' }} data-readout>{U.readout}</span>
      <div style={{ flex: 1 }} />
      <Show when={U.alt}><span class="offgrid">OFF GRID</span></Show>
      <span style={{ color: '#5c6573' }}>{hint()}</span>
    </div>
  );
}

const SHORTCUTS: [string, string][] = [
  ['Play / Stop', 'Spacja'], ['Mikser', 'F9'], ['Edytor sampla', '2× klik na klip audio'], ['Nagrywanie (uzbrój)', 'R'], ['Podążanie za głowicą', 'F'], ['Start na 0', 'Home'], ['Browser', 'F8'],
  ['Rysuj · Maluj · Usuń', 'P · B · D'], ['Wycisz · Tnij · Zaznacz', 'T · C · E'],
  ['Zaznacz prostokątem', 'Ctrl + przeciągnij'], ['Dodaj/usuń z zaznaczenia', 'Ctrl + klik'], ['Zaznacz wszystko', 'Ctrl+A'], ['Odznacz', 'Esc'],
  ['Kopiuj przeciągając', 'Shift + przeciągnij'], ['Poza siatką (off grid)', 'trzymaj Alt'], ['Duplikuj za zaznaczeniem', 'Ctrl+B'],
  ['Kopiuj · Wytnij · Wklej', 'Ctrl+C · X · V'], ['Usuń zaznaczone', 'Del'], ['Wycisz zaznaczone', 'Ctrl+M'],
  ['Przesuń w czasie', 'Shift + ← →'], ['Przesuń między ścieżkami', 'Shift + ↑ ↓'], ['Cofnij · Ponów', 'Ctrl+Z · Ctrl+Shift+Z'],
  ['Usuń klipy pod kursorem', 'PPM + przeciągnij'], ['Menu kontekstowe', 'PPM'], ['Zoom poziomy', 'Ctrl + kółko'], ['Wysokość wszystkich ścieżek', 'Alt + kółko'],
  ['Wysokość jednej ścieżki', 'przeciągnij dolną krawędź'], ['Pętla', 'przeciągnij po linijce'], ['Zmień nazwę ścieżki', '2× klik w nazwę'],
  ['Kolejność efektów', 'przeciągnij slot ↕'], ['Poprzedni / następny preset', '‹ › w oknie efektu'], ['Obrót widoku 3D (Reverb)', 'przeciągnij wizualizację'],
  ['Nowy · Otwórz projekt', 'Ctrl+N · Ctrl+O'], ['Zapisz · Zapisz jako', 'Ctrl+S · Ctrl+Shift+S'],
];

export function Help() {
  return (
    <Show when={U.help}>
      <div class="overlay" onPointerDown={() => setU('help', false)}>
        <div class="dialog" style={{ width: 'min(680px,92vw)' }} onPointerDown={e => e.stopPropagation()}>
          <div class="hd"><span>Skróty klawiszowe</span><button onClick={() => setU('help', false)}>×</button></div>
          <div style={{ display: 'grid', 'grid-template-columns': 'repeat(auto-fit,minmax(280px,1fr))', 'column-gap': '24px' }}>
            <For each={SHORTCUTS}>{([a, k]) => <div class="sc"><span class="a">{a}</span><span class="k">{k}</span></div>}</For>
          </div>
        </div>
      </div>
    </Show>
  );
}
