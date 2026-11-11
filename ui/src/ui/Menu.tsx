import { createSignal, For, Match, Show, Switch, onMount } from 'solid-js';
import { unwrap } from 'solid-js/store';
import type { MenuItem } from '../state/ui';
import { CHECK_GLYPH, menuGlyph, type MenuGlyph } from './menuIcons';
import { closeMenu, U } from '../state/ui';
import { S } from '../state/store';
import { commit } from '../state/edit';
import { clamp, dbText } from '../lib/format';
import { t } from '../lib/i18n';
import { Icon, TC } from './icons';
import { INST_ICONS, InstIcon } from './instIcons';

/** The one context menu (items are built by whoever opens it). */
export function ContextMenu() {
  return (
    <Show when={U.menu}>
      {menu => (
        <>
          <div class="backdrop" onPointerDown={closeMenu} onContextMenu={e => e.preventDefault()} />
          <MenuList items={menu().items} x={menu().x} y={menu().y} />
        </>
      )}
    </Show>
  );
}

/** One menu panel; `sub` items open a nested panel on hover. */
function MenuList(props: { items: MenuItem[]; x: number; y: number; sub?: boolean }) {
  const [open, setOpen] = createSignal<{ i: number; x: number; y: number } | null>(null);
  let el!: HTMLDivElement;
  const [pos, setPos] = createSignal({ x: props.x, y: props.y });
  onMount(() => {
    if (!props.sub) return;
    const r = el.getBoundingClientRect();
    setPos({ x: props.x, y: Math.max(4, Math.min(props.y, window.innerHeight - r.height - 4)) });
  });
  const openSub = (i: number, b: HTMLElement) => {
    const r = b.getBoundingClientRect(), W = 230;
    setOpen({ i, x: r.right + W > window.innerWidth ? r.left - W : r.right - 2, y: r.top - 5 });
  };
  const glyph = (it: { label: string; glyph?: MenuGlyph; checked?: boolean }) => it.checked != null ? (it.checked ? CHECK_GLYPH : null) : it.glyph ?? menuGlyph(it.label);
  return (
    <>
          <div ref={el} class="menu" classList={{ submenu: !!props.sub }} style={{ left: pos().x + 'px', top: pos().y + 'px' }} onPointerDown={e => e.stopPropagation()}>
            <For each={props.items}>
              {it => (
                <Switch>
                  <Match when={it.t === 'title' && it}>{i => <div class="mt">{i().label}</div>}</Match>
                  <Match when={it.t === 'sep'}><div class="ms" /></Match>
                  <Match when={it.t === 'sec' && it}>{i => <div class="msec">{i().label}</div>}</Match>
                  <Match when={it.t === 'sub' && it}>
                    {i => {
                      const idx = props.items.indexOf(i());
                      return (
                        <button class="mi" classList={{ hot: open()?.i === idx }} onPointerEnter={e => openSub(idx, e.currentTarget)} onClick={e => openSub(idx, e.currentTarget)}>
                          <span class="row" style={{ gap: '8px' }}><MenuIco g={glyph(i())} /><span>{i().label}</span></span>
                          <span class="kbd">›</span>
                        </button>
                      );
                    }}
                  </Match>
                  <Match when={it.t === 'item' && it}>
                    {i => (
                      <button class="mi" role={i().checked != null ? 'menuitemcheckbox' : 'menuitem'} aria-checked={i().checked} classList={{ danger: !!i().danger, dis: !!i().disabled }} title={i().title ?? ''} onPointerEnter={() => setOpen(null)}
                        onClick={() => { if (i().disabled) return; closeMenu(); i().act(); }}>
                        <span class="row" style={{ gap: '8px' }}>
                          <Show when={i().icon} fallback={<MenuIco g={glyph(i())} />}>
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
                        <For each={TC}>{c => <button title={t('d.ui.color')} style={{ background: c, 'box-shadow': c === i().current ? '0 0 0 2px var(--fg-hi)' : 'none' }} onClick={() => { i().pick(c); closeMenu(); }} />}</For>
                      </div>
                    )}
                  </Match>
                  <Match when={it.t === 'icons' && it}>
                    {i => (
                      <div class="iconpick">
                        <For each={Object.entries(INST_ICONS)}>{([id, g]) => (
                          <button title={id === i().current ? t('d.ui.iconremove', { label: g.label }) : g.label} classList={{ on: id === i().current }}
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
                      // a clip of the selection: the whole selection gets the same step (one undo step)
                      const set = (d: number) => {
                        const c = clip(); if (!c) return;
                        const cs = U.sel.includes(c.id) ? S.project.clips.filter(x => U.sel.includes(x.id)) : [c];
                        commit([{ op: 'putClips', clips: cs.map(x => ({ ...unwrap(x), gain: clamp(x.gain + d, -24, 12) })) }], t('d.ui.clipvol'));
                      };
                      return (
                        <div class="gainrow">
                          <span style={{ flex: 1, font: '500 12px var(--sans)', color: 'var(--fg)' }}>{t('d.ui.clipvol')}</span>
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
          {/* keyed: hovering another submenu row must swap the panel, not keep the first one */}
          <Show when={open()} keyed>{o => {
            const it = props.items[o.i];
            return it && it.t === 'sub' ? <MenuList items={it.items} x={o.x} y={o.y} sub /> : null;
          }}</Show>
    </>
  );
}

function MenuIco(props: { g: MenuGlyph | null }) {
  return (
    <span class="mico">
      <Show when={props.g}>{g => <svg viewBox={g().vb} width="12" height="12" fill="currentColor"><path fill-rule={g().eo ? 'evenodd' : 'nonzero'} d={g().d} /></svg>}</Show>
    </span>
  );
}
