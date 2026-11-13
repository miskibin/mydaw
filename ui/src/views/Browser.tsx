import { createEffect, createMemo, For, onCleanup, onMount, Show } from 'solid-js';
import { createStore } from 'solid-js/store';
import { be } from '../api/backend';
import type { Channel, ClipKind, Id, Kit, LibFile, LibraryProgress, Source } from '../api/types';
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
import { addSamplerFiles, chanById, newPattern, openPattern, replaceWithFile } from '../state/pattern';
import { libGroup, STD_KIT } from '../state/library';
import { FACTORY_FILES } from '../lib/factory';
import { groupFiles, groupOpen, matches, searchTerms, stepIndex } from '../lib/libraryView';
import { noteName } from '../lib/pitch';
import { libFileGlyph, libGroupGlyph, type MenuGlyph } from '../ui/menuIcons';
import { chanWin } from './ChannelWindow';
import { patternItemMenu } from './patternShared';
import { guessIcon, InstIcon, patternIcon } from '../ui/instIcons';

/**
 * Drag something from the browser with a ghost label; `onDrop` gets the release point. `over`
 * names what a drop at a point would do (the ghost shows it instead of the name), or null.
 */
function dragGhost(e: PointerEvent, name: string, color: string, onClick: () => void, onDrop: (x: number, y: number, alt: boolean) => void, over?: (x: number, y: number) => string | null) {
  const sx = e.clientX, sy = e.clientY; let drag = false;
  track(ev => {
    if (!drag && Math.hypot(ev.clientX - sx, ev.clientY - sy) > 4) drag = true;
    if (drag) setU('ghost', { x: ev.clientX + 12, y: ev.clientY + 8, name: over?.(ev.clientX, ev.clientY) ?? name, color });
  }, ev => {
    if (over) markRow(null);
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
                {/* the icon its clips show in the Playlist (guessed from the name), else a sample's */}
                <span class="picon" style={{ color: s.color }}><InstIcon id={guessIcon(s.name) ?? 'sample'} size={12} /></span>
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
const STD = 'k:' + STD_KIT;
/**
 * `files` by list key (`k:<kit>`, `f:<folder>`); `open` = kits and folders shown open; `gopen` =
 * groups the user opened or closed while browsing, `sopen` = the same during the current search
 * (forgotten when the search text changes); `sel` = the row the keyboard is on.
 */
const [lib, setLib] = createStore<{
  kits: Kit[]; folders: string[]; open: Record<string, boolean>; files: Record<string, LibFile[] | undefined>; errs: Record<string, string | undefined>;
  gopen: Record<string, boolean | undefined>; sopen: Record<string, boolean | undefined>; q: string; sel: string | null;
  progress: Record<string, LibraryProgress | undefined>; loaded: boolean; error: string | null;
}>({ kits: [], folders: [], open: {}, files: {}, errs: {}, gopen: {}, sopen: {}, q: '', sel: null, progress: {}, loaded: false, error: null });
const terms = createMemo(() => searchTerms(lib.q));
const searching = () => terms().length > 0;
const GROUP_COLOR = new Map(FACTORY_FILES.map(f => [f.group, f.color]));
let listEl: HTMLDivElement | undefined, searchEl: HTMLInputElement | undefined;

const listQuery = (key: string) => (key.startsWith('k:') ? { kit: key.slice(2) } : { folder: key.slice(2) });
const loading = new Map<string, Promise<void>>();
/** Loads a list once; a failure is kept (`errs`) and shown with a way to try again. */
function ensure(key: string): Promise<void> {
  if (lib.files[key]) return Promise.resolve();
  let p = loading.get(key);
  if (!p) {
    setLib('errs', key, undefined);
    p = be().libraryFiles(listQuery(key)).then(f => setLib('files', key, f), e => setLib('errs', key, String(e))).finally(() => loading.delete(key));
    loading.set(key, p);
  }
  return p;
}
async function refreshLib() {
  try { const l = await be().libraryList(); setLib({ kits: l.kits, folders: l.folders, loaded: true, error: null }); if (searching()) loadAll(); }
  catch (e) { setLib({ loaded: true, error: String(e) }); }
}
function toggleOpen(key: string) {
  const open = !lib.open[key];
  setLib('open', key, open);
  if (open) void ensure(key);
}
/** A search looks in everything that is installed, opened before or not. */
function loadAll() {
  for (const k of lib.kits) if (k.installed) void ensure('k:' + k.id);
  for (const f of lib.folders) void ensure('f:' + f);
}
function setQuery(q: string) {
  setLib({ q, sopen: {} });
  if (searching()) loadAll();
}
const matchCount = (key: string) => { const tm = terms(); return (lib.files[key] ?? []).reduce((n, f) => n + (matches(f, tm, libGroup) ? 1 : 0), 0); };
const fileAt = (path: string): LibFile | undefined => { for (const l of Object.values(lib.files)) { const f = l?.find(x => x.path === path); if (f) return f; } };

function startPreview(f: LibFile) {
  setU('previewPath', f.path);
  be().previewFile(f.path).catch(e => say(t('d.br.preverr', { err: String(e) })));
  say(t('d.br.prev', { name: f.name }));
}
function previewFile(f: LibFile) {
  if (U.previewing && U.previewPath === f.path) { void be().previewStop(); setU('previewPath', null); return; }
  startPreview(f);
}

/** ↑ ↓ walk the rows that are drawn (open groups, the search's matches) and play each; Enter makes a sampler channel. */
function onListKey(e: KeyboardEvent) {
  if (e.ctrlKey || e.metaKey || e.altKey || !listEl) return;
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    const rows = [...listEl.querySelectorAll<HTMLElement>('.fitem')];
    const cur = rows.findIndex(r => r.dataset.path === lib.sel);
    e.preventDefault(); e.stopPropagation();
    if (e.key === 'ArrowUp' && cur <= 0) { searchEl?.focus(); return; }
    const i = stepIndex(rows.length, cur, e.key === 'ArrowDown' ? 1 : -1), f = i >= 0 ? fileAt(rows[i].dataset.path!) : undefined;
    if (!f) return;
    setLib('sel', f.path);
    rows[i].scrollIntoView({ block: 'nearest' });
    startPreview(f);
  } else if (e.key === 'Enter' && lib.sel) {
    const f = fileAt(lib.sel); if (!f) return;
    e.preventDefault(); e.stopPropagation();
    void addSamplerFiles([f]);
  }
}

/**
 * Where a library file let go at a client point goes (FL): onto a channel's row in the Channel
 * Rack, or onto a sampler's window = that channel plays it (a sampler keeps its settings, another
 * instrument becomes a sampler; notes and routing stay); elsewhere on the rack = a new sampler
 * channel at the end; anywhere else it is imported, as an audio clip when the point is on a
 * playlist track. One source either way.
 */
function fileTarget(x: number, y: number): { replace?: Channel; row?: HTMLElement; rack?: boolean } {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  const w = el?.closest('.chanwin') ? chanWin() : null, win = w ? chanById(w.id) : undefined;
  if (win?.kind === 'sample') return { replace: win };
  const row = el?.closest<HTMLElement>('.rackwin [data-ch]') ?? undefined, c = row ? chanById(Number(row.dataset.ch)) : undefined;
  if (c && row) return { replace: c, row };
  return { rack: !!(w || el?.closest('.rackwin')) };
}
let marked: HTMLElement | null = null;
/** Outline the rack row a drop would replace. */
function markRow(row: HTMLElement | null | undefined) {
  if (marked === (row ?? null)) return;
  marked?.classList.remove('dropon'); marked = row ?? null; marked?.classList.add('dropon');
}
function fileOver(x: number, y: number): string | null {
  const d = fileTarget(x, y);
  markRow(d.row);
  return d.replace ? t('d.br.dropReplace', { name: d.replace.name }) : d.rack ? t('d.br.dropNewCh') : null;
}
function dropFile(f: LibFile, x: number, y: number, alt: boolean) {
  const d = fileTarget(x, y);
  if (d.replace) return void replaceWithFile(d.replace, f);
  if (d.rack) return void addSamplerFiles([f]);
  void importAt([f.path], x, y, alt);
}

const Chev = (props: { open: boolean }) => (
  <svg class="chev" classList={{ open: props.open }} width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 5l7 7-7 7" /></svg>
);
const Glyph = (props: { g: MenuGlyph; size: number }) => (
  <svg width={props.size} height={props.size} viewBox={props.g.vb} fill="currentColor" aria-hidden="true"><path fill-rule={props.g.eo ? 'evenodd' : 'nonzero'} d={props.g.d} /></svg>
);

function FileRow(props: { f: LibFile; sub: boolean }) {
  const f = props.f, color = GROUP_COLOR.get(f.group);
  return (
    <div class="fitem" data-path={f.path} classList={{ playing: U.previewing && U.previewPath === f.path, sel: lib.sel === f.path, sub: props.sub }} title={t('d.br.file.tip')}
      onContextMenu={e => { e.preventDefault(); setLib('sel', f.path); openMenu(e.clientX, e.clientY, [{ t: 'title', label: f.name }, { t: 'item', label: t('d.br.addsampler'), title: t('d.br.addsampler.tip'), act: () => void addSamplerFiles([f]) }], 90); }}
      onPointerDown={e => {
        if (e.button !== 0) return;
        e.preventDefault(); setLib('sel', f.path); listEl?.focus({ preventScroll: true });
        dragGhost(e, f.name, color ?? '#47b8a8', () => previewFile(f), (x, y, alt) => dropFile(f, x, y, alt), fileOver);
      }}>
      <span class="fico" style={{ color: color ?? 'var(--fg4)' }}><Glyph g={libFileGlyph(f)} size={11} /></span>
      <span class="ell nm">{f.name.replace(/\.[^.]+$/, '')}</span>
      <Show when={f.pitched && f.root != null}><span class="fkey" title={t('d.br.key.tip')}>{noteName(f.root!)}</span></Show>
    </div>
  );
}

function FileList(props: { k: string }) {
  const all = () => lib.files[props.k];
  const groups = createMemo(() => groupFiles(all() ?? [], terms(), libGroup));
  const choices = () => (searching() ? 'sopen' : 'gopen');
  return (
    <Show when={all()} fallback={
      <Show when={lib.errs[props.k]} fallback={<div class="fnote">{t('d.br.loading')}</div>}>
        {err => <div class="fnote err">{t('d.br.liberr', { err: err() })} <button class="linkbtn" onClick={() => void ensure(props.k)}>{t('d.br.retry')}</button></div>}
      </Show>
    }>
      <For each={groups()} fallback={<div class="fnote">{searching() ? t('d.br.nomatch', { q: lib.q.trim() }) : t('d.br.nofiles')}</div>}>
        {g => {
          const key = props.k + '|' + g.name;
          const open = () => !g.name || groupOpen(lib[choices()][key], searching(), all()!.length);
          return (
            <>
              <Show when={g.name}>
                <div class="grow" role="button" aria-expanded={open()} title={t('d.br.group.tip', { n: g.total })} style={{ '--c': GROUP_COLOR.get(g.name) ?? 'var(--fg3)' }}
                  onClick={() => setLib(choices(), key, !open())}>
                  <Chev open={open()} />
                  <span class="gico"><Glyph g={libGroupGlyph(g.name)} size={11} /></span>
                  <span class="ell nm">{libGroup(g.name)}</span>
                  <span class="cnt">{g.total}</span>
                </div>
              </Show>
              <Show when={open()}>
                <For each={g.files}>{f => <FileRow f={f} sub={!!g.name} />}</For>
                <Show when={g.total > g.files.length}><div class="fnote" classList={{ sub: !!g.name }}>{t('d.br.more', { n: g.total - g.files.length })}</div></Show>
              </Show>
            </>
          );
        }}
      </For>
    </Show>
  );
}

function LibraryTab() {
  onMount(() => {
    if (!lib.loaded) void refreshLib();
    void ensure(STD);
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
  const addFolder = async () => { try { setLib('folders', await be().libraryAddFolder()); if (searching()) loadAll(); } catch (e) { say(String(e)); } };
  const removeFolder = async (p: string) => { try { setLib('folders', await be().libraryRemoveFolder(p)); setLib('files', 'f:' + p, undefined); } catch (e) { say(String(e)); } };
  // browsing: every kit and folder; searching: the installed ones the search found something in
  const kits = () => lib.kits.filter(k => k.id !== STD_KIT && (!searching() || (k.installed && matchCount('k:' + k.id) > 0)));
  const folders = () => lib.folders.filter(f => !searching() || matchCount('f:' + f) > 0);
  const stdShown = () => !searching() || !lib.files[STD] || matchCount(STD) > 0;
  const nothing = () => searching() && !!lib.files[STD] && !stdShown() && !kits().length && !folders().length;
  const isOpen = (key: string) => searching() || !!lib.open[key];
  return (
    <>
      <div class="bsearch" title={t('d.br.search.tip')}>
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="M20 20l-3.6-3.6" /></svg>
        <input ref={searchEl} type="text" spellcheck={false} autocomplete="off" value={lib.q} placeholder={t('d.br.search')} aria-label={t('d.br.search')}
          onInput={e => setQuery(e.currentTarget.value)}
          onKeyDown={e => {
            if (e.key === 'Escape') { e.stopPropagation(); if (lib.q) setQuery(''); else e.currentTarget.blur(); }
            else if (e.key === 'ArrowDown') { e.preventDefault(); setLib('sel', null); listEl?.focus({ preventScroll: true }); onListKey(e); }
          }} />
        <Show when={lib.q}><button class="xbtn" title={t('d.br.search.clear')} aria-label={t('d.br.search.clear')} onClick={() => { setQuery(''); searchEl?.focus(); }}>×</button></Show>
      </div>
      <div class="scroll" ref={listEl} tabIndex={0} onKeyDown={onListKey}>
        <Show when={lib.error}><div class="bnote" style={{ color: 'var(--danger)' }}>{lib.error}</div></Show>
        <Show when={nothing()}><div class="bnote">{t('d.br.nomatch', { q: lib.q.trim() })}</div></Show>
        <Show when={stdShown()}>
          <div class="bhead" title={t('d.br.stdlibDesc')}>
            <span class="ell" style={{ 'text-transform': 'uppercase' }}>{t('d.br.stdlib')}</span>
            <Show when={lib.files[STD]}>{f => <span class="hmeta">{searching() ? `${matchCount(STD)} / ${f().length}` : f().length}</span>}</Show>
          </div>
          <div class="flist"><FileList k={STD} /></div>
        </Show>
        <Show when={kits().length}>
          <div class="bhead top">{t('d.br.kits')}</div>
          <For each={kits()}>
            {k => {
              const prog = () => lib.progress[k.id], key = 'k:' + k.id;
              return (
                <div class="kit">
                  <div class="top" classList={{ can: k.installed && !searching() }} role={k.installed ? 'button' : undefined} aria-expanded={k.installed ? isOpen(key) : undefined} onClick={() => k.installed && !searching() && toggleOpen(key)}>
                    <Show when={k.installed} fallback={<span style={{ width: '12px', flex: 'none' }} />}><Chev open={isOpen(key)} /></Show>
                    <span class="nm ell">{k.name}</span>
                    <Show when={!k.installed && !prog()}><button class="smallbtn primary" onClick={e => { e.stopPropagation(); install(k); }}>{t('d.br.install')}</button></Show>
                  </div>
                  <Show when={!searching()}>
                    <span class="desc" style={{ 'padding-left': '20px' }}>{k.description}</span>
                    <span class="meta" style={{ 'padding-left': '20px' }}>{t('d.br.kitmeta', { n: k.count, size: fmtBytes(k.bytes) }) + (k.installed ? t('d.br.kitinst') : '')}</span>
                  </Show>
                  <Show when={prog()}>
                    {p => (
                      <div style={{ 'padding-left': '20px', display: 'flex', 'flex-direction': 'column', gap: '3px' }}>
                        <div class="prog"><div style={{ width: clamp(p().total ? p().done / p().total : 0, 0, 1) * 100 + '%' }} /></div>
                        <span class="meta">{p().error ? p().error : t('d.br.downloading', { n: Math.round(p().total ? p().done / p().total * 100 : 0) })}</span>
                      </div>
                    )}
                  </Show>
                  <Show when={k.installed && isOpen(key)}><div class="flist in"><FileList k={key} /></div></Show>
                </div>
              );
            }}
          </For>
        </Show>
        <Show when={!searching() || folders().length}>
          <div class="bhead top">
            <span>{t('d.br.folders')}</span>
            <button class="linkbtn" onClick={addFolder} title={t('d.br.addfolder.tip')}>{t('d.br.add')}</button>
          </div>
          <For each={folders()} fallback={<div class="bnote">{t('d.br.addown')}</div>}>
            {f => (
              <>
                <div class="folder" role="button" aria-expanded={isOpen('f:' + f)} onClick={() => !searching() && toggleOpen('f:' + f)} title={f}>
                  <Chev open={isOpen('f:' + f)} />
                  <span class="ell" style={{ flex: 1 }}>{baseName(f) || f}</span>
                  <button class="xbtn" title={t('d.br.rmfolder')} onClick={e => { e.stopPropagation(); void removeFolder(f); }}>×</button>
                </div>
                <Show when={isOpen('f:' + f)}><div class="flist"><FileList k={'f:' + f} /></div></Show>
              </>
            )}
          </For>
        </Show>
        <div class="bnote">{t('d.br.libnote')}</div>
      </div>
    </>
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
