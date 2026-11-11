import { createEffect, createMemo, For, onCleanup, onMount, Show } from 'solid-js';
import { createStore } from 'solid-js/store';
import { be } from '../api/backend';
import type { ClipKind, Id, Kit, LibFile, LibraryProgress, Source } from '../api/types';
import { clamp, fmtBytes, baseName } from '../lib/format';
import { t } from '../lib/i18n';
import { snapFloor } from '../lib/snap';
import * as A from '../state/actions';
import { getPeaks, peakRange, peaksVersion } from '../state/peaks';
import { S } from '../state/store';
import { openMenu, say, setU, U } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { fitCanvas } from '../ui/frameLoop';
import { dropClipAt, gridPoint } from './Playlist';
import { addSamplerFiles, chanById, channels, newPattern, openPattern, replaceWithFile } from '../state/pattern';
import { libGroup, STD_KIT } from '../state/library';
import { chanWin } from './ChannelWindow';
import { patternItemMenu } from './patternShared';
import { InstIcon, patternIcon } from '../ui/instIcons';

/** Drag something from the browser with a ghost label; `onDrop` gets the release point. */
function dragGhost(e: PointerEvent, name: string, color: string, onClick: () => void, onDrop: (x: number, y: number, alt: boolean) => void) {
  const sx = e.clientX, sy = e.clientY; let drag = false;
  track(ev => {
    if (!drag && Math.hypot(ev.clientX - sx, ev.clientY - sy) > 4) drag = true;
    if (drag) setU('ghost', { x: ev.clientX + 12, y: ev.clientY + 8, name, color });
  }, ev => {
    setU('ghost', null);
    if (!drag) return onClick();
    onDrop(ev.clientX, ev.clientY, ev.altKey);
  }, 'grabbing');
}

function pick(kind: ClipKind, ref: Id, name: string, color: string, e: PointerEvent) {
  if (e.button !== 0) return;
  e.preventDefault();
  setU('cur', { kind, ref });
  say(t('d.br.picked', { name }));
  dragGhost(e, name, color, () => {}, (x, y, alt) => { dropClipAt(x, y, kind, ref, alt); });
}

export async function importFiles(paths: string[], at?: { track: number; beat: number }) {
  if (!paths.length) return;
  try {
    const r = await be().importAudio({ paths, track: at?.track, beat: at?.beat });
    if (r.sourceIds.length) setU('cur', { kind: 'audio', ref: r.sourceIds[r.sourceIds.length - 1] });
    if (r.clipIds.length) setU('sel', r.clipIds);
    say(t('d.br.loaded', { names: paths.map(baseName).join(', ') }));
  } catch (err) { say(t('d.br.loadfail', { err: String(err) })); }
}

/** Import at a client point if it is over the playlist grid. */
export function importAt(paths: string[], x: number, y: number, alt = false) {
  const p = gridPoint(x, y);
  if (!p.inside || p.track < 0) return importFiles(paths);
  return importFiles(paths, { track: p.track, beat: Math.max(0, snapFloor(p.beat, A.snapUnit(alt))) });
}

function Thumb(props: { src: Source }) {
  let cv!: HTMLCanvasElement;
  createEffect(() => {
    peaksVersion();
    const pk = getPeaks(props.src.id); if (!pk || !cv) return;
    const W = cv.clientWidth || 180, H = 14, dpr = window.devicePixelRatio || 1;
    fitCanvas(cv, W, H);
    const g = cv.getContext('2d')!; g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, W, H);
    const bins = pk.length / 2, mid = H / 2;
    g.globalAlpha = 0.7; g.fillStyle = props.src.color; g.beginPath();
    for (let x = 0; x < W; x++) {
      const a = Math.floor(x / W * bins), b = Math.max(a + 1, Math.floor((x + 1) / W * bins));
      const [mn, mx] = peakRange(pk, a, b);
      g.rect(x, mid - mx * mid, 1, Math.max(0.6, (mx - mn) * mid));
    }
    g.fill();
  });
  return <canvas ref={cv} style={{ display: 'block', width: '100%', height: '14px' }} />;
}

