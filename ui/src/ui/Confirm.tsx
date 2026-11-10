// "Unsaved changes" question before New / Open / closing the window.
import { createSignal, Show } from 'solid-js';
import { t } from '../lib/i18n';
import { S } from '../state/store';

type Answer = 'save' | 'discard' | 'cancel';
const [ask, setAsk] = createSignal<{ what: string; done: (a: Answer) => void } | null>(null);

/** Resolves 'discard' straight away when nothing is unsaved. */
export function confirmUnsaved(what: string): Promise<Answer> {
  if (!S.dirty) return Promise.resolve('discard');
  return new Promise(res => setAsk({ what, done: a => { setAsk(null); res(a); } }));
}

export function ConfirmDialog() {
  return (
    <Show when={ask()}>
      {q => (
        <div class="overlay" style={{ 'z-index': 300 }} onPointerDown={() => q().done('cancel')}>
          <div class="dialog" style={{ width: 'min(420px,92vw)' }} onPointerDown={e => e.stopPropagation()}
            onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') q().done('cancel'); if (e.key === 'Enter') q().done('save'); }}>
            <div class="hd"><span>{t('d.ui.unsaved')}</span><button onClick={() => q().done('cancel')}>×</button></div>
            <p style={{ margin: '4px 0 16px', color: 'var(--fg2)', 'font-size': '13px' }}>
              {t('d.ui.unsaved.body', { name: S.path ? t('d.ui.named', { name: S.path.split(/[\\/]/).pop()! }) : t('d.ui.unnamed'), what: q().what })}
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
  );
}
