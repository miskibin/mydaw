/**
 * The webview half of the spike-4 IPC benchmark (`app/src/ipc_bench.rs`).
 *
 * Dev-only. Nothing imports this module unless the Rust side contributed the
 * `ipc-bench` panel, which takes `MYDAW_DEV_PANELS=1` or `MYDAW_IPC_BENCH=1`; in
 * a normal run Vite keeps it in a chunk the app never fetches.
 *
 * What it does, in order:
 *
 * 1. **Synchronise the clocks.** `performance.now()` and Rust's `Instant` epoch
 *    are unrelated. A ping handshake measures the offset, and *Rust* computes it
 *    (`bench_sync`) so there is exactly one implementation of that arithmetic,
 *    and it is the one with tests. Every latency here is one-way, accurate to
 *    the +/- error bar the sync reports.
 * 2. **Run each scenario.** Rust streams N frames of a given size at a given
 *    rate; every frame carries its send timestamp, and the receive timestamp is
 *    taken as the first statement of the message handler.
 * 3. **Measure the real meter path**, with the live `subscribe_meters` stream
 *    and the pump's own timestamp, so the app's actual meter latency is a
 *    measurement and not an extrapolation.
 * 4. **Hand the raw samples to Rust**, which aggregates, writes
 *    `.debug/ipc-bench/<run>.json` and prints the report.
 */

import type { MeterFrame, Unsubscribe } from '../api/types';

// ---------------------------------------------------------------------------
// Wire types, mirrored from `app/src/ipc_bench.rs`
// ---------------------------------------------------------------------------

export type BenchTransport = 'channel-json' | 'channel-binary';

export interface StreamConfig {
  transport: BenchTransport;
  payloadBytes: number;
  rateHz: number;
  durationMs: number;
}

export interface EpochSample {
  jsBeforeMs: number;
  rustMs: number;
  jsAfterMs: number;
}

export interface EpochSync {
  offsetMs: number;
  rttMinMs: number;
  rttMedianMs: number;
  errorMs: number;
  samples: number;
}

export interface ScenarioSamples {
  label: string;
  transport?: BenchTransport;
  payloadBytes: number;
  targetRateHz: number;
  durationMs: number;
  planned: number;
  latenciesMs: number[];
  firstRecvMs: number;
  lastRecvMs: number;
  longTasks: number;
  longTaskMs: number;
  note?: string;
}

export interface LatencyStats {
  count: number;
  minMs: number;
  meanMs: number;
  p50Ms: number;
  p95Ms: number;
  p99Ms: number;
  maxMs: number;
}

export interface ScenarioReport {
  label: string;
  transport: BenchTransport | null;
  payloadBytes: number;
  targetRateHz: number;
  durationMs: number;
  planned: number;
  received: number;
  dropped: number;
  achievedRateHz: number;
  throughputMbS: number;
  longTasks: number;
  longTaskMs: number;
  stats: LatencyStats | null;
  note: string | null;
}

export interface BenchReport {
  runId: string;
  environment: {
    userAgent: string;
    hardwareConcurrency: number | null;
    devicePixelRatio: number | null;
    webviewVersion: string | null;
    os: string;
    arch: string;
    profile: string;
    appVersion: string;
  };
  epoch: EpochSync | null;
  rtt: LatencyStats | null;
  rttAsync: LatencyStats | null;
  scenarios: ScenarioReport[];
}

// ---------------------------------------------------------------------------
// Knobs
// ---------------------------------------------------------------------------

/** Pings in the clock handshake. Enough that one scheduling hiccup cannot set the floor. */
export const EPOCH_PINGS = 40;
/** Extra `bench_ping_async` round trips, to price the worker-pool path. */
export const ASYNC_PINGS = 40;
/** Quiet time between scenarios, so one scenario's backlog is not the next one's latency. */
export const SETTLE_MS = 300;
/** A scenario is drained once this long passes with no frame arriving. */
export const DRAIN_QUIET_MS = 250;
/** Cap on draining, so one wedged transport cannot stall the whole matrix. */
export const MAX_DRAIN_MS = 10_000;
/** How long the live meter path is watched. */
export const METER_SCENARIO_MS = 5_000;
/** Label of the live meter scenario, referenced by the panel and the spec. */
export const METER_SCENARIO_LABEL = 'meter-path (live)';
/** Quiet period before an unattended run starts measuring. */
export const AUTO_START_DELAY_MS = 2_500;

type TauriCore = typeof import('@tauri-apps/api/core');

/** `@tauri-apps/api` is imported lazily, exactly as `api/backend.ts` does it. */
async function core(): Promise<TauriCore> {
  return import('@tauri-apps/api/core');
}

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------------
// Frame decoding
// ---------------------------------------------------------------------------

