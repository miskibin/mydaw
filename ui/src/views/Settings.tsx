import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import type { AudioConfig, AudioDevices, AudioHost, AudioStatus, CliStatus, MidiDevice, Vst3Info } from '../api/types';
import * as A from '../state/actions';
import { keyName, midiFlash, midiFollow, midiStatus, selectMidiDevice, setMidiFollow, setMidiTarget } from '../state/midi';
import { S } from '../state/store';
import { say, setU, U } from '../state/ui';
import { Segmented } from '../ui/Knob';
import { prefs, resetPrefs, setPref } from '../state/prefs';
import { setTheme, THEMES, themeMode, themeName } from '../ui/theme';
import { t } from '../lib/i18n';
import type { TranslationKey } from '../lib/i18n';

const RATES = [44100, 48000, 88200, 96000];
const BUFFERS = [32, 64, 128, 256, 512, 1024, 2048];

export function Settings() {
  return <Show when={U.settings}><SettingsDialog /></Show>;
}

function SettingsDialog() {
  const [hosts, setHosts] = createSignal<AudioHost[]>([]);
  const [devs, setDevs] = createSignal<AudioDevices>({ outputs: [], inputs: [] });
  const [st, setSt] = createSignal<AudioStatus | null>(null);
  const [cli, setCli] = createSignal<CliStatus | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const loadDevs = (host: string) => be().audioDevices(host).then(setDevs).catch(e => setErr(String(e)));
  let off: (() => void) | null = null;
  onCleanup(() => off?.());
  onMount(async () => {
    try {
      const [h, s] = await Promise.all([be().audioHosts(), be().audioGetConfig()]);
      setHosts(h); setSt(s); await loadDevs(s.host);
    } catch (e) { setErr(String(e)); }
    be().cliStatus().then(setCli).catch(() => {});
    off = await be().on('audio-status', s => setSt(s));
  });
  const apply = async (patch: Partial<AudioConfig>) => {
    const s = st(); if (!s) return;
    const cfg: AudioConfig = { host: s.host, output: s.output, input: s.input, sampleRate: s.sampleRate, bufferSize: s.bufferSize, inputChannel: s.inputChannel ?? null, ...patch };
    setBusy(true); setErr(null);
    try {
      if (patch.host && patch.host !== s.host) { await loadDevs(patch.host); cfg.output = null; cfg.input = null; }
      setSt(await be().audioSetConfig(cfg));
    } catch (e) { setErr(String(e)); }
    setBusy(false);
  };
  const opt = (v: string | number | null) => v == null ? '' : String(v);
  const [tab, setTab] = createSignal<Tab>(lastTab);
  const pickTab = (t: Tab) => { lastTab = t; setTab(t); };
  return (
    <div class="overlay" onPointerDown={() => setU('settings', false)}>
      <div class="dialog setdlg" onPointerDown={e => e.stopPropagation()} onKeyDown={e => e.stopPropagation()}>
        <div class="hd"><span>{t('d.set.title')}</span><button onClick={() => setU('settings', false)}>×</button></div>
        <div class="setbody">
          <nav class="settabs">
            <For each={TABS}>{([id, label]) => <button classList={{ on: tab() === id }} onClick={() => pickTab(id)}>{t(label)}</button>}</For>
          </nav>
          <div class="setpane">
            <Show when={tab() === 'general'}><GeneralSection /></Show>
            <Show when={tab() === 'look'}><LookSection /></Show>
            <Show when={tab() === 'audio'}>
        <Show when={st()} fallback={<span class="note">{err() ?? t('d.set.loading')}</span>}>
          {s => (
            <div class="form">
              <label>{t('d.set.driver')}</label>
              <div class="row" style={{ gap: '8px' }}>
                <select class="sel" value={s().host} disabled={busy()} onChange={e => apply({ host: e.currentTarget.value })}>
                  <For each={hosts()}>{h => <option value={h.id}>{h.name}</option>}</For>
                </select>
              </div>
              <Show when={!s().asioCompiled}><span /><span class="note warn">{t('d.set.noasio')}</span></Show>
              <label>{t('d.set.output')}</label>
              <select class="sel" value={opt(s().output)} disabled={busy()} onChange={e => apply({ output: e.currentTarget.value || null })}>
                <option value="">{t('d.set.default')}</option>
                <For each={devs().outputs}>{d => <option value={d}>{d}</option>}</For>
              </select>
              <label>{t('d.set.input')}</label>
              <select class="sel" value={opt(s().input)} disabled={busy()} onChange={e => apply({ input: e.currentTarget.value || null })}>
                <option value="">{t('d.set.defaultnone')}</option>
                <For each={devs().inputs}>{d => <option value={d}>{d}</option>}</For>
              </select>
              <label>{t('d.set.inchan')}</label>
              <select class="sel" value={String(s().inputChannel ?? 0)} disabled={busy() || s().inputChannels < 2}
                title={s().inputChannels ? t('d.set.inchan.tip') : t('d.set.inchan.tip0')}
                onChange={e => apply({ inputChannel: +e.currentTarget.value })}>
                <For each={Array.from({ length: Math.max(s().inputChannels, (s().inputChannel ?? 0) + 1) }, (_, i) => i)}>{i => <option value={String(i)}>{t('d.set.inmono', { n: i + 1 })}</option>}</For>
              </select>
              <label>{t('d.set.rate')}</label>
              <select class="sel" value={opt(s().sampleRate)} disabled={busy()} onChange={e => apply({ sampleRate: e.currentTarget.value ? +e.currentTarget.value : null })}>
                <option value="">{t('d.set.auto')}</option>
                <For each={RATES}>{r => <option value={String(r)}>{r} Hz</option>}</For>
              </select>
              <label>{t('d.set.buffer')}</label>
              <select class="sel" value={opt(s().bufferSize)} disabled={busy()} onChange={e => apply({ bufferSize: e.currentTarget.value ? +e.currentTarget.value : null })}>
                <option value="">{t('d.set.auto')}</option>
                <For each={BUFFERS}>{b => <option value={String(b)}>{t('d.set.samples', { n: b })}</option>}</For>
              </select>
              <label>{t('d.set.state')}</label>
              <span class="note mono" style={{ color: s().running ? 'var(--green)' : 'var(--danger)' }}>
                {t('d.set.statline', { state: s().running ? t('d.set.running') : t('d.set.stopped'), rate: s().actualRate, buf: s().actualBuffer, ms: s().latencyMs.toFixed(1), ch: s().inputChannels })}
              </span>
              <Show when={s().error}><span /><span class="note err">{s().error}</span></Show>
              <Show when={s().note}><span /><span class="note">{s().note}</span></Show>
              <Show when={err()}><span /><span class="note err">{err()}</span></Show>
              <label>{t('d.set.recoffset')}</label>
              <RecCalib st={s()} apply={apply} busy={busy()} />
              <Show when={s().inputUnderruns || s().inputOverruns}>
                <label>{t('d.set.dropouts')}</label>
                <span class="note warn">{t('d.set.dropouts.note', { n: (s().inputUnderruns ?? 0) + (s().inputOverruns ?? 0) })}</span>
              </Show>
              <label>CLI</label>
              <span class="note">
                <Show when={cli()} fallback="—">
                  {c => <><span class="mono" style={{ color: 'var(--fg-hi)' }}>{c().command}</span> · {c().installed ? t('d.set.inpath', { path: c().path ? ' (' + c().path + ')' : '' }) : t('d.set.notinpath')}{c().note ? ' · ' + c().note : ''}</>}
                </Show>
              </span>
            </div>
          )}
        </Show>
            </Show>
            <Show when={tab() === 'midi'}><MidiSection /></Show>
          </div>
        </div>
      </div>
    </div>
  );
}

