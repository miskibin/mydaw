/**
 * The transport bar's record control: one REC button with three states, a live
 * input meter, and a way out.
 *
 * ```text
 *   idle        [REC]                         click -> arm
 *   armed       [REC] ▮▮▯▯▯ [x]               click -> record
 *   recording   [REC] 0:07 ▮▮▮▯▯ [x]          click -> stop, keep the take
 * ```
 *
 * # Why arming is a state you can see
 * WAVE opens the capture device only while armed -- holding a microphone open
 * all session is what stops a conferencing app from working and lights the OS
 * privacy indicator for nothing. That makes arming visible whether we draw it or
 * not, so it is drawn: the meter is the proof that the right input is selected
 * and the level is right, *before* the take rather than after it.
 *
 * # Why it is quiet
 * The meter and the counter appear only while the device is open, and take
 * about 50 px between them. A permanent recording panel would cost that space in
 * every session that never records one note. Per AGENTS.md: the control earns
 * its space by being information-bearing exactly when there is information.
 *
 * Below `view.recordBar` the readout is dropped entirely and only the button
 * remains: at that width the bar has no spare pixels, and a readout that pushed
 * the view tabs off the edge would cost more than it tells. The button still
 * shows armed and recording, the status bar reports the take, and Settings has a
 * full-width level meter for setting one.
 *
 * The meter and the counter are painted on the shared frame loop rather than
 * from signals -- they move continuously and must not drive the reactive
 * scheduler, the same rule the clock and the mixer meters follow.
 */

import { Show, type JSX } from 'solid-js';

import { peakFraction } from '../panels/mixer/scale';
import {
  cancelTitle,
  formatElapsed,
  nearLimit,
  recordAction,
  recordTitle,
} from '../lib/recording';
import { useAppState } from '../state';
import { useView } from './view';
import { onFrame } from '../ui/frameLoop';
import { IconRecord } from '../ui/icons';

export function RecordControl(): JSX.Element {
  const app = useAppState();
  const view = useView();

  const status = () => app.inputStatus();
  // The status carries `hasDevices`, but it is only refreshed while the device
  // is open; the enumerated list is what is current while idle.
  const hasDevices = () => app.inputDevices().length > 0 || (status()?.hasDevices ?? false);
  const action = () => recordAction(status(), hasDevices());
  const open = () => action() === 'start' || action() === 'stop';

  let rmsEl: HTMLSpanElement | undefined;
  let peakEl: HTMLSpanElement | undefined;
  let clockEl: HTMLSpanElement | undefined;

  onFrame(() => {
    const current = status();
    if (!current) return;
    if (rmsEl) rmsEl.style.width = `${peakFraction(current.rms) * 100}%`;
    if (peakEl) peakEl.style.left = `${peakFraction(current.peak) * 100}%`;
    if (clockEl) clockEl.textContent = formatElapsed(current.elapsedSeconds);
  });

  function onRec(): void {
    switch (action()) {
      case 'unavailable':
        return;
      case 'arm':
        void app.setInputArmed(true);
        return;
      case 'start':
        void app.startRecording();
        return;
      case 'stop':
        void app.finishRecording().then((channel) => {
          // One gesture: record, and the take is a playable channel with its
          // editor already up. `null` means the take was not kept, and the
          // reason is already in the status bar.
          if (channel !== null) view.openSampleEditor(channel);
        });
    }
  }

  function onCancel(): void {
    if (action() === 'stop') void app.cancelRecording();
    else void app.setInputArmed(false);
  }

  return (
    <>
      <button
        type="button"
        class="wv-tbtn wv-tbtn--rec wv-can-disable"
        classList={{
          'is-armed': action() === 'start',
          'is-recording': action() === 'stop',
        }}
        disabled={action() === 'unavailable'}
        aria-pressed={open()}
        title={recordTitle(status(), hasDevices())}
        onClick={onRec}
      >
        <IconRecord />
      </button>

      <Show when={open() && view.recordBar()}>
        <div class="wv-rec" classList={{ 'is-recording': action() === 'stop' }}>
          <Show when={action() === 'stop'}>
            <span
              class="wv-rec__clock"
              classList={{ 'is-near-limit': nearLimit(status()) }}
              ref={clockEl}
              title={
                nearLimit(status())
                  ? `Approaching the ${Math.round((status()?.maxSeconds ?? 0) / 60)}-minute take limit`
                  : 'Length of this take'
              }
            >
              0:00
            </span>
          </Show>
          <span
            class="wv-rec__meter"
            title={`Input level — ${status()?.deviceName || 'capture device'}`}
            aria-hidden="true"
          >
            <span class="wv-rec__rms" ref={rmsEl} />
            <span class="wv-rec__peak" ref={peakEl} />
          </span>
          <button
            type="button"
            class="wv-rec__cancel"
            title={cancelTitle(status())}
            aria-label={cancelTitle(status())}
            onClick={onCancel}
          >
            ×
          </button>
        </div>
      </Show>
    </>
  );
}