function ProjectTab() {
  const chanMap = createMemo(() => new Map((S.project.channels ?? []).map(c => [c.id, c])));
  const importBtn = async () => {
    try { const paths = await be().pickAudioFiles(); if (paths) await importFiles(paths); }
    catch (e) { say(t('d.br.noimport', { err: String(e) })); }
  };
  return (
    <div class="scroll">
      <div class="bhead">
        <span>{t('d.br.patterns')}</span>
        <button class="linkbtn" title={t('d.br.newpat.tip')} onClick={newPattern}>{t('d.br.new')}</button>
      </div>
      <div class="plist">
        <For each={S.project.patterns}>
          {p => (
            <div class="pitem" classList={{ on: U.cur?.kind === 'pat' && U.cur.ref === p.id }} title={t('d.br.pat.tip')} style={{ '--c': p.color }}
              onContextMenu={e => { e.preventDefault(); patternItemMenu(p, e.clientX, e.clientY); }}
              onPointerDown={e => { if (e.button === 0 && dblCheck('pitem' + p.id)) { openPattern(p.id); return; } pick('pat', p.id, p.name, p.color, e); }}>
              <Show when={patternIcon(p, chanMap())} fallback={<span style={{ width: '8px', height: '8px', 'border-radius': '2px', flex: 'none', background: p.color }} />}>
                {id => <span class="picon" style={{ color: p.color }}><InstIcon id={id()} size={12} /></span>}
              </Show>
              <span class="nm">{p.name}</span>
              <span class="meta">{t('d.br.bars', { n: p.len / 4 })}</span>
            </div>
          )}
        </For>
      </div>
      <div class="bhead top">
        <span>{t('d.br.audio')}</span>
        <button class="linkbtn" title={t('d.br.import.tip')} onClick={importBtn}>{t('d.br.import')}</button>
      </div>
      <div style={{ display: 'flex', 'flex-direction': 'column', padding: '4px 0' }}>
        <For each={S.project.sources}>
          {s => (
            <div class="sitem" classList={{ on: U.cur?.kind === 'audio' && U.cur.ref === s.id }} title={t('d.br.src.tip')}
              onPointerDown={e => pick('audio', s.id, s.name, s.color, e)}>
              <div class="row" style={{ gap: '8px' }}>
                <span style={{ width: '8px', height: '8px', 'border-radius': '50%', flex: 'none', background: s.color }} />
                <span class="ell" style={{ flex: 1, font: '500 12px var(--sans)' }}>{s.name}</span>
                <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg4)' }}>{s.dur.toFixed(2)}s</span>
              </div>
              <Thumb src={s} />
            </div>
          )}
        </For>
      </div>
      <div class="bnote">{t('d.br.dragnote')}</div>
    </div>
  );
}

// ---------- library ----------
const [lib, setLib] = createStore<{ kits: Kit[]; folders: string[]; open: Record<string, boolean>; files: Record<string, LibFile[] | undefined>; progress: Record<string, LibraryProgress | undefined>; loaded: boolean; error: string | null }>({
  kits: [], folders: [], open: {}, files: {}, progress: {}, loaded: false, error: null,
});

async function refreshLib() {
  try { const l = await be().libraryList(); setLib({ kits: l.kits, folders: l.folders, loaded: true, error: null }); }
  catch (e) { setLib({ loaded: true, error: String(e) }); }
}
async function toggleOpen(key: string, q: { kit?: string; folder?: string }) {
  const open = !lib.open[key];
  setLib('open', key, open);
  if (open && !lib.files[key]) {
    try { setLib('files', key, await be().libraryFiles(q)); } catch (e) { say(t('d.br.liberr', { err: String(e) })); setLib('files', key, []); }
  }
}

function previewFile(f: LibFile) {
  if (U.previewing && U.previewPath === f.path) { void be().previewStop(); setU('previewPath', null); return; }
  setU('previewPath', f.path);
  be().previewFile(f.path).catch(e => say(t('d.br.preverr', { err: String(e) })));
  say(t('d.br.prev', { name: f.name }));
}

