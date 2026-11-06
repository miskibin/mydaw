import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './styles.css';
import { render } from 'solid-js/web';
import { getBackend } from './api/backend';
import { onStateEvent } from './state/edit';
import { replaceState, S, setS } from './state/store';
import { startTelemetry } from './state/telemetry';
import { setU } from './state/ui';
import { App } from './App';
import { initTheme } from './ui/theme';

initTheme();

async function boot() {
  const be = await getBackend();
  const [catalog, state] = await Promise.all([be.fxCatalog(), be.getState()]);
  setS('catalog', catalog);
  replaceState(state);
  await be.on('state', onStateEvent);
  await startTelemetry();
  // initial selection like the prototype: the Vocal insert with its compressor
  const voc = S.project.inserts.find(m => m.fx.length > 1) ?? S.project.inserts[1] ?? S.project.inserts[0];
  if (voc) setU({ mixSel: voc.id, fxSel: voc.fx[1]?.id ?? voc.fx[0]?.id ?? null });
  const p = S.project.patterns[0];
  setU('cur', p ? { kind: 'pat', ref: p.id } : S.project.sources[0] ? { kind: 'audio', ref: S.project.sources[0].id } : null);
  setU('playing', state.transport.playing);
  render(() => <App />, document.getElementById('root')!);
}

boot().catch(e => {
  console.error(e);
  document.getElementById('root')!.innerHTML = '<pre style="color:#e8607a;padding:20px;font:12px monospace">WAVE: nie udało się uruchomić UI\n' + String(e?.stack ?? e) + '</pre>';
});
