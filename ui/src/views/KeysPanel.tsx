// WAVE Keys: the sampled grand piano's page in the channel window. Two knobs (how long a
// released note rings, how much the velocity changes the level) and the state of the
// recordings: shipped with the app, or downloaded once into the sample library.
import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import { KEYS_DYNAMICS, KEYS_RELEASE, type Channel, type KeysStatus } from '../api/types';
import { clamp } from '../lib/format';
import { t, type TranslationKey } from '../lib/i18n';
import { autoDisplay, isAutomated, paramMenu } from '../state/automation';
import { commit, Gesture, wheelEdit } from '../state/edit';
import { chanById, chanOp } from '../state/pattern';
import { say } from '../state/ui';
import { InstIcon } from '../ui/instIcons';
import { Knob } from '../ui/Knob';

type KeysChannel = Extract<Channel, { kind: 'keys' }>;

/** Release knob travel is logarithmic: 40 ms .. 3 s. */
const relT = (ms: number) => Math.log(ms / KEYS_RELEASE.min) / Math.log(KEYS_RELEASE.max / KEYS_RELEASE.min);
const relMs = (t: number) => Math.round(KEYS_RELEASE.min * Math.pow(KEYS_RELEASE.max / KEYS_RELEASE.min, clamp(t, 0, 1)));
const relText = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + ' s' : Math.round(ms) + ' ms');

export function KeysPanel(props: { c: KeysChannel }) {
  const live = () => chanById(props.c.id) as KeysChannel;
  const [st, setSt] = createSignal<KeysStatus | null>(null);
  /** Download progress 0..1 while the piano is being fetched. */
  const [dl, setDl] = createSignal<number | null>(null);
  const [err, setErr] = createSignal<string | null>(null);

  onMount(() => {
    void be().keysStatus().then(setSt).catch(e => setErr(String(e)));
    const offs = [
      be().on('keys-status', setSt),
      be().on('library-progress', p => {
        if (p.id !== (st()?.kit ?? 'salamander-piano')) return;
        if (p.error) { setDl(null); setErr(p.error); return; }
        setDl(p.total ? p.done / p.total : null);
        if (p.total && p.done >= p.total) { setDl(null); void be().keysStatus().then(setSt); }
      }),
    ];
    onCleanup(() => offs.forEach(o => void o.then(un => un())));
  });

  const download = async () => {
    const kit = st()?.kit; if (!kit) return;
    setErr(null); setDl(0);
    try { await be().libraryInstall(kit); say(t('b.kp.downloaded')); }
    catch (e) { setErr(String(e)); }
    finally { setDl(null); void be().keysStatus().then(setSt); }
  };

  const knob = (lab: TranslationKey, titleKey: TranslationKey, tv: () => number, text: () => string, patch: (v: number) => KeysChannel['keys'], def: number, param: 'release' | 'dynamics') => {
    let g: Gesture | null = null;
    const op = (v: number) => [chanOp(live(), { keys: patch(v) } as Partial<Channel>)];
    const target = () => ({ kind: 'keys' as const, chanId: props.c.id, param });
    const reset = () => commit(op(def), t('b.kp.gest', { name: t(lab) }));
    return (
      <Knob key={'keys' + lab + props.c.id} label={t(lab)} title={t('b.kp.knobTitle', { title: t(titleKey) })} t={tv} text={text}
        automated={() => isAutomated(target())}
        onMenu={(x, y) => paramMenu(x, y, target(), reset, props.c.color)}
        onStart={() => { g = new Gesture(t('b.kp.gest', { name: t(lab) })); }}
        onChange={v => { g?.update(op(v)); say(props.c.name + ' · ' + t(lab).toLowerCase() + ' ' + text()); }}
        onEnd={() => { g?.end(); g = null; }}
        onReset={reset}
        onWheel={v => wheelEdit('keys' + lab, t('b.kp.gest', { name: t(lab) }), op(v))} />
    );
  };

  // the knobs show the automated value while it plays
  const kBase = () => live()?.keys ?? props.c.keys;
  const k = () => {
    const base = kBase();
    const at = (param: 'release' | 'dynamics') => autoDisplay({ kind: 'keys', chanId: props.c.id, param });
    return { release: at('release') ?? base.release, dynamics: at('dynamics') ?? base.dynamics };
  };
  const state = () => st()?.state ?? 'idle';
  return (
    <div class="keyspanel">
      <div class="keyshd">
        <span class="big" style={{ color: props.c.color }}><InstIcon id="piano" size={34} /></span>
        <div class="keysinfo">
          <div class="nm">WAVE KEYS</div>
          <div class="note">{t('b.kp.desc')}</div>
        </div>
      </div>
      <Show when={state() === 'missing' || (state() === 'idle' && st() && !st()!.installed) || dl() != null}>
        <div class="keysmsg">
          <Show when={dl() != null} fallback={<>
            <span>{t('b.kp.missing')}</span>
            <button class="smallbtn primary" onClick={download}>{t('b.kp.download')}</button>
          </>}>
            <span>{t('b.kp.downloading', { n: Math.round((dl() ?? 0) * 100) })}</span>
            <div class="keysbar"><div style={{ width: Math.round((dl() ?? 0) * 100) + '%' }} /></div>
          </Show>
        </div>
      </Show>
      <Show when={state() === 'loading' && dl() == null}><div class="keysmsg"><span>{t('b.kp.loading')}</span></div></Show>
      <Show when={err() ?? (state() === 'error' ? st()?.error ?? t('b.kp.loadFail') : null)}>
        {m => <div class="keysmsg bad"><span>{m()}</span><button class="smallbtn" onClick={download}>{t('b.kp.retry')}</button></div>}
      </Show>
      <div class="keysknobs">
        {knob('b.kp.release', 'b.kp.releaseTip', () => relT(k().release), () => relText(k().release), t => ({ ...kBase(), release: relMs(t) }), relT(KEYS_RELEASE.def), 'release')}
        {knob('b.kp.dynamics', 'b.kp.dynamicsTip', () => k().dynamics / 100, () => Math.round(k().dynamics) + ' %', t => ({ ...kBase(), dynamics: Math.round(clamp(t, 0, 1) * 100) }), KEYS_DYNAMICS.def / 100, 'dynamics')}
        <div class="keyscredit" title={t('b.kp.creditTitle')}>
          {t('b.kp.credit')}
        </div>
      </div>
    </div>
  );
}