/**
 * A library file let go at a client point: onto a sampler's window = that channel plays it; onto
 * the Channel Rack = a new sampler channel (before the row it was dropped on); anywhere else it
 * is imported, as an audio clip when the point is on a playlist track. One source either way.
 */
function dropFile(f: LibFile, x: number, y: number, alt: boolean) {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  const w = el?.closest('.chanwin') ? chanWin() : null, target = w ? chanById(w.id) : undefined;
  if (target?.kind === 'sample') return void replaceWithFile(target, f);
  if (w || el?.closest('.rackwin')) {
    const row = el?.closest<HTMLElement>('[data-ch]');
    const at = row ? channels().findIndex(c => c.id === Number(row.dataset.ch)) : -1;
    return void addSamplerFiles([f], at >= 0 ? at : undefined);
  }
  void importAt([f.path], x, y, alt);
}

function FileList(props: { k: string }) {
  const groups = () => {
    const m = new Map<string, LibFile[]>();
    for (const f of lib.files[props.k] ?? []) { const g = m.get(f.group) ?? []; g.push(f); m.set(f.group, g); }
    return [...m.entries()];
  };
  return (
    <Show when={lib.files[props.k]} fallback={<div class="fgroup">{t('d.br.loading')}</div>}>
      <For each={groups()} fallback={<div class="fgroup">{t('d.br.nofiles')}</div>}>
        {([g, files]) => (
          <>
            <Show when={g}><div class="fgroup">{libGroup(g)}</div></Show>
            <For each={files}>
              {f => (
                <div class="fitem" classList={{ playing: U.previewing && U.previewPath === f.path }} title={t('d.br.file.tip')}
                  onContextMenu={e => { e.preventDefault(); openMenu(e.clientX, e.clientY, [{ t: 'title', label: f.name }, { t: 'item', label: t('d.br.addsampler'), title: t('d.br.addsampler.tip'), act: () => void addSamplerFiles([f]) }], 90); }}
                  onPointerDown={e => { if (e.button !== 0) return; e.preventDefault(); dragGhost(e, f.name, '#47b8a8', () => previewFile(f), (x, y, alt) => dropFile(f, x, y, alt)); }}>
                  <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" style={{ flex: 'none', opacity: 0.7 }}><path d="M4 12h2l2-6 3 12 3-9 2 3h4" /></svg>
                  <span class="ell">{f.name.replace(/\.[^.]+$/, '')}</span>
                </div>
              )}
            </For>
          </>
        )}
      </For>
    </Show>
  );
}

