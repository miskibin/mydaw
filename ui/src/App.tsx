import { createEffect, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from './api/backend';
import { baseName } from './lib/format';
import { snapRes } from './lib/snap';
import * as A from './state/actions';
import { findFx, S } from './state/store';
import { swapTake } from './state/comp';
import { closeMenu, say, setU, U } from './state/ui';
import { ContextMenu } from './ui/Menu';
import { Browser, importAt } from './views/Browser';
import { FxWindow } from './views/FxWindow';
import { Mixer } from './views/Mixer';
import { Playlist } from './views/Playlist';
import { SampleEditor } from './views/SampleEditor';
import { PitchEditor } from './views/PitchEditor';
import { Export } from './views/Export';
import { Settings, VstPicker } from './views/Settings';
import { MusicDialog } from './views/MusicDialog';
import { FxPickerHost, openFxPicker } from './views/FxPicker';
import { Help, StatusBar, Toolbar, TOOLS } from './views/Toolbar';

/** Stacking rank of a plugin window inside the window layer (1 = bottom). */
function plugRank(z: number) { let r = 1; for (const p of U.plugs) if (p.z < z) r++; return r; }

function onKey(e: KeyboardEvent) {
  if (e.key === 'Alt') { e.preventDefault(); if (!U.alt) setU('alt', true); return; }
  const tg = (e.target as HTMLElement).tagName;
  if (tg === 'INPUT' || tg === 'SELECT' || tg === 'TEXTAREA') return;
  const k = e.key.toLowerCase(), m = e.ctrlKey || e.metaKey;
  if (e.code === 'Space') { e.preventDefault(); (document.activeElement as HTMLElement | null)?.blur?.(); if (!e.repeat) void A.togglePlay(); return; }
  if (e.key === 'Escape') {
    if (U.menu) return closeMenu();
    if (U.autoEdit != null) return setU('autoEdit', null);
    if (U.musicWin) return A.closeMusicAnalysis();
    if (U.fxPicker) return setU('fxPicker', null);
    if (U.vstPicker) return setU('vstPicker', null);
    if (U.settings) return setU('settings', false);
    if (U.exportOpen) return void A.cancelExportOrClose();
    if (U.help) return setU('help', false);
    if (U.plugs.length) return A.closePlug(A.topPlug()!.fxId);
    if (U.pitchEd) return A.closePitchEditor();
    if (U.editor) return A.closeEditor();
    if (U.compSel) return setU('compSel', null);
    return setU('sel', []);
  }
  if (e.key === 'F8') { e.preventDefault(); return setU('pickOpen', !U.pickOpen); }
  if (e.key === 'F7') { e.preventDefault(); return U.fxPicker ? setU('fxPicker', null) : openFxPicker(U.mixSel); }
  if (e.key === 'F5') { e.preventDefault(); return U.plugsHidden ? A.showPlugs() : A.playlistFront(); }
  if (e.key === 'F9') { e.preventDefault(); return setU('mixOpen', !U.mixOpen); }
  if (m && k === 'z') { e.preventDefault(); return void (e.shiftKey ? A.redo() : A.undo()); }
  if (m && k === 'y') { e.preventDefault(); return void A.redo(); }
  if (m && k === 'n') { e.preventDefault(); return void A.projectNew(); }
  if (m && k === 'o') { e.preventDefault(); return void A.projectOpen(); }
  if (m && k === 's') { e.preventDefault(); return void A.projectSave(e.shiftKey); }
  if (m && e.shiftKey && k === 'r') { e.preventDefault(); return setU('exportOpen', true); }
  if (m && k === 'a') { e.preventDefault(); return A.selectAll(); }
  if (m && k === 'b') { e.preventDefault(); return A.duplicate(); }
  if (m && k === 'c') { e.preventDefault(); return A.copy(); }
  if (m && k === 'x') { e.preventDefault(); return A.cut(); }
  if (m && k === 'v') { e.preventDefault(); return A.paste(); }
  if (m && k === 'm') { e.preventDefault(); return A.toggleMuteSel(); }
  if (m && (k === '=' || k === '+')) { e.preventDefault(); return A.zoomAt(1.25); }
  if (m && k === '-') { e.preventDefault(); return A.zoomAt(0.8); }
  if (e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); return A.delIds([...U.sel]); }
  if (e.key === 'Home') { e.preventDefault(); return A.seek(0); }
  if (e.key === '?') return setU('help', !U.help);
  if (m && !e.shiftKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown') && swapTake(e.key === 'ArrowUp' ? -1 : 1)) { e.preventDefault(); return; }
  if (e.shiftKey && e.key.startsWith('Arrow')) {
    e.preventDefault();
    const u = snapRes(U.snap, U.bw) || 0.25;
    if (e.key === 'ArrowLeft') A.nudge(-u, 0);
    if (e.key === 'ArrowRight') A.nudge(u, 0);
    if (e.key === 'ArrowUp') A.nudge(0, -1);
    if (e.key === 'ArrowDown') A.nudge(0, 1);
    return;
  }
  if (!m && !e.altKey) {
    if (k === 'r') return A.toggleRec();
    if (k === 'f') { setU('follow', !U.follow); return say(U.follow ? 'Podążanie za głowicą: wł.' : 'Podążanie za głowicą: wył.'); }
    const t = TOOLS.find(x => x[1].toLowerCase() === k);
    if (t) { setU('tool', t[0]); say('Narzędzie: ' + t[2]); }
  }
}
const onKeyUp = (e: KeyboardEvent) => {
  if (e.key === 'Alt') { e.preventDefault(); setU('alt', false); }
  if (e.code === 'Space' && (e.target as HTMLElement).tagName === 'BUTTON') e.preventDefault(); // no button click on Space
};
const onBlur = () => { if (U.alt) setU('alt', false); };

export function App() {
  onMount(() => {
    window.addEventListener('keydown', onKey);
    window.addEventListener('keyup', onKeyUp);
    window.addEventListener('blur', onBlur);
    let offDrop: (() => void) | null = null;
    be().onOsDrop(ev => {
      if (ev.type === 'over') { if (!U.dragOver) setU('dragOver', true); }
      else if (ev.type === 'leave') setU('dragOver', false);
      else { setU('dragOver', false); if (ev.paths.length) void importAt(ev.paths, ev.x, ev.y, U.alt); }
    }).then(u => { offDrop = u; });
    onCleanup(() => {
      window.removeEventListener('keydown', onKey); window.removeEventListener('keyup', onKeyUp); window.removeEventListener('blur', onBlur);
      offDrop?.();
    });
  });

  // effects whose windows are open → analysis in the engine (≤ 8)
  let lastWatch = '';
  createEffect(() => {
    // the 8 front-most windows (engine limit), in a stable order so focusing does not re-send
    const ids = (U.plugsHidden ? [] : [...U.plugs]).sort((a, b) => b.z - a.z).map(p => p.fxId).filter(id => { const f = findFx(id)?.f; return f && f.type !== 'vst3'; }).slice(0, 8).sort((a, b) => a - b);
    const key = ids.join(',');
    if (key !== lastWatch) { lastWatch = key; be().watchFx(ids).catch(e => console.warn(e)); }
  });
  // close windows of effects that disappeared (undo, CLI, …)
  createEffect(() => {
    const gone = U.plugs.filter(p => !findFx(p.fxId));
    if (gone.length) setU('plugs', U.plugs.filter(p => findFx(p.fxId)));
    if (U.editor && !S.project.clips.some(c => c.id === U.editor!.clipId)) setU('editor', null);
    if (U.pitchEd && !S.project.clips.some(c => c.id === U.pitchEd!.clipId)) setU('pitchEd', null);
    if (U.musicWin?.clipId != null && !S.project.clips.some(c => c.id === U.musicWin!.clipId)) setU('musicWin', null);
    if (U.musicWin?.fxId != null && !findFx(U.musicWin!.fxId)) setU('musicWin', null);
  });
  createEffect(() => { be().setTitle((S.path ? baseName(S.path) : 'Bez nazwy') + (S.dirty ? ' •' : '') + ' — WAVE'); });

  return (
    <div class="app" onContextMenu={e => e.preventDefault()}>
      <Toolbar />
      <div style={{ flex: 1, 'min-height': 0, display: 'flex' }}>
        <Show when={U.pickOpen}><Browser /></Show>
        <Playlist />
      </div>
      <Show when={U.mixOpen}><Mixer /></Show>
      <StatusBar />
      <SampleEditor />
      <PitchEditor />
      <div class="winlayer" classList={{ hidden: U.plugsHidden }}>
        <For each={U.plugs}>{pw => <FxWindow pw={pw} z={plugRank(pw.z)} />}</For>
      </div>
      <Show when={U.ghost}>{g => <div class="ghost" style={{ left: g().x + 'px', top: g().y + 'px', 'box-shadow': `inset 0 0 0 1px ${g().color},0 6px 16px rgba(0,0,0,.45)` }}>{g().name}</div>}</Show>
      <ContextMenu />
      <Help />
      <Export />
      <Settings />
      <VstPicker />
      <FxPickerHost />
      <MusicDialog />
    </div>
  );
}
