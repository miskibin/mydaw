import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { baseName, clamp, fmtClock, fmtPos, TapTracker } from '../lib/format';
import { SNAPS, snapLabel } from '../lib/snap';
import * as A from '../state/actions';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { S } from '../state/store';
import { curBeat, tele } from '../state/telemetry';
import { openMenu, say, setU, U, type Tool } from '../state/ui';
import { track } from '../ui/drag';
import { addFrameTask } from '../ui/frameLoop';
import { Icon, ICONS } from '../ui/icons';
import { NB, toggleNotebook } from '../state/notebook';
import { NOTEBOOK_ICON } from './Notebook';

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
      say('Tap tempo — klikaj dalej w rytm utworu…');
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
      { t: 'item', label: 'Wybij tempo (Tap)', kbd: 'klikaj TAP', act: () => doTap() },
      { t: 'item', label: 'Wykryj tempo i tonację z projektu…', disabled: !hasAudio, act: () => A.openMusicAnalysis({}) },
      { t: 'sep' },
      { t: 'item', label: `Połowa tempa (${clampBpm(b / 2).toFixed(1)} BPM)`, disabled: b / 2 < 20, act: () => setB(b / 2, 'Połowa tempa') },
      { t: 'item', label: `Podwójne tempo (${clampBpm(b * 2).toFixed(1)} BPM)`, disabled: b * 2 > 999, act: () => setB(b * 2, 'Podwójne tempo') },
      { t: 'sep' },
      { t: 'chips', label: 'Szybki wybór', options: ['80', '90', '100', '110', '120', '128', '140', '160'], active: [80, 90, 100, 110, 120, 128, 140, 160].indexOf(Math.round(b)), pick: i => setB([80, 90, 100, 110, 120, 128, 140, 160][i]) },
    ], 260);
  };
  const T = () => S.transport;
  const projName = () => (S.path ? baseName(S.path) : 'Bez nazwy');
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
    if (el !== e.currentTarget && !el.closest('[data-drag]')) return;
    const now = performance.now();
    if (now - lastDown < 350) { lastDown = 0; void win.toggleMaximize(); return; }
    lastDown = now;
    void win.startDragging();
  };
  return (
    <div class="toolbar" classList={{ native: !!win }} onMouseDown={dragDown}>
      <button class="ibtn" classList={{ on: U.pickOpen }} onClick={() => setU('pickOpen', !U.pickOpen)} title="Browser (Alt+F8)">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9 4v16" /></svg>
      </button>
      <button class="ibtn" classList={{ on: NB.open }} onClick={toggleNotebook} title="Notatnik — tekst utworu, linia = takt (F8)" data-nbbtn>
        <Icon d={NOTEBOOK_ICON} size={15} sw={1.8} />
      </button>
      <div class="row" style={{ gap: '1px' }}>
        <button class="ibtn" disabled={!S.canUndo} onClick={() => void A.undo()} title="Cofnij (Ctrl+Z)"><Icon d="M9 14L4 9l5-5 M4 9h10a6 6 0 0 1 0 12h-3" /></button>
        <button class="ibtn" disabled={!S.canRedo} onClick={() => void A.redo()} title="Ponów (Ctrl+Shift+Z)"><Icon d="M15 14l5-5-5-5 M20 9H10a6 6 0 0 0 0 12h3" /></button>
      </div>
      <div class="vsep" />
      <div class="seg">
        <button title="Play / Stop (Spacja)" data-act="play" classList={{ play: U.playing }} onClick={() => void A.togglePlay()}>
          <svg width="11" height="11" viewBox="0 0 14 14"><path d="M3 1.5v11l9-5.5z" fill="currentColor" /></svg>
        </button>
        <button title="Stop (wraca na początek utworu; Spacja wraca do znacznika startu)" onClick={() => void A.stop(true)}><svg width="10" height="10" viewBox="0 0 12 12"><rect x="1" y="1" width="10" height="10" rx="1" fill="currentColor" /></svg></button>
        <button title="Nagrywanie (R)" class="rec" classList={{ armed: T().rec }} onClick={A.toggleRec}>
          <svg width="10" height="10" viewBox="0 0 12 12"><circle cx="6" cy="6" r="5" fill="currentColor" /></svg>
        </button>
        <button title="Odsłuch wejścia" class="mon" classList={{ on: T().monitor }} onClick={A.toggleMonitor}>
          <Icon d="M4 15v-3a8 8 0 0 1 16 0v3 M3 14h4v7H3z M17 14h4v7h-4z" size={13} />
        </button>
        <button title="Metronom" classList={{ on: T().metro, dim: !T().metro }} onClick={() => void A.setTransportFlag('metro', !T().metro)}>
          <Icon d="M9 3h6l4 18H5z M12 16l6-9" size={13} />
        </button>
        <button class="count" classList={{ on: T().countIn }} title="Count-in: 1 takt przed nagrywaniem" onClick={() => void A.setTransportFlag('countIn', !T().countIn)}>1 BAR</button>
      </div>
      {/* the slot stays reserved while recording is off, so pressing R never re-wraps the toolbar
          (and shifts the playlist) on a narrow window */}
      <div class="row" title="Poziom wejścia" style={{ gap: '6px', visibility: T().rec || T().monitor ? 'visible' : 'hidden' }}>
        <span class="tag9">IN</span>
        <div class="inmeter"><div ref={inEl} /></div>
      </div>
      <div class="clockbox" onContextMenu={bpmMenu}>
        <span ref={clockEl} class="clock">1:01:01</span>
        <div style={{ width: '1px', height: '14px', background: 'var(--line)' }} />
        <span class="bpm" title="Tempo — przeciągnij, kółko, Shift = setne · PPM = menu tempa" onPointerDown={bpmDown} onWheel={bpmWheel}>{S.project.bpm.toFixed(2)}</span>
        <span class="tag9">BPM</span>
        <button class="tapbtn" classList={{ on: tapping() }} data-tap title="Wybij tempo (Tap) — klikaj w rytm utworu (Shift = z częściami setnymi)" onClick={e => doTap(e)}>TAP</button>
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
      <div data-drag class="dragfill" />
      <span data-drag class="projName" title={(S.path ?? 'Projekt niezapisany') + (S.dirty ? ' — niezapisane zmiany' : '')}>{projName()}<Show when={S.dirty}><span data-drag class="dirty">•</span></Show></span>
      <button class="tbtn" classList={{ on: U.exportOpen }} onClick={() => setU('exportOpen', true)} title="Eksport do WAV (Ctrl+Shift+R)">
        <Icon d="M12 3v12 M8 11l4 4 4-4 M5 21h14" size={13} />Eksport
      </button>
      <span data-drag class="loopText" style={{ color: S.project.loop ? 'var(--green)' : 'var(--fg5)' }}>{S.project.loop ? 'LOOP ' + fmtPos(S.project.loop.a) + ' – ' + fmtPos(S.project.loop.b) : 'SONG'}</span>
      <Show when={U.plugs.length + (U.patWin ? 1 : 0)}>
        <button class="tbtn" classList={{ on: !U.plugsHidden }} onClick={() => (U.plugsHidden ? A.showPlugs() : A.playlistFront())}
          title={U.plugsHidden ? `Pokaż okna (efekty, pattern) (F5) — ${U.plugs.length + (U.patWin ? 1 : 0)}` : 'Schowaj okna efektów i patternu za playlistę (F5, albo klik w playlistę)'}>
          <Icon d="M4 7h12v12H4z M8 3h12v12" size={13} />Okna {U.plugs.length + (U.patWin ? 1 : 0)}
        </button>
      </Show>
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
      <button class="ibtn" classList={{ on: U.settings }} style={{ width: '26px', height: '26px' }} onClick={() => setU('settings', !U.settings)} title="Ustawienia (audio, wygląd)"><Icon d={ICONS.gear} size={14} sw={1.8} /></button>
      <button class="zbtn" classList={{ on: U.help }} style={{ font: '600 12px var(--mono)' }} onClick={() => setU('help', !U.help)} title="Skróty klawiszowe (?)">?</button>
      <Show when={win}>
        <div class="winbtns">
          <button title="Minimalizuj" onClick={() => void win!.minimize()}><svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 5h10" stroke="currentColor" /></svg></button>
          <button title={maxed() ? 'Przywróć' : 'Maksymalizuj'} onClick={() => void win!.toggleMaximize()}>
            <Show when={maxed()} fallback={<svg width="10" height="10" viewBox="0 0 10 10"><rect x=".5" y=".5" width="9" height="9" fill="none" stroke="currentColor" /></svg>}>
              <svg width="10" height="10" viewBox="0 0 10 10"><path d="M2.5 2.5V.5h7v7h-2 M.5 2.5h7v7h-7z" fill="none" stroke="currentColor" /></svg>
            </Show>
          </button>
          <button class="close" title="Zamknij" onClick={() => void win!.close()}><svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1.1" /></svg></button>
        </div>
      </Show>
    </div>
  );
}