type Tab = 'general' | 'look' | 'audio' | 'midi';
const TABS: [Tab, TranslationKey][] = [['general', 'd.set.tab.general'], ['look', 'd.set.tab.look'], ['audio', 'd.set.tab.audio'], ['midi', 'd.set.tab.midi']];
let lastTab: Tab = 'general';

const OnOff = (p: { on: () => boolean; set: (v: boolean) => void; title?: string }) =>
  <Segmented options={[t('d.set.off'), t('d.set.on')]} value={() => (p.on() ? 1 : 0)} pick={i => p.set(i === 1)} title={p.title ?? ''} />;

const AUTOSAVE = [0, 1, 2, 5, 10];
const SCALES = [0.8, 0.9, 1, 1.1, 1.25, 1.5];

function GeneralSection() {
  return (
    <>
      <div class="form">
        <label>{t('settings.language')}</label>
        <Segmented options={[t('settings.english'), t('settings.polish')]} value={() => prefs().lang === 'pl' ? 1 : 0}
          pick={i => setPref('lang', i === 1 ? 'pl' : 'en')} />
      </div>
      <div class="sechd">{t('d.set.sec.project')}</div>
      <div class="form">
        <label>{t('d.set.autosave')}</label>
        <Segmented options={AUTOSAVE.map(m => (m ? t('d.set.min', { n: m }) : t('d.set.off')))} value={() => AUTOSAVE.indexOf(prefs().autosaveMin)}
          pick={i => setPref('autosaveMin', AUTOSAVE[i])} title={t('d.set.autosave.tip')} />
      </div>
      <div class="sechd">{t('d.set.sec.edit')}</div>
      <div class="form">
        <label>{t('d.set.audition')}</label>
        <OnOff on={() => prefs().noteAudition} set={v => setPref('noteAudition', v)} title={t('d.set.audition.tip')} />
        <label>{t('d.set.hints')}</label>
        <OnOff on={() => prefs().hints} set={v => setPref('hints', v)} title={t('d.set.hints.tip')} />
      </div>
      <div class="sechd">{t('d.set.sec.rec')}</div>
      <div class="form">
        <label>{t('d.set.metro')}</label>
        <OnOff on={() => S.transport.metro} set={v => void A.setTransportFlag('metro', v)} />
        <label>Count-in</label>
        <OnOff on={() => S.transport.countIn} set={v => void A.setTransportFlag('countIn', v)} title={t('d.set.countin.tip')} />
      </div>
      <div class="setfoot">
        <button class="smallbtn" onClick={() => { resetPrefs(); setTheme('dark'); say(t('d.set.reset.say')); }}>{t('d.set.reset')}</button>
      </div>
    </>
  );
}

