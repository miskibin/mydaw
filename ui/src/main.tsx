import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './styles.css';
import { render } from 'solid-js/web';
import { getBackend } from './api/backend';
import { onStateEvent } from './state/edit';
import { replaceState, S, setS } from './state/store';
import { openRack } from './state/pattern';
import { startTelemetry } from './state/telemetry';
import { setU } from './state/ui';
import { App } from './App';
import { initTheme } from './ui/theme';
import { initPrefs } from './state/prefs';
import { initUpdates } from './state/updates';
import { t } from './lib/i18n';
import { loadStdLib } from './state/library';

initTheme();
initPrefs();
if (import.meta.env.VITE_PERF) void import('./perfHook'); // perf harness only; compiled out otherwise

async function boot() {
  const be = await getBackend();
  const [catalog, state] = await Promise.all([be.fxCatalog(), be.getState()]);
  setS('catalog', catalog);
  replaceState(state);
  await be.on('state', onStateEvent);
  await startTelemetry();
  // the standard library's file list, so the first "add channel" menu already offers it
  void loadStdLib();
  // initial selection like the prototype: the Vocal insert with its compressor
  const voc = S.project.inserts.find(m => m.fx.length > 1) ?? S.project.inserts[1] ?? S.project.inserts[0];
  if (voc) setU({ mixSel: voc.id, fxSel: voc.fx[1]?.id ?? voc.fx[0]?.id ?? null });
  const p = S.project.patterns[0];
  setU('cur', p ? { kind: 'pat', ref: p.id } : S.project.sources[0] ? { kind: 'audio', ref: S.project.sources[0].id } : null);
  setU('playing', state.transport.playing);
  render(() => <App />, document.getElementById('root')!);
  // nothing on the playlist yet: start on the Channel Rack, where a beat begins (FL)
  if (!S.project.clips.length && S.project.patterns.length) openRack();
  void initUpdates(); // version, "What's new" after an update, the first automatic check
}

boot().catch(e => {
  console.error(e);
  const pre = document.createElement('pre');
  pre.style.cssText = 'color:#e8607a;padding:20px;font:12px monospace';
  pre.textContent = t('startup.failed', { error: String(e?.stack ?? e) });
  document.getElementById('root')!.replaceChildren(pre);
});
