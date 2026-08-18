import { createSignal, For, Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import type { PanelProps } from '../panels/types';
import {
  finishRun,
  runMatrix,
  runScenario,
  scenarioLabel,
  syncClocks,
  type BenchReport,
  type MeterDeps,
  type ScenarioReport,
  type StreamConfig,
} from './ipcBench';

/**
 * The hidden IPC benchmark panel (spike 4).
 *
 * Only reachable when the Rust side contributed the `ipc-bench` manifest, which
 * takes `MYDAW_DEV_PANELS=1`. It is a measurement instrument, not product UI:
 * run the whole matrix or one scenario, read the table, keep the JSON.
 *
 * The auto variant of the manifest (`MYDAW_IPC_BENCH=1`) does not render
 * anything interesting -- `state.tsx` starts that run without waiting for the
 * panel to mount, so a run never depends on which tab the dock happens to show.
 */
export function IpcBenchPanel(props: PanelProps): JSX.Element {
  const app = useAppState();
  const [busy, setBusy] = createSignal(false);
  const [progress, setProgress] = createSignal('');
  const [report, setReport] = createSignal<BenchReport | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const auto = () => props.manifest.id.endsWith('-auto');

  /** The meter subscription the app itself owns, handed back after the run. */
  const meterDeps = (): MeterDeps => ({
    subscribeMeters: (onFrame) => app.backend.subscribeMeters(onFrame),
    play: () => app.backend.transportPlay(),
    stop: () => app.backend.transportStop(),
    onFrame: (frame) => {
      app.meters.push(frame);
      app.playhead.update(frame.samplePos, frame.playing);
    },
    restore: () => app.resubscribeMeters(),
  });

  const guard = async (label: string, run: () => Promise<BenchReport>) => {
    if (busy()) return;
    setBusy(true);
    setError(null);
    setProgress(label);
    try {
      setReport(await run());
      setProgress('done');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
      setProgress('');
    } finally {
      setBusy(false);
    }
  };

  const runFull = () =>
    guard('starting', () =>
      runMatrix({
        meters: meterDeps(),
        onProgress: (label, index, total) => setProgress(`${index + 1}/${total} ${label}`),
      }),
    );

  /** One scenario, aggregated and written through the same path as a full run. */
  const runOne = (config: StreamConfig) =>
    guard(scenarioLabel(config), async () => {
      const handshake = await syncClocks();
      const samples = await runScenario(config, handshake.sync.offsetMs);
      return finishRun([samples], handshake);
    });

  const download = () => {
    const current = report();
    if (!current) return;
    const blob = new Blob([JSON.stringify(current, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `ipc-bench-${current.runId}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  const num = (value: number | undefined, digits = 2) =>
    value === undefined || Number.isNaN(value) ? '—' : value.toFixed(digits);

  const row = (scenario: ScenarioReport) => (
    <tr>
      <td>{scenario.label}</td>
      <td>{num(scenario.stats?.p50Ms)}</td>
      <td>{num(scenario.stats?.p95Ms)}</td>
      <td>{num(scenario.stats?.p99Ms)}</td>
      <td>{num(scenario.stats?.maxMs)}</td>
      <td>{num(scenario.achievedRateHz, 1)}</td>
      <td>{scenario.dropped}</td>
      <td>{scenario.longTasks}</td>
    </tr>
  );

  return (
    <div class="ipc-bench">
      <div class="ipc-bench__actions">
        <button type="button" onClick={runFull} disabled={busy()}>
          Run full matrix
        </button>
        <button
          type="button"
          onClick={() =>
            runOne({
              transport: 'channel-json',
              payloadBytes: 64,
              rateHz: 60,
              durationMs: 5_000,
            })
          }
          disabled={busy()}
        >
          Meter frame, JSON
        </button>
        <button
          type="button"
          onClick={() =>
            runOne({
              transport: 'channel-binary',
              payloadBytes: 64,
              rateHz: 60,
              durationMs: 5_000,
            })
          }
          disabled={busy()}
        >
          Meter frame, binary
        </button>
        <button type="button" onClick={download} disabled={!report()}>
          Download JSON
        </button>
      </div>

      <p class="ipc-bench__status">
        <Show when={auto()}>
          <strong>auto mode</strong>{' '}
        </Show>
        {busy() ? `running ${progress()}` : (progress() ?? '')}
      </p>

      <Show when={error()}>
        <p class="ipc-bench__error">{error()}</p>
      </Show>

      <Show when={report()}>
        {(current) => (
          <>
            <p class="ipc-bench__meta">
              {current().runId} · webview {current().environment.webviewVersion ?? 'unknown'} ·{' '}
              {current().environment.profile} build · clock ±{num(current().epoch?.errorMs, 3)} ms ·
              invoke rtt p50 {num(current().rtt?.p50Ms, 3)} ms
            </p>
            <table class="ipc-bench__table">
              <thead>
                <tr>
                  <th>scenario</th>
                  <th>p50</th>
                  <th>p95</th>
                  <th>p99</th>
                  <th>max</th>
                  <th>Hz</th>
                  <th>drop</th>
                  <th>lt</th>
                </tr>
              </thead>
              <tbody>
                <For each={current().scenarios}>{row}</For>
              </tbody>
            </table>
          </>
        )}
      </Show>
    </div>
  );
}

export default IpcBenchPanel;