function LookSection() {
  return (
    <>
      <div class="sechd">{t('d.set.sec.theme')}</div>
      <div class="themegrid">
        <For each={THEMES}>{th => (
          <button class="themecard" classList={{ on: themeMode() === th.id }} onClick={() => { setTheme(th.id); say(t('d.set.say.theme', { name: themeName(th) })); }} title={themeName(th)}>
            <span class="sw" style={{ background: th.swatch[0] }}>
              <span style={{ background: th.swatch[1] }} /><span style={{ background: th.swatch[2] }} />
            </span>
            <span class="nm">{themeName(th)}</span>
          </button>
        )}</For>
        <button class="themecard" classList={{ on: themeMode() === 'system' }} onClick={() => { setTheme('system'); say(t('d.set.say.theme.sys')); }} title={t('d.set.theme.sys.tip')}>
          <span class="sw sys"><span /><span /></span>
          <span class="nm">{t('d.set.theme.sys')}</span>
        </button>
      </div>
      <div class="sechd" style={{ 'margin-top': '14px' }}>{t('d.set.sec.ui')}</div>
      <div class="form">
        <label>{t('d.set.scale')}</label>
        <Segmented options={SCALES.map(z => Math.round(z * 100) + '%')} value={() => SCALES.indexOf(prefs().uiScale)}
          pick={i => setPref('uiScale', SCALES[i])} title={t('d.set.scale.tip')} />
        <label>{t('d.set.clipicons')}</label>
        <OnOff on={() => prefs().clipIcons} set={v => setPref('clipIcons', v)} title={t('d.set.clipicons.tip')} />
      </div>
    </>
  );
}

