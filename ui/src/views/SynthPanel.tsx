// The built-in synth's page in the channel window: a preset bar (category → preset, ‹ ›) and the
// parameter sections as compact knob groups. Choices with few options render as segmented
// buttons, long lists as a select. Edits are `putChannel` (engine fast path, held notes follow).
import { createMemo, createSignal, For, Show } from 'solid-js';
import type { Channel, ParamDef, SynthCatalog } from '../api/types';
import { pFmt, pNorm, pVal } from '../lib/presets';
import { clampSynth, synthValue, withSynthParam, withSynthPreset } from '../lib/synth';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { chanById, chanOp } from '../state/pattern';
import { S } from '../state/store';
import { openMenu, say, type MenuItem } from '../state/ui';
import { Knob, Segmented } from '../ui/Knob';

/** Sections shown by default; the rest open with "Więcej". */
const MAIN = new Set(['osc1', 'osc2', 'subnoise', 'filter', 'ampenv', 'fx']);

export function SynthPanel(props: { c: Channel }) {
  const cat = () => S.catalog?.synth as SynthCatalog | undefined;
  const [more, setMore] = createSignal(false);
  const live = () => chanById(props.c.id) ?? props.c;
  const presetName = () => (live().kind === 'synth' ? (live() as Channel & { kind: 'synth' }).synth.preset : null);
  const list = createMemo(() => cat()?.presets ?? []);
  const step = (d: number) => {
    const c = cat(); if (!c || !list().length) return;
    const i = list().findIndex(p => p.name === presetName());
    const n = list()[(i + d + list().length) % list().length];
    commit([chanOp(withSynthPreset(c, live(), n.name))], 'Preset syntezatora'); say('Preset: ' + n.name);
  };
  const pick = (x: number, y: number) => {
    const c = cat(); if (!c) return;
    const items: MenuItem[] = [{ t: 'title', label: 'Presety syntezatora' }];
    for (const g of c.categories) {
      const ps = c.presets.filter(p => p.category === g);
      if (!ps.length) continue;
      items.push({ t: 'sub', label: g, items: ps.map(p => ({ t: 'item', label: p.name, kbd: p.name === presetName() ? '●' : '', act: () => { commit([chanOp(withSynthPreset(c, live(), p.name))], 'Preset syntezatora'); say('Preset: ' + p.name); } }) as MenuItem) });
    }
    openMenu(x, y, items, 360);
  };
  return (
    <Show when={cat()} fallback={<div class="note" style={{ padding: '14px' }}>Katalog syntezatora niedostępny.</div>}>
      {c => (
        <div class="synthpanel">
          <div class="synthbar">
            <button class="arrow" title="Poprzedni preset" onClick={() => step(-1)}>‹</button>
            <button class="synthpreset" onClick={e => { const r = e.currentTarget.getBoundingClientRect(); pick(r.left, r.bottom + 4); }}>
              <span class="ell">{presetName() ?? 'Własny dźwięk'}</span>
              <span class="cat">{list().find(p => p.name === presetName())?.category ?? ''}</span>
            </button>
            <button class="arrow" title="Następny preset" onClick={() => step(1)}>›</button>
            <div style={{ flex: 1 }} />
            <button class="smallbtn" classList={{ primary: more() }} onClick={() => setMore(!more())} title="Pokaż LFO, obwiednię modulacji, matrycę i głosy">{more() ? 'Mniej' : 'Więcej'}</button>
          </div>
          <div class="synthsecs">
            <For each={c().sections.filter(s => more() || MAIN.has(s.id))}>{sec => (
              <div class="synthsec">
                <div class="sechd">{sec.name}</div>
                <div class="synthknobs">
                  <For each={sec.params}>{p => <SynthParam cat={c()} p={p} c={props.c} />}</For>
                </div>
              </div>
            )}</For>
          </div>
        </div>
      )}
    </Show>
  );
}

function SynthParam(props: { cat: SynthCatalog; p: ParamDef; c: Channel }) {
  const live = () => chanById(props.c.id) ?? props.c;
  const v = () => (live().kind === 'synth' ? synthValue(props.cat, (live() as Channel & { kind: 'synth' }).synth, props.p.id) : props.p.def);
  const op = (x: number) => chanOp(withSynthParam(props.cat, live(), props.p.id, clampSynth(props.p, x)));
  const p = props.p;
  if (p.options) {
    if (p.options.length <= 5) return (
      <div class="synthchoice"><span class="lab">{p.label}</span>
        <Segmented small options={p.options} value={() => Math.round(v())} pick={i => commit([op(i)], p.label)} />
      </div>
    );
    return (
      <div class="synthchoice"><span class="lab">{p.label}</span>
        <select class="sel" value={String(Math.round(v()))} onChange={e => commit([op(+e.currentTarget.value)], p.label)}>
          <For each={p.options}>{(o, i) => <option value={String(i())}>{o}</option>}</For>
        </select>
      </div>
    );
  }
  let g: Gesture | null = null;
  return (
    <Knob key={'syn' + props.c.id + p.id} label={p.label} size="sm" t={() => pNorm(p, v())} text={() => pFmt(p, v())}
      zero={() => (p.min < 0 && p.max > 0 && !p.log ? pNorm(p, 0) : 0)}
      title="Przeciągnij ↕ · Shift = precyzyjnie · 2× klik = domyślna"
      onStart={() => { g = new Gesture(p.label); }}
      onChange={t => { g?.update([op(pVal(p, t))]); say(p.label + ' ' + pFmt(p, pVal(p, t))); }}
      onEnd={() => { g?.end(); g = null; }}
      onReset={() => commit([op(p.def)], p.label)}
      onWheel={t => wheelEdit('syn' + p.id, p.label, [op(pVal(p, t))])} />
  );
}
