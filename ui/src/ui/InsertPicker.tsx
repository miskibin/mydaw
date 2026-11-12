// The MIXER picker in the header of the sample and channel windows (FL's FX-track box): a list of
// the mixer inserts, and the mouse wheel over it steps through them (down = next), one undo
// step per burst of wheel ticks.
import { For, Show } from 'solid-js';
import type { Id, Op } from '../api/types';
import { t } from '../lib/i18n';
import { commit, wheelEdit } from '../state/edit';
import { insName, S } from '../state/store';
import { say, setU } from '../state/ui';

export function InsertPicker(props: {
  /** The insert chosen, or null for `nullLabel` (e.g. "like the clip's track"). */
  value: Id | null;
  /** When given, null is a choice too, listed first. */
  nullLabel?: string;
  /** The ops that route to `id`. */
  ops: (id: Id | null) => Op[];
  /** Undo label. */
  label: string;
  /** Wheel gesture key (one per window). */
  wheelKey: string;
  title: string;
  /** Master can be picked (a track can play into it; a channel's null already means "its track"). */
  master?: boolean;
}) {
  const ids = (): (Id | null)[] => [...(props.nullLabel != null ? [null] : []), ...S.project.inserts.filter(m => props.master || m.id !== 0).map(m => m.id)];
  const nameOf = (id: Id | null) => { const m = id == null ? undefined : S.project.inserts.find(x => x.id === id); return m ? insName(m) : props.nullLabel ?? ''; };
  const show = (id: Id | null) => { if (id != null) setU('mixSel', id); };
  const pick = (id: Id | null) => { commit(props.ops(id), props.label); show(id); say(t('d.smp.routedTo', { name: nameOf(id) })); };
  const wheel = (e: WheelEvent) => {
    e.preventDefault();
    if (!e.deltaY) return;
    const l = ids(), i = l.indexOf(props.value), j = Math.min(l.length - 1, Math.max(0, i + (e.deltaY > 0 ? 1 : -1)));
    if (i < 0 || j === i) return;
    wheelEdit(props.wheelKey, props.label, props.ops(l[j]));
    show(l[j]);
    say(t('d.smp.routedTo', { name: nameOf(l[j]) }));
  };
  return (
    <>
      <span class="tag9">MIXER</span>
      <select class="sel inspick" value={props.value == null ? '' : String(props.value)} title={props.title}
        onChange={e => pick(e.currentTarget.value === '' ? null : +e.currentTarget.value)} onWheel={wheel}>
        <Show when={props.nullLabel != null}><option value="">{props.nullLabel}</option></Show>
        <For each={S.project.inserts.filter(m => props.master || m.id !== 0)}>{m => <option value={String(m.id)}>{insName(m)}</option>}</For>
      </select>
    </>
  );
}