function MidiSection() {
  const [devs, setDevs] = createSignal<MidiDevice[]>([]);
  const [err, setErr] = createSignal<string | null>(null);
  const refresh = () => be().midiListDevices().then(setDevs).catch(e => setErr(String(e)));
  onMount(refresh);
  const run = (p: Promise<void>) => { setErr(null); p.catch(e => setErr(String(e))); };
  const st = midiStatus;
  const pitched = () => (S.project.channels ?? []);
  const targetVal = () => (midiFollow() ? 'follow' : String(st()?.target ?? ''));
  const pickTarget = (v: string) => {
    if (v === 'follow') { setMidiFollow(true); const ch = U.patWin?.ch; if (ch != null) run(setMidiTarget(ch)); return; }
    setMidiFollow(false); run(setMidiTarget(v === '' ? null : +v));
  };
  return (
    <>
      <div class="sechd">{t('d.set.sec.midi')}</div>
      <div class="form">
        <label>{t('d.set.device')}</label>
        <div class="row" style={{ gap: '8px' }}>
          <select class="sel" style={{ flex: 1 }} value={st()?.selected?.id ?? ''} onChange={e => run(selectMidiDevice(e.currentTarget.value || null))}>
            <option value="">{t('d.set.none')}</option>
            <Show when={st()?.selected && !devs().some(d => d.id === st()!.selected!.id)}>
              <option value={st()!.selected!.id}>{t('d.set.disconnected', { name: st()!.selected!.name })}</option>
            </Show>
            <For each={devs()}>{d => <option value={d.id}>{d.name}</option>}</For>
          </select>
          <button class="smallbtn" style={{ height: '26px' }} onClick={refresh} title={t('d.set.refresh.tip')}>{t('d.set.refresh')}</button>
        </div>
        <label>{t('d.set.playson')}</label>
        <select class="sel" value={targetVal()} onChange={e => pickTarget(e.currentTarget.value)}
          title={t('d.set.playson.tip')}>
          <option value="follow">{t('d.set.follow')}</option>
          <option value="">{t('d.set.firstinst')}</option>
          <For each={pitched()}>{c => <option value={String(c.id)}>{c.name}</option>}</For>
        </select>
        <label>{t('d.set.state')}</label>
        <span class="note mono row" style={{ gap: '6px' }}>
          <span class="midiled" classList={{ on: !!st()?.connected, hit: midiFlash() }} />
          <Show when={st()?.connected} fallback={<span style={{ color: 'var(--fg4)' }}>{st()?.selected ? t('d.set.waiting') : t('d.set.notchosen')}</span>}>
            {c => <span style={{ color: 'var(--green)' }}>{c().name}</span>}
          </Show>
          <Show when={st()?.lastNote}>{n => <span>{t('d.set.last', { key: keyName(n().key), vel: n().on ? ' v' + n().velocity : '' })}</span>}</Show>
        </span>
        <Show when={st()?.error || err()}><span /><span class="note err">{st()?.error ?? err()}</span></Show>
      </div>
    </>
  );
}

