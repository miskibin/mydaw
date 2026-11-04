/**
 * File › Export › WAV (Ctrl+R).
 *
 * Settings first, then the native save dialog, then the render's progress in
 * the same window -- the order Ableton uses, so the file name is chosen with
 * the format already decided. The render happens in a worker process: the
 * session stays live and playable, and Cancel stops the worker without
 * touching the destination.
 */

import { For, Show, createMemo, onCleanup, type JSX } from 'solid-js';

import type { ExportOptions } from '../api/types';
import {
  EXPORT_BIT_DEPTHS,
  EXPORT_SAMPLE_RATES,
  EXPORT_TAILS,
  bitDepthLabel,
  exportBlocker,
  exportBodyTicks,
  exportSeconds,
  exportSummary,
  exportWarnings,
  fileNameOf,
  formatDuration,
  progressLabel,
  progressPercent,
  sampleRateLabel,
} from '../lib/exportWav';
import { patternTicks } from '../lib/pattern';
import { useAppState } from '../state';

function Choice<T extends string | number | boolean>(props: {
  label: string;
  value: T;
  choices: readonly T[];
  format: (value: T) => string;
  disabled: boolean;
  onChange: (value: T) => void;
}): JSX.Element {
  return (
    <div class="wv-export__row">
      <span class="wv-export__label">{props.label}</span>
      <div class="wv-seg" role="radiogroup" aria-label={props.label}>
        <For each={props.choices}>
          {(choice) => (
            <button
              type="button"
              class="wv-seg__btn"
              classList={{ 'is-active': choice === props.value }}
              role="radio"
              aria-checked={choice === props.value}
              disabled={props.disabled}
              onClick={() => props.onChange(choice)}
            >
              {props.format(choice)}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}

export function ExportDialog(): JSX.Element {
  const app = useAppState();
  const exporter = app.exporter;
  const options = exporter.options;
  const busy = () => exporter.phase() === 'choosing' || exporter.phase() === 'running';
  const locked = () => exporter.phase() !== 'settings';

  const bodyTicks = createMemo(() => {
    const pattern = app.pattern();
    return exportBodyTicks(
      options().mode,
      app.playlist()?.endTicks ?? 0,
      pattern ? patternTicks(pattern) : 0,
    );
  });
  const seconds = () => exportSeconds(bodyTicks(), app.bpm(), options().tailSeconds);
  const blocker = () => exportBlocker(options().mode, bodyTicks(), seconds());
  const rangeNote = () => {
    const tail = options().tailSeconds;
    const tailText = tail > 0 ? ` + ${tail} s tail` : '';
    return options().mode === 'song'
      ? `${formatDuration(seconds())} · to the end of the last clip${tailText}`
      : `${formatDuration(seconds())} · one loop of “${app.pattern()?.name ?? 'pattern'}”${tailText}`;
  };

  function set(patch: Partial<ExportOptions>): void {
    exporter.setOptions(patch);
  }

  function start(): void {
    if (blocker() !== null) return;
    void exporter.start();
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      exporter.hide();
      return;
    }
    const target = event.target as HTMLElement | null;
    // A focused button handles its own Enter; a button the last click removed
    // (New export) no longer can, so it does not count.
    const onButton = target?.isConnected && target.closest('button');
    if (event.key === 'Enter' && exporter.phase() === 'settings' && !onButton) {
      event.preventDefault();
      start();
    }
  }
  window.addEventListener('keydown', onKeyDown, true);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown, true));

  return (
    <div class="wv-scrim wv-scrim--dim" onClick={() => exporter.hide()}>
      <div
        class="wv-modal wv-export"
        role="dialog"
        aria-modal="true"
        aria-label="Export WAV"
        onClick={(event) => event.stopPropagation()}
      >
        <div class="wv-modal__head">
          <span>Export WAV</span>
          <span class="wv-spacer" />
          <span class="wv-modal__kind" title={app.project().path ?? app.project().name}>
            {app.project().name}
          </span>
        </div>

        <div class="wv-export__body">
          <Choice
            label="Range"
            value={options().mode}
            choices={['song', 'pattern'] as const}
            format={(mode) => (mode === 'song' ? 'Song' : 'Pattern')}
            disabled={locked()}
            onChange={(mode) => set({ mode })}
          />
          <p class="wv-export__note" classList={{ 'is-blocked': blocker() !== null && !locked() }}>
            {blocker() ?? rangeNote()}
          </p>
          <Choice
            label="Sample rate"
            value={options().sampleRate}
            choices={EXPORT_SAMPLE_RATES}
            format={sampleRateLabel}
            disabled={locked()}
            onChange={(sampleRate) => set({ sampleRate })}
          />
          <Choice
            label="Bit depth"
            value={options().bitDepth}
            choices={EXPORT_BIT_DEPTHS}
            format={bitDepthLabel}
            disabled={locked()}
            onChange={(bitDepth) => set({ bitDepth })}
          />
          <Show when={options().bitDepth === 16}>
            <p class="wv-export__note">Dithered (TPDF) to 16 bits, for CD and most streaming uploads.</p>
          </Show>
          <Choice
            label="Tail"
            value={options().tailSeconds}
            choices={
              (EXPORT_TAILS as readonly number[]).includes(options().tailSeconds)
                ? EXPORT_TAILS
                : [...EXPORT_TAILS, options().tailSeconds].sort((a, b) => a - b)
            }
            format={(tail) => (tail === 0 ? 'Cut' : `${tail} s`)}
            disabled={locked()}
            onChange={(tailSeconds) => set({ tailSeconds })}
          />
          <Choice
            label="Normalize"
            value={options().normalize}
            choices={[false, true] as const}
            format={(on) => (on ? 'Peak −0.1 dBFS' : 'Off')}
            disabled={locked()}
            onChange={(normalize) => set({ normalize })}
          />

          <Show when={exporter.phase() === 'settings' || exporter.phase() === 'choosing'}>
            <p class="wv-export__fine">
              Rendered offline in a separate process from the current, unsaved session: every
              channel, insert, CLAP/VST3 plugin, send and the master chain. Playback stays live.
              Plugin latency is not compensated yet.
            </p>
          </Show>

          <Show when={exporter.phase() === 'running' && exporter.job()}>
            {(job) => (
              <div class="wv-export__status" aria-live="polite">
                <div class="wv-export__line">
                  <span>{progressLabel(job())}</span>
                  <span class="wv-spacer" />
                  <span class="wv-export__mono">{progressPercent(job())}%</span>
                </div>
                <div
                  class="wv-export__bar"
                  role="progressbar"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={progressPercent(job())}
                >
                  <div style={{ width: `${progressPercent(job())}%` }} />
                </div>
                <p class="wv-export__path" title={job().path}>{job().path}</p>
              </div>
            )}
          </Show>

          <Show when={exporter.phase() === 'completed' && exporter.job()}>
            {(job) => (
              <div class="wv-export__status" aria-live="polite">
                <div class="wv-export__line">
                  <strong>Saved {fileNameOf(job().path ?? '')}</strong>
                </div>
                <p class="wv-export__path" title={job().path}>{job().path}</p>
                <p class="wv-export__mono">{exportSummary(job())}</p>
                <For each={exportWarnings(job())}>
                  {(warning) => <p class="wv-export__warning">{warning}</p>}
                </For>
              </div>
            )}
          </Show>

          <Show when={exporter.phase() === 'cancelled'}>
            <div class="wv-export__status" aria-live="polite">
              <p>Export cancelled. Nothing was written.</p>
            </div>
          </Show>

          <Show when={exporter.error()}>
            <p class="wv-export__error" role="alert">
              {exporter.phase() === 'failed' ? 'Export failed: ' : ''}
              {exporter.error()}
            </p>
          </Show>
        </div>

        <div class="wv-export__foot">
          <Show
            when={!busy()}
            fallback={
              <Show when={exporter.phase() === 'running'} fallback={<span class="wv-export__fine">Choose where to save…</span>}>
                <button type="button" class="wv-chip" onClick={() => void exporter.cancel()}>
                  Cancel export
                </button>
              </Show>
            }
          >
            <button type="button" class="wv-chip" onClick={() => exporter.hide()}>
              Close
            </button>
            <Show
              when={exporter.phase() === 'settings'}
              fallback={
                <button type="button" class="wv-chip" onClick={() => exporter.reset()}>
                  {exporter.phase() === 'completed' ? 'New export' : 'Back to settings'}
                </button>
              }
            >
              <button
                type="button"
                class="wv-chip wv-export__go"
                disabled={blocker() !== null}
                title={blocker() ?? 'Choose a file name, then render (Enter)'}
                ref={(el) => queueMicrotask(() => el.focus())}
                onClick={start}
              >
                Export…
              </button>
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
}
