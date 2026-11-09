// "Unsaved changes" question before New / Open / closing the window.
import { createSignal, Show } from 'solid-js';
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
            <div class="hd"><span>Niezapisane zmiany</span><button onClick={() => q().done('cancel')}>×</button></div>
            <p style={{ margin: '4px 0 16px', color: 'var(--fg2)', 'font-size': '13px' }}>
              Projekt {S.path ? '„' + S.path.split(/[\\/]/).pop() + '”' : 'bez nazwy'} ma niezapisane zmiany. Zapisać przed: {q().what}?
            </p>
            <div class="row" style={{ gap: '8px', 'justify-content': 'flex-end' }}>
              <button class="smallbtn" onClick={() => q().done('cancel')}>Anuluj</button>
              <button class="smallbtn danger" onClick={() => q().done('discard')}>Nie zapisuj</button>
              <button class="smallbtn primary" ref={el => setTimeout(() => el.focus())} onClick={() => q().done('save')}>Zapisz</button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