/** What a received frame carries, whichever transport delivered it. */
export interface DecodedFrame {
  seq: number;
  sentMs: number;
}

/**
 * Read a frame regardless of transport.
 *
 * A binary frame always arrives as an `ArrayBuffer` (Tauri either evals a
 * `Uint8Array(...).buffer` literal for small payloads or fetches
 * `application/octet-stream` for large ones); a JSON frame arrives as a parsed
 * object. Anything else is a bug worth seeing, so it decodes to `null` and is
 * counted as a malformed frame rather than silently averaged in.
 */
export function decodeFrame(message: unknown): DecodedFrame | null {
  if (message instanceof ArrayBuffer) {
    if (message.byteLength < 16) return null;
    const view = new DataView(message);
    return { seq: view.getUint32(0, true), sentMs: view.getFloat64(4, true) };
  }
  if (ArrayBuffer.isView(message)) {
    const view = new DataView(message.buffer, message.byteOffset, message.byteLength);
    if (view.byteLength < 16) return null;
    return { seq: view.getUint32(0, true), sentMs: view.getFloat64(4, true) };
  }
  if (typeof message === 'object' && message !== null) {
    const record = message as Record<string, unknown>;
    if (typeof record.seq === 'number' && typeof record.sentMs === 'number') {
      return { seq: record.seq, sentMs: record.sentMs };
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Main-thread health
// ---------------------------------------------------------------------------

/**
 * Counts `longtask` entries (>50 ms of blocked main thread) while a scenario
 * runs. A transport that is fast on average but freezes the UI twice a second
 * is not usable for a DAW, and this is what makes that visible.
 *
 * `PerformanceObserver` with the `longtask` type is not guaranteed everywhere,
 * so construction failure degrades to "0 long tasks observed" rather than
 * failing the run.
 */
export class LongTaskCounter {
  private observer: PerformanceObserver | undefined;
  count = 0;
  totalMs = 0;

  start(): void {
    this.count = 0;
    this.totalMs = 0;
    try {
      this.observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          this.count += 1;
          this.totalMs += entry.duration;
        }
      });
      this.observer.observe({ type: 'longtask', buffered: false });
    } catch {
      this.observer = undefined;
    }
  }

  stop(): void {
    this.observer?.disconnect();
    this.observer = undefined;
  }
}

// ---------------------------------------------------------------------------
// Clock handshake
// ---------------------------------------------------------------------------

export interface ClockHandshake {
  sync: EpochSync;
  samples: EpochSample[];
  rttMs: number[];
  rttAsyncMs: number[];
}

/**
 * Measure the Rust/JS clock offset and the bare invoke round trip.
 *
 * Both command flavours are timed: `bench_ping` is synchronous (Tauri runs it on
 * the main thread, like most UI commands) and `bench_ping_async` goes to the
 * worker pool. The offset is computed from the sync pings only, so it is not
 * contaminated by the pool's scheduling.
 *
 * @throws if Rust cannot derive an offset from the handshake at all.
 */
