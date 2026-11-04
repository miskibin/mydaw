/**
 * The transport bar's record control: FL's record mode on one button, a live
 * input meter, input monitoring, and a way out.
 *
 * ```text
 *   idle        [●]                              click/R -> arm
 *   armed       [●] ▮▮▯▯▯ [🎧] [x]               Space records; click/R -> disarm
 *   recording   [●] 0:07 ▮▮▮▯▯ [🎧] [x]          Space stops and lands the take
 * ```
 *
 * # Why arming is a state you can see
 * WAVE opens the capture device only while armed -- holding a microphone open
 * all session is what stops a conferencing app from working and lights the OS
 * privacy indicator for nothing. That makes arming visible whether we draw it or
 * not, so it is drawn: the meter is the proof that the right input is selected
 * and the level is right, *before* the take rather than after it.
 *
 * # Monitoring
 * The headphones toggle is the one setting a singer changes mid-session: on
 * when the interface has no direct monitoring (you hear yourself through the
 * Vocal channel and its effects), off when it does (hearing yourself twice,
 * once late, is worse). It is remembered.
 *
 * The meter and the counter are painted on the shared frame loop rather than
 * from signals -- they move continuously and must not drive the reactive
 * scheduler, the same rule the clock and the mixer meters follow.
 */

import { Show, type JSX } from 'solid-js';

import { peakFraction } from '../panels/mixer/scale';
import { cancelTitle, formatElapsed, nearLimit, recordAction, recordTitle } from '../lib/recording';
import { useAppState } from '../state';
import { useView } from './view';
import { onFrame } from '../ui/frameLoop';
import { IconRecord } from '../ui/icons';

function IconHeadphones(): JSX.Element {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
      <path d="M2.5 11V8.5a5.5 5.5 0 0 1 11 0V11" />
      <rect x="2" y="9.5" width="3" height="4.5" rx="1" fill="currentColor" />
      <rect x="11" y="9.5" width="3" height="4.5" rx="1" fill="currentColor" />
    </svg>
  );
}

export function RecordControl(): JSX.Element {
  const app = useAppState();
  const view = useView();

  const status = () => app.inputStatus();
  const action = () => recordAction(status(), app.hasInputDevices(), app.playing());
  const state = () => status()?.state ?? 'idle';
  const open = () => state() !== 'idle';
  const recording = () => state() === 'recording';
  const monitoring = () => status()?.monitoring ?? true;

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

  function onCancel(): void {
    if (recording()) void app.cancelRecording();
    else void app.setInputArmed(false);
  }

  return (
    <>
      <button
        type="button"
        class="wv-tbtn wv-tbtn--rec wv-can-disable"
        classList={{
          'is-armed': state() === 'armed',
          'is-recording': recording(),
        }}
        disabled={action() === 'unavailable'}
        aria-pressed={open()}
        aria-label="Record"
        title={recordTitle(status(), app.hasInputDevices(), app.playing())}
        onClick={() => app.toggleRecord()}
      >
        <IconRecord />
      </button>

      <Show when={open() && view.recordBar()}>
        <div class="wv-rec" classList={{ 'is-recording': recording() }}>
          <Show when={recording()}>
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
            class="wv-rec__monitor"
            classList={{ 'is-on': monitoring() }}
            aria-pressed={monitoring()}
            aria-label="Input monitoring"
            title={
              monitoring()
                ? 'Monitoring on — you hear the input through the Vocal channel. Turn off if your interface monitors directly.'
                : 'Monitoring off — turn on to hear yourself through WAVE (with the Vocal channel’s effects)'
            }
            onClick={() => void app.setInputMonitoring(!monitoring())}
          >
            <IconHeadphones />
          </button>
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
