/**
 * The export job's lifecycle, owned by `state.tsx` so the dialog is a view.
 *
 * settings → (native save dialog) → running → completed | failed | cancelled.
 * The render runs in a worker process; this only polls its status, so the UI
 * never blocks and the live session keeps playing.
 */

import { createSignal, type Accessor } from 'solid-js';

import type { Backend } from '../api/backend';
import type { ExportJobStatus, ExportOptions } from '../api/types';
import { sanitizeExportOptions } from './exportWav';

export const EXPORT_POLL_MS = 200;
const OPTIONS_KEY = 'wave.export.options';

export type ExportPhase = 'settings' | 'choosing' | 'running' | 'completed' | 'failed' | 'cancelled';

export interface ExportController {
  open: Accessor<boolean>;
  /** Show the dialog. A finished job is cleared so it opens on the settings. */
  show(): void;
  /** Close it. Refused while a file is being chosen or rendered. */
  hide(): void;
  options: Accessor<ExportOptions>;
  setOptions(patch: Partial<ExportOptions>): void;
  phase: Accessor<ExportPhase>;
  job: Accessor<ExportJobStatus | null>;
  /** Why the export could not start or finish, worded for the dialog. */
  error: Accessor<string | null>;
  /** Ask for the file, then render. Resolves once the worker has started (or not). */
  start(): Promise<void>;
  cancel(): Promise<void>;
  /** Back to the settings after a finished job, keeping the options. */
  reset(): void;
  dispose(): void;
}

type ExportBackend = Pick<Backend, 'exportAudio' | 'getExportStatus' | 'cancelExport'>;

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function loadOptions(): ExportOptions {
  try {
    const raw = localStorage.getItem(OPTIONS_KEY);
    return sanitizeExportOptions(raw === null ? null : JSON.parse(raw));
  } catch {
    return sanitizeExportOptions(null);
  }
}

function saveOptions(options: ExportOptions): void {
  try {
    localStorage.setItem(OPTIONS_KEY, JSON.stringify(options));
  } catch {
    // A remembered setting is a convenience; private mode simply forgets it.
  }
}

export function createExportController(
  backend: ExportBackend,
  onFinished: (status: ExportJobStatus) => void = () => {},
  pollMs = EXPORT_POLL_MS,
): ExportController {
  const [open, setOpen] = createSignal(false);
  const [options, setOptionsSignal] = createSignal<ExportOptions>(loadOptions());
  const [phase, setPhase] = createSignal<ExportPhase>('settings');
  const [job, setJob] = createSignal<ExportJobStatus | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;

  const busy = () => phase() === 'choosing' || phase() === 'running';

  function finish(status: ExportJobStatus): void {
    setJob(status);
    setPhase(status.status === 'running' ? 'running' : status.status);
    if (status.status === 'failed') setError(status.error ?? 'the export failed');
    if (status.status !== 'running') onFinished(status);
  }

  function poll(jobId: string): void {
    clearTimeout(timer);
    if (disposed) return;
    timer = setTimeout(() => {
      backend
        .getExportStatus(jobId)
        .then((status) => {
          if (disposed || job()?.jobId !== jobId) return;
          finish(status);
          if (status.status === 'running') poll(jobId);
        })
        .catch((reason: unknown) => {
          if (disposed || job()?.jobId !== jobId) return;
          finish({ ...job()!, status: 'failed', error: message(reason) });
        });
    }, pollMs);
  }

  function reset(): void {
    if (busy()) return;
    setJob(null);
    setError(null);
    setPhase('settings');
  }

  return {
    open,
    show() {
      if (!busy()) reset();
      setOpen(true);
    },
    hide() {
      if (busy()) return;
      setOpen(false);
    },
    options,
    setOptions(patch) {
      if (busy()) return;
      const next = { ...options(), ...patch };
      setOptionsSignal(next);
      saveOptions(next);
    },
    phase,
    job,
    error,
    async start() {
      if (busy()) return;
      setError(null);
      setJob(null);
      setPhase('choosing');
      try {
        const started = await backend.exportAudio(options());
        if (disposed) return;
        if (started === null) {
          setPhase('settings');
          return;
        }
        finish(started);
        if (started.status === 'running') poll(started.jobId);
      } catch (reason) {
        if (disposed) return;
        setPhase('settings');
        setError(message(reason));
      }
    },
    async cancel() {
      const current = job();
      if (phase() !== 'running' || current === null) return;
      try {
        await backend.cancelExport(current.jobId);
      } catch (reason) {
        setError(message(reason));
      }
      // The worker's own report says when it has stopped; keep polling.
    },
    reset,
    dispose() {
      disposed = true;
      clearTimeout(timer);
    },
  };
}
