// "Unsaved changes" question before New / Open / closing the window, and the offer to
// recover an unsaved project after a crash.
import { createSignal, Show } from 'solid-js';
import { lang, t } from '../lib/i18n';
import { S } from '../state/store';

type Answer = 'save' | 'discard' | 'cancel';
const [ask, setAsk] = createSignal<{ what: string; done: (a: Answer) => void } | null>(null);

/** Resolves 'discard' straight away when nothing is unsaved. */
export function confirmUnsaved(what: string): Promise<Answer | 'busy'> {
  if (ask()) return Promise.resolve('busy'); // keep the first confirmation actionable
  if (!S.dirty) return Promise.resolve('discard');
  return new Promise(res => setAsk({ what, done: a => { setAsk(null); res(a); } }));
}

export type RecoverAnswer = 'restore' | 'discard' | 'later';
const [askRec, setAskRec] = createSignal<{ savedMs: number; openPath: string | null; done: (a: RecoverAnswer) => void } | null>(null);

/**
 * "WAVE closed unexpectedly…": restore the project, delete it, or keep it for the next start.
 * `openPath`: the project file open now (e.g. the one just opened from Explorer), which a
 * restore replaces in this window; the question says so.
 */
export function confirmRecover(savedMs: number, openPath: string | null = null): Promise<RecoverAnswer> {
  return new Promise(res => setAskRec({ savedMs, openPath, done: a => { setAskRec(null); res(a); } }));
}

const fileName = (path: string) => path.split(/[\\/]/).pop()!;
const when = (ms: number) => new Date(ms).toLocaleString(lang() === 'pl' ? 'pl-PL' : 'en-GB', { dateStyle: 'medium', timeStyle: 'short' });

export function ConfirmDialog() {
  return (<>
    <Show when={askRec()}>
      {q => (
        <div class="overlay" style={{ 'z-index': 300 }}>
          <div class="dialog" role="alertdialog" aria-labelledby="rec-hd" style={{ width: 'min(440px,92vw)' }}
            onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') q().done('later'); if (e.key === 'Enter') q().done('restore'); }}>
            <div class="hd"><span id="rec-hd">{t('d.ui.recover')}</span><button title={t('d.ui.recover.later.tip')} onClick={() => q().done('later')}>×</button></div>
            <p style={{ margin: '4px 0 16px', color: 'var(--fg2)', 'font-size': '13px' }}>
              {t('d.ui.recover.body', { when: when(q().savedMs) })}
              <Show when={q().openPath}>{p => <> {t('d.ui.recover.replaces', { name: t('d.ui.named', { name: fileName(p()) }) })}</>}</Show>
            </p>
            <div class="row" style={{ gap: '8px', 'justify-content': 'flex-end' }}>
              <button class="smallbtn" title={t('d.ui.recover.later.tip')} onClick={() => q().done('later')}>{t('d.ui.recover.later')}</button>
              <button class="smallbtn danger" onClick={() => q().done('discard')}>{t('d.ui.recover.discard')}</button>
              <button class="smallbtn primary" ref={el => setTimeout(() => el.focus())} onClick={() => q().done('restore')}>{t('d.ui.recover.restore')}</button>
            </div>
          </div>
        </div>
      )}
    </Show>
    <Show when={ask()}>
      {q => (
        <div class="overlay" style={{ 'z-index': 300 }} onPointerDown={() => q().done('cancel')}>
          <div class="dialog" style={{ width: 'min(420px,92vw)' }} onPointerDown={e => e.stopPropagation()}
            onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') q().done('cancel'); if (e.key === 'Enter') q().done('save'); }}>
            <div class="hd"><span>{t('d.ui.unsaved')}</span><button onClick={() => q().done('cancel')}>×</button></div>
            <p style={{ margin: '4px 0 16px', color: 'var(--fg2)', 'font-size': '13px' }}>
              {t('d.ui.unsaved.body', { name: S.path ? t('d.ui.named', { name: fileName(S.path) }) : t('d.ui.unnamed'), what: q().what })}
            </p>
            <div class="row" style={{ gap: '8px', 'justify-content': 'flex-end' }}>
              <button class="smallbtn" onClick={() => q().done('cancel')}>{t('d.ui.cancel')}</button>
              <button class="smallbtn danger" onClick={() => q().done('discard')}>{t('d.ui.discard')}</button>
              <button class="smallbtn primary" ref={el => setTimeout(() => el.focus())} onClick={() => q().done('save')}>{t('d.ui.save')}</button>
            </div>
          </div>
        </div>
      )}
    </Show>
  </>);
}
