// Update dialog, "What's new" dialog, the status-bar button and the Settings → Updates rows.
import { createMemo, For, Match, Show, Switch } from 'solid-js';
import { lang, t } from '../lib/i18n';
import { parseNotes, type Block, type Inline } from '../lib/changelog';
import type { Release } from '../lib/changelog';
import { checkUpdates, closeUpdateDialog, closeWhatsNew, installUpdate, openReleasePage, openUpdateDialog, setAutoUpdate, showWhatsNew, startUpdate, upd } from '../state/updates';

const mb = (n: number) => (n / 1e6).toFixed(n >= 1e8 ? 0 : 1);

function Inl(p: { parts: Inline[] }) {
  return <For each={p.parts}>{x => x.t === 'b' ? <b>{x.s}</b> : x.t === 'code' ? <code>{x.s}</code> : x.s}</For>;
}

type Group = { t: 'h'; s: string } | { t: 'ul'; items: Inline[][] } | { t: 'p'; inl: Inline[] };
const group = (blocks: Block[]): Group[] => {
  const out: Group[] = [];
  for (const b of blocks) {
    if (b.t === 'li') { const last = out[out.length - 1]; if (last?.t === 'ul') last.items.push(b.inl); else out.push({ t: 'ul', items: [b.inl] }); } else out.push(b);
  }
  return out;
};

/** Release notes: headings, bullets, **bold** and `code`. Built from text nodes only, never from HTML. */
function Notes(p: { md: string | null }) {
  const groups = createMemo(() => group(parseNotes(p.md ?? '')));
  return (
    <div class="notes">
      <Show when={groups().length} fallback={<p class="muted">{t('d.upd.nonotes')}</p>}>
        <For each={groups()}>{g => g.t === 'h' ? <h4>{g.s}</h4> : g.t === 'ul' ? <ul><For each={g.items}>{i => <li><Inl parts={i} /></li>}</For></ul> : <p><Inl parts={g.inl} /></p>}</For>
      </Show>
    </div>
  );
}

const pct = () => { const s = upd(); return s.total > 0 ? Math.min(100, Math.floor(s.done * 100 / s.total)) : null; };

/** Status-bar button while an update is found, downloading or waiting for the restart. */
export function UpdateChip() {
  const s = () => upd();
  const label = () => {
    switch (s().phase) {
      case 'available': return t('d.upd.chip.available', { v: s().info?.version ?? '' });
      case 'downloading': return pct() == null ? t('d.upd.chip.downloading0') : t('d.upd.chip.downloading', { pct: pct()! });
      case 'ready': return t('d.upd.chip.ready');
      default: return t('d.upd.chip.installing');
    }
  };
  return (
    <Show when={s().phase !== 'idle' && s().phase !== 'checking'}>
      <button class="updchip" classList={{ ready: s().phase === 'ready', busy: s().phase === 'downloading' || s().phase === 'installing' }} title={t('d.upd.chip.tip')}
        style={{ '--p': (pct() ?? 0) + '%' }} onClick={() => openUpdateDialog()}>{label()}</button>
    </Show>
  );
}

function ReleaseLink(p: { version: string | null }) {
  return <button class="smallbtn" title={t('d.upd.btn.page.tip')} onClick={() => void openReleasePage(p.version)}>{t('d.upd.btn.page')}</button>;
}

export function Updates() {
  return <><UpdateDialog /><WhatsNewDialog /></>;
}