export function StatusBar() {
  const hint = () => TOOLS.find(t => t[0] === U.tool)![2] + ': ' + TOOL_HINT[U.tool] + ' · snap ' + (U.alt ? 'off' : snapLabel(U.snap, U.bw).replace('Snap: ', '')) + ' · ? skróty';
  return (
    <div class="status">
      <span style={{ color: S.transport.rec && U.playing ? 'var(--red)' : U.playing ? 'var(--green)' : 'var(--fg6)' }}>●</span>
      <span style={{ color: 'var(--fg)' }} data-readout>{U.readout}</span>
      <div style={{ flex: 1 }} />
      <Show when={U.alt}><span class="offgrid">OFF GRID</span></Show>
      <span style={{ color: 'var(--fg5)' }}>{hint()}</span>
    </div>
  );
}

const SHORTCUTS: [string, string][] = [
  ['Play / Stop', 'Spacja'], ['Mikser', 'F9'], ['Playlista na wierzch (chowa okna efektów i patternu)', 'F5'], ['Edytor patternu (Channel Rack)', 'F6 · 2× klik na pattern'], ['Dodaj efekt do zaznaczonego kanału', 'F7'], ['Edytor sampla', '2× klik na klip audio'], ['Nagrywanie (uzbrój)', 'R'], ['Podążanie za głowicą', 'F'], ['Start na 0', 'Home'], ['Browser', 'Alt+F8'],
  ['Notatnik (tekst, linia = takt)', 'F8'], ['Notatnik: rozmiar czcionki', 'Ctrl + kółko'], ['Notatnik: głowica na takt', 'klik w numer taktu'], ['Notatnik: wyjdź z edycji', 'Esc'],
  ['Rysuj · Maluj · Usuń', 'P · B · D'], ['Wycisz · Tnij · Zaznacz', 'T · C · E'],
  ['Zaznacz prostokątem', 'Ctrl + przeciągnij'], ['Dodaj/usuń z zaznaczenia', 'Ctrl + klik'], ['Zaznacz wszystko', 'Ctrl+A'], ['Odznacz', 'Esc'],
  ['Kopiuj przeciągając', 'Shift + przeciągnij'], ['Poza siatką (off grid)', 'trzymaj Alt'], ['Duplikuj za zaznaczeniem', 'Ctrl+B'],
  ['Kopiuj · Wytnij · Wklej', 'Ctrl+C · X · V'], ['Usuń zaznaczone', 'Del'], ['Wycisz zaznaczone', 'Ctrl+M'],
  ['Przesuń w czasie', 'Shift + ← →'], ['Przesuń między ścieżkami', 'Shift + ↑ ↓'], ['Cofnij · Ponów', 'Ctrl+Z · Ctrl+Shift+Z'],
  ['Usuń klipy pod kursorem', 'PPM + przeciągnij'], ['Menu kontekstowe', 'PPM'], ['Zoom poziomy', 'Ctrl + kółko'], ['Wysokość wszystkich ścieżek', 'Alt + kółko'],
  ['Wysokość jednej ścieżki', 'przeciągnij dolną krawędź'], ['Pętla', 'przeciągnij po linijce'], ['Zmień nazwę ścieżki', '2× klik w nazwę'],
  ['Kolejność efektów', 'przeciągnij slot ↕'], ['Poprzedni / następny preset', '‹ › w oknie efektu'], ['Obrót widoku 3D (Reverb)', 'przeciągnij wizualizację'],
  ['Nowy · Otwórz projekt', 'Ctrl+N · Ctrl+O'], ['Zapisz · Zapisz jako', 'Ctrl+S · Ctrl+Shift+S'],
  ['Eksport do WAV (master)', 'Ctrl+Shift+R'],
  ["Nagrywanie take'ów", 'ustaw pętlę i nagrywaj: 1 przebieg = 1 take'], ["Pokaż take'i", '2× klik na comp · ▸ w nagłówku ścieżki'],
  ['Comping (swipe)', "przeciągnij po linii take'a"], ["Cały take do compu", "klik w nazwę take'a"],
  ['Poprzedni / następny take fragmentu', 'Ctrl + ↑ ↓'], ['Granica compu · crossfade', 'przeciągnij · PPM'],
  ['Automatyzuj pokrętło / suwak', 'PPM → Automatyzuj na playliście'], ['Edycja punktów automatyzacji', '2× klik na klip · klik = punkt · PPM = usuń'],
  ['Edytor wysokości', 'PPM na klipie audio → Edytuj wysokość…'], ['Narzędzia edytora wysokości', '1–6'], ['Dopasuj widok edytora', 'F'],
  ['Krok: włącz · wyłącz', 'klik · PPM (przeciągnij = maluj)'], ['Odsłuch kanału', 'Ctrl + klik w nazwę'], ['Mute · Solo kanału', 'klik w diodę · PPM na diodzie'],
  ['Menu kanału (wypełnij, przesuń, klonuj…)', 'PPM na kanale'], ['Piano roll kanału melodycznego', '2× klik w nazwę / wiersz'], ['Velocity', 'przeciągnij w pasku pod siatką'],
  ['Piano roll: rysuj · maluj · zaznacz · usuń', 'P · B · E · D'], ['Piano roll: półton · oktawa', '↑ ↓ · Ctrl + ↑ ↓'], ['Piano roll: duplikuj w prawo', 'Ctrl+B'],
  ['Piano roll: bez siatki', 'trzymaj Alt'], ['Podgląd patternu w pętli', '▶ Pętla w oknie patternu'],
  ['Nowy band EQ', '2× klik na wykresie'], ['Wytnij rezonans', 'przeciągnij z podświetlonego szczytu widma'], ['Q bandu', 'kółko nad punktem'],
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
