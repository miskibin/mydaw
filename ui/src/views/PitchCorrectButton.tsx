// "Pitch Correct" on a mixer insert, from the windows that edit one sound (pitch editor, sample window,
// sampler channel): the real-time tuner (Auto-Tune style) goes on that sound's insert — or, when it is
// already there, its window opens. Master and "no own insert" are refused with the reason, because a
// tuner on the whole mix (or on a shared insert) would tune everything else too.
import { Show } from 'solid-js';
import type { Id } from '../api/types';
import { t } from '../lib/i18n';
import * as A from '../state/actions';
import { insName, S } from '../state/store';
import { FXMETA, Icon } from '../ui/icons';

export function PitchCorrectButton(props: { insert: () => Id | null; keyScale?: () => { key: number; scale: number } }) {
  const ins = () => { const id = props.insert(); return id == null ? null : S.project.inserts.find(m => m.id === id) ?? null; };
  const on = () => ins()?.fx.find(f => f.type === 'tune');
  const why = () => ins() == null ? t('d.pc.noInsert') : ins()!.id === 0 ? t('d.pc.master') : null;
  const click = () => {
    const m = ins(); if (!m || m.id === 0) return;
    const f = on();
    if (f) { A.openPlug(f.id); return; }
    A.addPitchCorrect(m.id, props.keyScale?.());
  };
  return (
    <button class="smallbtn pcbtn" disabled={!!why()} onClick={click}
      title={why() ?? (on() ? t('d.pc.open', { name: insName(ins()!) }) : t('d.pc.add', { name: insName(ins()!) }))}>
      <Icon d={FXMETA.tune.icon} size={12} />
      <span>{t('d.pc.label')}</span>
      <Show when={on()}><span class="pcon" style={{ background: FXMETA.tune.color }} /></Show>
    </button>
  );
}