function UpdateDialog() {
  const s = () => upd();
  const open = () => s().dialog && (s().phase === 'available' || s().phase === 'downloading' || s().phase === 'ready' || s().phase === 'installing');
  return (
    <Show when={open()}>
      <div class="overlay" style={{ 'z-index': 250 }} onPointerDown={() => closeUpdateDialog()}>
        <div class="dialog upddlg" role="dialog" aria-modal="true" aria-label={t('d.upd.title.available')} onPointerDown={e => e.stopPropagation()}
          onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') closeUpdateDialog(); }}>
          <div class="hd"><span>{s().phase === 'ready' || s().phase === 'installing' ? t('d.upd.title.ready') : t('d.upd.title.available')}</span><button aria-label={t('d.upd.btn.close')} onClick={() => closeUpdateDialog()}>×</button></div>
          <div class="updver">
            <b>{t('d.upd.versions', { v: s().info?.version ?? '', cur: s().current })}</b>
            <Show when={s().info?.date}><span>{s().info!.date}</span></Show>
          </div>
          <Notes md={s().info?.notes ?? null} />
          <Switch>
            <Match when={s().phase === 'downloading'}>
              <div class="updbar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct() ?? undefined}><i style={{ width: (pct() ?? 0) + '%' }} /></div>
              <div class="updmsg">{s().total > 0 ? t('d.upd.dl', { done: mb(s().done), total: mb(s().total) }) : t('d.upd.dl.unknown', { done: mb(s().done) })}</div>
              <div class="updmsg muted">{t('d.upd.dl.keep')}</div>
            </Match>
            <Match when={s().phase === 'ready'}><div class="updmsg">{t('d.upd.ready.body')}</div></Match>
            <Match when={s().phase === 'installing'}><div class="updmsg">{t('d.upd.installing')}</div></Match>
          </Switch>
          <Show when={s().error}><div class="updmsg err" role="alert">{t('d.upd.fail', { err: s().error! })}</div></Show>
          <div class="updfoot">
            <ReleaseLink version={s().info?.version ?? null} />
            <div style={{ flex: 1 }} />
            <Show when={s().phase !== 'installing'}>
              <button class="smallbtn" onClick={() => closeUpdateDialog()}>{s().phase === 'downloading' ? t('d.upd.btn.close') : t('d.upd.btn.later')}</button>
            </Show>
            <Switch>
              <Match when={s().phase === 'available'}>
                <button class="smallbtn primary" ref={el => setTimeout(() => el.focus())} onClick={() => void startUpdate()}>{s().error ? t('d.upd.btn.retry') : t('d.upd.btn.download')}</button>
              </Match>
              <Match when={s().phase === 'ready'}>
                <button class="smallbtn primary" ref={el => setTimeout(() => el.focus())} onClick={() => void installUpdate()}>{s().error ? t('d.upd.btn.retry') : t('d.upd.btn.restart')}</button>
              </Match>
            </Switch>
          </div>
        </div>
      </div>
    </Show>
  );
}

function WhatsNewDialog() {
  return (
    <Show when={upd().news}>
      {news => (
        <div class="overlay" style={{ 'z-index': 250 }} onPointerDown={() => void closeWhatsNew()}>
          <div class="dialog upddlg" role="dialog" aria-modal="true" aria-label={t('d.news.title')} onPointerDown={e => e.stopPropagation()}
            onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape' || e.key === 'Enter') void closeWhatsNew(); }}>
            <div class="hd"><span>{t('d.news.title')}</span><button aria-label={t('d.upd.btn.close')} onClick={() => void closeWhatsNew()}>×</button></div>
            <For each={news()}>{(r: Release) => (
              <section class="newsrel">
                <div class="updver"><b>{t('d.news.ver', { v: r.version })}</b><Show when={r.date}><span>{r.date}</span></Show></div>
                <Notes md={r.notes} />
              </section>
            )}</For>
            <Show when={lang() === 'pl'}><div class="updmsg muted">{t('d.news.enonly')}</div></Show>
            <div class="updfoot">
              <ReleaseLink version={news()[0]?.version ?? null} />
              <div style={{ flex: 1 }} />
              <button class="smallbtn primary" ref={el => setTimeout(() => el.focus())} onClick={() => void closeWhatsNew()}>{t('d.news.ok')}</button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}

/** Settings → General → Updates. `OnOff` is the Settings dialog's own switch. */
export function UpdatesSettings(p: { OnOff: (a: { on: () => boolean; set: (v: boolean) => void; title?: string }) => unknown }) {
  const s = () => upd();
  return (
    <>
      <div class="sechd">{t('d.upd.sec')}</div>
      <div class="form">
        <label>{t('d.upd.version')}</label>
        <div class="updrow"><b>{s().current || '—'}</b><button class="smallbtn" onClick={() => showWhatsNew()}>{t('d.upd.whatsnew')}</button></div>
        <label>{t('d.upd.auto')}</label>
        {p.OnOff({ on: () => s().autoUpdate, set: v => void setAutoUpdate(v), title: t('d.upd.auto.tip') }) as never}
        <label />
        <div class="updrow">
          <button class="smallbtn" disabled={s().phase !== 'idle' && s().phase !== 'available'} onClick={() => void checkUpdates(true)}>{s().phase === 'checking' ? t('d.upd.checking') : t('d.upd.check')}</button>
          <Switch>
            <Match when={s().phase === 'available'}>
              <span>{t('d.upd.found', { v: s().info?.version ?? '' })}</span><button class="smallbtn primary" onClick={() => openUpdateDialog()}>{t('d.upd.view')}</button>
            </Match>
            <Match when={s().phase === 'downloading' || s().phase === 'ready' || s().phase === 'installing'}>
              <button class="smallbtn primary" onClick={() => openUpdateDialog()}>{t('d.upd.view')}</button>
            </Match>
            <Match when={s().error}><span class="err" role="alert">{t('d.upd.err', { err: s().error! })}</span></Match>
            <Match when={s().upToDateAt != null}><span class="muted">{t('d.upd.uptodate')}</span></Match>
          </Switch>
        </div>
      </div>
    </>
  );
}