function LibraryTab() {
  onMount(() => {
    if (!lib.loaded) void refreshLib();
    let off: (() => void) | null = null;
    be().on('library-progress', p => {
      setLib('progress', p.id, p);
      if (p.error) say(t('d.br.instfail', { id: p.id, err: p.error }));
      else if (p.done >= p.total) { setLib('progress', p.id, undefined); void refreshLib(); say(t('d.br.installed', { id: p.id })); }
    }).then(u => { off = u; });
    onCleanup(() => off?.());
  });
  const install = (k: Kit) => {
    setLib('progress', k.id, { id: k.id, done: 0, total: k.bytes });
    be().libraryInstall(k.id).catch(e => { setLib('progress', k.id, undefined); say(t('d.br.insterr', { err: String(e) })); });
  };
  const addFolder = async () => { try { setLib('folders', await be().libraryAddFolder()); } catch (e) { say(String(e)); } };
  const removeFolder = async (p: string) => { try { setLib('folders', await be().libraryRemoveFolder(p)); setLib('files', 'f:' + p, undefined); } catch (e) { say(String(e)); } };
  return (
    <div class="scroll">
      <div class="bhead">{t('d.br.kits')}</div>
      <Show when={lib.error}><div class="bnote" style={{ color: 'var(--danger)' }}>{lib.error}</div></Show>
      <For each={lib.kits}>
        {k => {
          const prog = () => lib.progress[k.id];
          return (
            <div class="kit">
              <div class="top">
                <Show when={k.installed} fallback={<span class="chev" />}>
                  <span class="chev" style={{ cursor: 'pointer' }} onClick={() => toggleOpen('k:' + k.id, { kit: k.id })}>{lib.open['k:' + k.id] ? '▾' : '▸'}</span>
                </Show>
                <span class="nm ell" style={{ cursor: k.installed ? 'pointer' : 'default' }} onClick={() => k.installed && toggleOpen('k:' + k.id, { kit: k.id })}>{k.id === STD_KIT ? t('d.br.stdlib') : k.name}</span>
                <Show when={!k.installed && !prog()}><button class="smallbtn primary" onClick={() => install(k)}>{t('d.br.install')}</button></Show>
              </div>
              <span class="desc" style={{ 'padding-left': '22px' }}>{k.id === STD_KIT ? t('d.br.stdlibDesc') : k.description}</span>
              <span class="meta" style={{ 'padding-left': '22px' }}>{k.id === STD_KIT ? t('d.br.stdlibMeta', { n: k.count }) : t('d.br.kitmeta', { n: k.count, size: fmtBytes(k.bytes) }) + (k.installed ? t('d.br.kitinst') : '')}</span>
              <Show when={prog()}>
                {p => (
                  <div style={{ 'padding-left': '22px', display: 'flex', 'flex-direction': 'column', gap: '3px' }}>
                    <div class="prog"><div style={{ width: clamp(p().total ? p().done / p().total : 0, 0, 1) * 100 + '%' }} /></div>
                    <span class="meta">{p().error ? p().error : t('d.br.downloading', { n: Math.round(p().total ? p().done / p().total * 100 : 0) })}</span>
                  </div>
                )}
              </Show>
              <Show when={k.installed && lib.open['k:' + k.id]}><div style={{ margin: '4px -10px 0' }}><FileList k={'k:' + k.id} /></div></Show>
            </div>
          );
        }}
      </For>
      <div class="bhead top">
        <span>{t('d.br.folders')}</span>
        <button class="linkbtn" onClick={addFolder} title={t('d.br.addfolder.tip')}>{t('d.br.add')}</button>
      </div>
      <For each={lib.folders} fallback={<div class="bnote">{t('d.br.addown')}</div>}>
        {f => (
          <>
            <div class="folder" onClick={() => toggleOpen('f:' + f, { folder: f })} title={f}>
              <span class="chev">{lib.open['f:' + f] ? '▾' : '▸'}</span>
              <span class="ell" style={{ flex: 1 }}>{baseName(f) || f}</span>
              <button class="xbtn" title={t('d.br.rmfolder')} onClick={e => { e.stopPropagation(); void removeFolder(f); }}>×</button>
            </div>
            <Show when={lib.open['f:' + f]}><FileList k={'f:' + f} /></Show>
          </>
        )}
      </For>
      <div class="bnote">{t('d.br.libnote')}</div>
    </div>
  );
}

export function Browser() {
  const split = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, w0 = U.pickW;
    track(ev => setU('pickW', clamp(w0 + ev.clientX - sx, 160, 460)), null, 'col-resize');
  };
  return (
    <>
      <div class="browser" style={{ width: U.pickW + 'px' }}>
        <div class="btabs">
          <button classList={{ on: U.browserTab === 'project' }} onClick={() => setU('browserTab', 'project')}>{t('d.br.tab.project')}</button>
          <button classList={{ on: U.browserTab === 'library' }} onClick={() => setU('browserTab', 'library')}>{t('d.br.tab.library')}</button>
        </div>
        <Show when={U.browserTab === 'project'} fallback={<LibraryTab />}><ProjectTab /></Show>
      </div>
      <div class="vsplit" onPointerDown={split} onDblClick={() => setU('pickOpen', false)} title={t('d.br.resize')} />
    </>
  );
}
