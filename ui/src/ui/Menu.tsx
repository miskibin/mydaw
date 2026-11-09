import { For, Match, Show, Switch, onMount } from 'solid-js';
import { closeMenu, U } from '../state/ui';
import { S } from '../state/store';
import { commit } from '../state/edit';
import { clamp, dbText } from '../lib/format';
import { Icon, TC } from './icons';
import { INST_ICONS, InstIcon } from './instIcons';

/** The one context menu (items are built by whoever opens it). */
export function ContextMenu() {
  return (
    <Show when={U.menu}>
      {menu => (
        <>
          <div class="backdrop" onPointerDown={closeMenu} onContextMenu={e => e.preventDefault()} />
          <div class="menu" style={{ left: menu().x + 'px', top: menu().y + 'px' }} onPointerDown={e => e.stopPropagation()}>
            <For each={menu().items}>
              {it => (
                <Switch>
                  <Match when={it.t === 'title' && it}>{i => <div class="mt">{i().label}</div>}</Match>
                  <Match when={it.t === 'sep'}><div class="ms" /></Match>
                  <Match when={it.t === 'item' && it}>
                    {i => (
                      <button class="mi" classList={{ danger: !!i().danger, dis: !!i().disabled }} title={i().title ?? ''}
                        onClick={() => { if (i().disabled) return; closeMenu(); i().act(); }}>
                        <span class="row" style={{ gap: '8px' }}>
                          <Show when={i().icon}>
                            <span class="tile" style={{ width: '18px', height: '18px', 'border-radius': '4px', background: (i().iconColor ?? '#9aa2ad') + '26', color: i().iconColor ?? 'var(--fg3)' }}>
                              <Icon d={i().icon!} size={12} sw={2.2} />
                            </span>
                          </Show>
                          <span>{i().label}</span>
                        </span>
                        <span class="kbd">{i().kbd ?? ''}</span>
                      </button>
                    )}
                  </Match>
                  <Match when={it.t === 'swatches' && it}>
                    {i => (
                      <div class="swatches">
                        <For each={TC}>{c => <button title="Kolor" style={{ background: c, 'box-shadow': c === i().current ? '0 0 0 2px var(--fg-hi)' : 'none' }} onClick={() => { i().pick(c); closeMenu(); }} />}</For>
                      </div>
                    )}
                  </Match>
                  <Match when={it.t === 'icons' && it}>
                    {i => (
                      <div class="iconpick">
                        <For each={Object.entries(INST_ICONS)}>{([id, g]) => (
                          <button title={g.label + (id === i().current ? ' (klik = usuń)' : '')} classList={{ on: id === i().current }}
                            style={{ color: id === i().current ? (i().color ?? 'var(--fg-hi)') : undefined }}
                            onClick={() => { i().pick(id === i().current ? null : id); closeMenu(); }}>
                            <InstIcon id={id} size={14} />
                          </button>
                        )}</For>
                      </div>
                    )}
                  </Match>
                  <Match when={it.t === 'gain' && it}>
                    {i => {
                      const clip = () => S.project.clips.find(c => c.id === i().clipId);
                      const set = (d: number) => { const c = clip(); if (c) commit([{ op: 'putClips', clips: [{ ...c, gain: clamp(c.gain + d, -24, 12) }] }], 'Głośność klipu'); };
                      return (
                        <div class="gainrow">
                          <span style={{ flex: 1, font: '500 12px var(--sans)', color: 'var(--fg)' }}>Głośność klipu</span>
                          <button onClick={() => set(-1)}>−</button>
                          <span style={{ 'min-width': '54px', 'text-align': 'center', font: '500 11px var(--mono)', color: 'var(--blue)' }}>{dbText(clip()?.gain ?? 0)}</span>
                          <button onClick={() => set(1)}>+</button>
                        </div>
                      );
                    }}
                  </Match>
                  <Match when={it.t === 'chips' && it}>
                    {i => (
                      <div class="chiprow" classList={{ dis: !!i().disabled }}>
                        <Show when={i().label}><span class="cl">{i().label}</span></Show>
                        <div class="chips">
                          <For each={i().options}>{(o, k) => (
                            <button classList={{ on: i().active === k() }} disabled={i().disabled}
                              onClick={() => { if (i().disabled) return; if (i().close !== false) closeMenu(); i().pick(k()); }}>{o}</button>
                          )}</For>
                        </div>
                      </div>
                    )}
                  </Match>
                  <Match when={it.t === 'rename' && it}>
                    {i => {
                      let el!: HTMLInputElement;
                      onMount(() => { el.value = i().value; setTimeout(() => { el.focus(); el.select(); }, 0); });
                      return <input ref={el} onKeyDown={e => {
                        e.stopPropagation();
                        if (e.key === 'Enter') { const v = el.value.trim(); closeMenu(); if (v) i().done(v); }
                        if (e.key === 'Escape') closeMenu();
                      }} />;
                    }}
                  </Match>
                </Switch>
              )}
            </For>
          </div>
        </>
      )}
    </Show>
  );
}