/** "VST3…" from the add-effect menu: a searchable list of scanned plugins. */
export function VstPicker() {
  return <Show when={U.vstPicker}>{v => <VstPickerDialog insId={v().insId} />}</Show>;
}
function VstPickerDialog(props: { insId: number }) {
  const [list, setList] = createSignal<Vst3Info[] | null>(null);
  const [q, setQ] = createSignal('');
  const [err, setErr] = createSignal<string | null>(null);
  const scan = (rescan: boolean) => { setList(null); setErr(null); be().vst3Scan(rescan).then(setList).catch(e => { setErr(String(e)); setList([]); }); };
  onMount(() => scan(false));
  let input!: HTMLInputElement;
  onMount(() => setTimeout(() => input?.focus(), 0));
  const shown = () => { const s = q().toLowerCase(); return (list() ?? []).filter(p => !s || (p.name + ' ' + p.vendor + ' ' + p.category).toLowerCase().includes(s)); };
  const close = () => setU('vstPicker', null);
  return (
    <div class="overlay" onPointerDown={close}>
      <div class="dialog" style={{ width: 'min(440px,92vw)', display: 'flex', 'flex-direction': 'column', gap: '10px' }} onPointerDown={e => e.stopPropagation()}>
        <div class="hd" style={{ 'margin-bottom': 0 }}><span>{t('d.set.vst.title')}</span><button onClick={close}>×</button></div>
        <div class="row" style={{ gap: '8px' }}>
          <input ref={input} class="textin" style={{ flex: 1 }} placeholder={t('d.set.search')} value={q()} onInput={e => setQ(e.currentTarget.value)}
            onKeyDown={e => { e.stopPropagation(); if (e.key === 'Escape') close(); if (e.key === 'Enter' && shown()[0]) { A.addVst(props.insId, shown()[0]); close(); } }} />
          <button class="smallbtn" style={{ height: '26px' }} onClick={() => scan(true)} title={t('d.set.rescan.tip')}>{t('d.set.rescan')}</button>
        </div>
        <Show when={err()}><span class="note err">{err()}</span></Show>
        <div class="plist" style={{ height: '300px' }}>
          <Show when={list()} fallback={<span class="note">{t('d.set.scanning')}</span>}>
            <For each={shown()} fallback={<span class="note">{t('d.set.novst')}</span>}>
              {p => (
                <div class="pi" onClick={() => { A.addVst(props.insId, p); close(); say(t('d.set.vst.added', { name: p.name })); }} title={p.path}>
                  <span class="tile" style={{ width: '20px', height: '20px', background: '#9a8cf026', color: '#9a8cf0' }}>
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M4 6h16v12H4z M8 10v4 M12 9v6 M16 11v2" /></svg>
                  </span>
                  <span class="ell" style={{ flex: 1, font: '500 12px var(--sans)', color: 'var(--fg-hi)' }}>{p.name}</span>
                  <span class="ell" style={{ font: '11px var(--sans)', color: 'var(--fg4)', 'max-width': '140px' }}>{p.vendor}</span>
                </div>
              )}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}

/** Recording offset: measured with clicks through the speakers / headphones into the mic. */
function RecCalib(props: { st: AudioStatus; apply: (p: Partial<AudioConfig>) => Promise<void>; busy: boolean }) {
  const [meas, setMeas] = createSignal(false);
  const [msg, setMsg] = createSignal<string | null>(null);
  const off = () => props.st.recOffsetMs ?? 0;
  const set = (v: number) => void props.apply({ recOffsetMs: Math.round(Math.max(-200, Math.min(200, v)) * 10) / 10 });
  const measure = async () => {
    if (meas()) return;
    setMeas(true); setMsg(t('d.set.measuring'));
    try {
      const r = await be().audioMeasureLatency();
      if (r.confidence < 0.5) setMsg(t('d.set.unsure', { n: Math.round(r.confidence * 100) }));
      else { set(r.suggestedOffsetMs); setMsg(t('d.set.measured', { rt: r.roundTripMs.toFixed(1), rep: r.reportedMs.toFixed(1), off: r.suggestedOffsetMs.toFixed(1) })); }
    } catch (e) { setMsg(t('d.set.measfail', { err: String(e) })); }
    setMeas(false);
  };
  return (
    <div style={{ display: 'flex', 'flex-direction': 'column', gap: '4px' }}>
      <div class="row" style={{ gap: '8px' }}>
        <input class="textin" type="number" step="0.5" style={{ width: '80px' }} value={off()} disabled={props.busy}
          title={t('d.set.offset.tip')}
          onChange={e => { const v = parseFloat(e.currentTarget.value); if (isFinite(v)) set(v); }} />
        <span class="note">ms</span>
        <button class="smallbtn primary" style={{ height: '26px' }} disabled={meas() || props.busy} onClick={measure}
          title={t('d.set.measure.tip')}>{meas() ? t('d.set.measuring.btn') : t('d.set.measure')}</button>
        <Show when={off() !== 0}><button class="smallbtn" style={{ height: '26px' }} onClick={() => set(0)}>{t('d.set.zero')}</button></Show>
      </div>
      <Show when={msg()}><span class="note">{msg()}</span></Show>
    </div>
  );
}