export async function syncClocks(pings = EPOCH_PINGS): Promise<ClockHandshake> {
  const { invoke } = await core();
  const samples: EpochSample[] = [];
  const rttMs: number[] = [];

  // One warm-up round trip: the first invoke of a session pays for module init
  // and would otherwise be the outlier that widens every error bar.
  await invoke<number>('bench_ping');

  for (let i = 0; i < pings; i += 1) {
    const jsBeforeMs = performance.now();
    const rustMs = await invoke<number>('bench_ping');
    const jsAfterMs = performance.now();
    samples.push({ jsBeforeMs, rustMs, jsAfterMs });
    rttMs.push(jsAfterMs - jsBeforeMs);
  }

  const rttAsyncMs: number[] = [];
  await invoke<number>('bench_ping_async');
  for (let i = 0; i < ASYNC_PINGS; i += 1) {
    const before = performance.now();
    await invoke<number>('bench_ping_async');
    rttAsyncMs.push(performance.now() - before);
  }

  const sync = await invoke<EpochSync | null>('bench_sync', { samples });
  if (!sync) throw new Error('the clock handshake produced no usable samples');
  return { sync, samples, rttMs, rttAsyncMs };
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/** A scenario's label, matching the one Rust builds for the same config. */
export function scenarioLabel(config: StreamConfig): string {
  const bytes =
    config.payloadBytes % (1024 * 1024) === 0
      ? `${config.payloadBytes / (1024 * 1024)}MB`
      : config.payloadBytes % 1024 === 0
        ? `${config.payloadBytes / 1024}KB`
        : `${config.payloadBytes}B`;
  return `${config.transport}/${bytes}@${config.rateHz}Hz`;
}

/**
 * Run one streaming scenario end to end.
 *
 * Resolves when the last scheduled frame arrives, or when the grace period after
 * the nominal duration expires -- whichever comes first. The grace is generous
 * (the whole point is to catch a transport that falls behind), and a scenario
 * that ends on the timeout reports its shortfall as drops.
 */
export async function runScenario(config: StreamConfig, offsetMs: number): Promise<ScenarioSamples> {
  const { Channel, invoke } = await core();

  const latenciesMs: number[] = [];
  let firstRecvMs = 0;
  let lastRecvMs = 0;
  let malformed = 0;
  let planned = 0;
  let resolveDone: (() => void) | undefined;
  const done = new Promise<void>((resolve) => {
    resolveDone = resolve;
  });

  const channel = new Channel<unknown>();
  channel.onmessage = (message: unknown) => {
    const recvMs = performance.now();
    const frame = decodeFrame(message);
    if (!frame) {
      malformed += 1;
      return;
    }
    // The frame's stamp is Rust time; the offset moves it into this clock.
    latenciesMs.push(recvMs - (frame.sentMs - offsetMs));
    if (latenciesMs.length === 1) firstRecvMs = recvMs;
    lastRecvMs = recvMs;
    if (planned > 0 && latenciesMs.length >= planned) resolveDone?.();
  };

  const longTasks = new LongTaskCounter();
  longTasks.start();
  planned = await invoke<number>('bench_stream', { config, channel });

  // Grace: a transport that fell behind needs room to catch up -- "late" and
  // "lost" are different verdicts and the difference is worth waiting for -- but
  // a dead one must not hang the matrix. Two seconds flat, plus 100 ms for every
  // megabyte the scenario scheduled.
  const scheduledMb = (config.payloadBytes * config.rateHz * (config.durationMs / 1000)) / 1e6;
  const graceMs = 2_000 + 100 * scheduledMb;
  await Promise.race([done, sleep(config.durationMs + graceMs)]);
  longTasks.stop();

  // Drain before returning. A transport that fell behind still has frames queued
  // in the webview, and letting them land during the *next* scenario would
  // charge that scenario for this one's backlog. Counted, because "delivered,
  // but seconds late" is a distinct failure from "dropped".
  let late = 0;
  let lastSeenMs = performance.now();
  channel.onmessage = () => {
    late += 1;
    lastSeenMs = performance.now();
  };
  const drainDeadline = performance.now() + MAX_DRAIN_MS;
  while (performance.now() - lastSeenMs < DRAIN_QUIET_MS && performance.now() < drainDeadline) {
    await sleep(DRAIN_QUIET_MS / 2);
  }
  channel.onmessage = () => {};

  const notes: string[] = [];
  if (malformed > 0) notes.push(`${malformed} malformed frames`);
  if (late > 0) notes.push(`${late} frames arrived after the window`);
  if (latenciesMs.length + late < planned) {
    notes.push(`${planned - latenciesMs.length - late} never arrived`);
  }

  return {
    label: scenarioLabel(config),
    transport: config.transport,
    payloadBytes: config.payloadBytes,
    targetRateHz: config.rateHz,
    durationMs: config.durationMs,
    planned,
    latenciesMs,
    firstRecvMs,
    lastRecvMs,
    longTasks: longTasks.count,
    longTaskMs: longTasks.totalMs,
    // Spread rather than `note: undefined`: the project compiles with
    // `exactOptionalPropertyTypes`, where an absent key and an undefined one are
    // different things -- and serde on the other side agrees.
    ...(notes.length > 0 ? { note: notes.join('; ') } : {}),
  };
}

/** What the meter scenario needs from the app around it. */
export interface MeterDeps {
  subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe>;
  play(): Promise<void>;
  stop(): Promise<void>;
  /** Forward each frame to the app, so the meters keep working during the run. */
  onFrame?(frame: MeterFrame): void;
  /** Put the app's own meter subscription back afterwards. */
  restore?(): Promise<void>;
}

/**
 * Measure the *real* meter path: audio thread -> throttle -> pump -> channel ->
 * this callback, with the pump's own timestamp.
 *
 * Needs sound: the frames come from the audio callback, so the transport is
 * started for the duration and stopped again. On a machine with no output device
 * nothing arrives, and the scenario says so instead of reporting a zero.
 */
export async function runMeterScenario(
  deps: MeterDeps,
  offsetMs: number,
  durationMs = METER_SCENARIO_MS,
): Promise<ScenarioSamples> {
  const { invoke } = await core();
  const latenciesMs: number[] = [];
  let firstRecvMs = 0;
  let lastRecvMs = 0;
  let unstamped = 0;

  await invoke('bench_meter_stamp', { enabled: true });
  const longTasks = new LongTaskCounter();
  longTasks.start();

  const detach = await deps.subscribeMeters((frame) => {
    const recvMs = performance.now();
    deps.onFrame?.(frame);
    if (typeof frame.pumpMs !== 'number') {
      unstamped += 1;
      return;
    }
    latenciesMs.push(recvMs - (frame.pumpMs - offsetMs));
    if (latenciesMs.length === 1) firstRecvMs = recvMs;
    lastRecvMs = recvMs;
  });

  await deps.play();
  await sleep(durationMs);
  await deps.stop();
  detach();
  longTasks.stop();
  await invoke('bench_meter_stamp', { enabled: false });
  await deps.restore?.();

  const notes = ['live subscribe_meters path: 30 Hz throttle + 15 ms pump poll'];
  if (unstamped > 0) notes.push(`${unstamped} frames arrived before stamping took effect`);
  if (latenciesMs.length === 0) notes.push('no meter frames at all (no audio device?)');

  return {
    label: METER_SCENARIO_LABEL,
    payloadBytes: 96,
    targetRateHz: 30,
    durationMs,
    planned: 0,
    latenciesMs,
    firstRecvMs,
    lastRecvMs,
    longTasks: longTasks.count,
    longTaskMs: longTasks.totalMs,
    note: notes.join('; '),
  };
}

// ---------------------------------------------------------------------------
// The matrix
// ---------------------------------------------------------------------------

export interface MatrixOptions {
  /** Scenarios to run. Defaults to the plan Rust publishes. */
  plan?: StreamConfig[];
  /** Called before each scenario, for the panel's progress line. */
  onProgress?(label: string, index: number, total: number): void;
  /** Omit to skip the live meter measurement (a browser session, say). */
  meters?: MeterDeps;
}

/**
 * Run the whole matrix, then hand the raw samples to Rust for aggregation,
 * writing and reporting.
 *
 * Returns the aggregated report. In auto mode the app exits a beat after this
 * resolves -- that is Rust's doing, not this function's.
 */
export async function runMatrix(options: MatrixOptions = {}): Promise<BenchReport> {
  const { invoke } = await core();
  const plan = options.plan ?? (await invoke<StreamConfig[]>('bench_plan'));
  const handshake = await syncClocks();

  const scenarios: ScenarioSamples[] = [];
  const total = plan.length + (options.meters ? 1 : 0);
  let index = 0;
  for (const config of plan) {
    options.onProgress?.(scenarioLabel(config), index, total);
    scenarios.push(await runScenario(config, handshake.sync.offsetMs));
    index += 1;
    await sleep(SETTLE_MS);
  }

  if (options.meters) {
    options.onProgress?.(METER_SCENARIO_LABEL, plan.length, total);
    scenarios.push(await runMeterScenario(options.meters, handshake.sync.offsetMs));
  }

  return finishRun(scenarios, handshake);
}

/**
 * Hand a finished run to Rust: aggregation, the results file, the stdout report
 * and (in auto mode) the exit all happen there.
 *
 * The single exit point of this module -- one shape of `BenchRunInput`, whether
 * it came from the full matrix, one button in the panel, or a run that fell over
 * before it measured anything.
 */
export async function finishRun(
  scenarios: ScenarioSamples[],
  handshake?: ClockHandshake,
): Promise<BenchReport> {
  const { invoke } = await core();
  return invoke<BenchReport>('bench_finish', {
    run: {
      userAgent: navigator.userAgent,
      hardwareConcurrency: navigator.hardwareConcurrency ?? null,
      devicePixelRatio: window.devicePixelRatio ?? null,
      epochSamples: handshake?.samples ?? [],
      rttMs: handshake?.rttMs ?? [],
      rttAsyncMs: handshake?.rttAsyncMs ?? [],
      scenarios,
    },
  });
}

/**
 * The unattended entry point (`MYDAW_IPC_BENCH=1`): run everything, log
 * progress to the console, and let Rust write, print and exit.
 *
 * Never throws: a bench that fails must say so on stdout and let the app close,
 * not leave a window sitting open forever waiting for a human.
 */
export async function runAutoMatrix(meters?: MeterDeps): Promise<void> {
  try {
    // Let startup finish first. The CLAP scan the browser panel kicks off on
    // mount is the loudest thing this app ever does, and measuring the IPC
    // boundary through it would measure that instead.
    await sleep(AUTO_START_DELAY_MS);
    console.info('[ipc-bench] auto run starting');
    const report = await runMatrix({
      ...(meters ? { meters } : {}),
      onProgress: (label, index, total) =>
        console.info(`[ipc-bench] ${index + 1}/${total} ${label}`),
    });
    console.info(`[ipc-bench] done: ${report.runId}`);
  } catch (err) {
    console.error('[ipc-bench] auto run failed', err);
    try {
      // Still finish: an empty report that says "no clock sync" is a result an
      // agent can read, and it is what closes the window.
      await finishRun([]);
    } catch {
      // Rust is unreachable; the console line above is all there is to say.
    }
  }
}
