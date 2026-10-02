import type { PlayMode } from '../api/types';

/** FL-style Stop rewinds; Pause preserves the engine's exact position. */
export function createTransportControl(commands: {
  mode: () => PlayMode;
  setMode: (mode: PlayMode) => Promise<void>;
  play: () => Promise<void>;
  stop: () => Promise<void>;
  seek: (position: number) => Promise<void>;
  isPlaying: () => boolean;
  position: () => number;
  samplesPerBeat: () => number;
  playing: (value: boolean) => void;
  move: (position: number, playing: boolean) => void;
  report: (error: unknown) => void;
}) {
  const starts: Record<PlayMode, number> = { pat: 0, song: 0 };
  let pending = Promise.resolve();
  let queued = 0;
  let modeQueued = 0;
  let target = commands.mode();
  let intent = 0;
  let seekIntent = 0;
  let paused = false;
  let failures = 0;

  const startAt = (mode: PlayMode) => Math.round(starts[mode] * commands.samplesPerBeat());

  function enqueue(run: () => Promise<void>): void {
    queued += 1;
    pending = pending.then(run).catch((error: unknown) => {
      failures += 1;
      paused = false;
      commands.playing(false);
      commands.report(error);
    }).finally(() => { queued -= 1; });
  }

  function setMode(mode: PlayMode): void {
    const alreadySelected = modeQueued > 0 ? target === mode : commands.mode() === mode;
    target = mode;
    if (alreadySelected) return;
    const requested = ++intent;
    const resume = commands.isPlaying();
    paused = false;
    modeQueued += 1;
    enqueue(async () => {
      try {
        // Do not briefly play the new scope at the old scope's sample position.
        if (resume) await commands.stop();
        await commands.setMode(mode);
        const start = startAt(mode);
        await commands.seek(start);
        if (requested !== intent) return;
        commands.move(start, resume);
        if (resume) await commands.play();
      } finally {
        modeQueued -= 1;
      }
    });
  }

  function play(): void {
    const requested = ++intent;
    const mode = target;
    const resume = paused;
    const previousFailures = failures;
    paused = false;
    commands.playing(true);
    enqueue(async () => {
      if (requested !== intent) return;
      // A failed mode command must never start the other editor's music.
      if (commands.mode() !== mode || failures !== previousFailures) {
        commands.playing(false);
        return;
      }
      const start = startAt(mode);
      if (!resume) await commands.seek(start);
      if (requested !== intent) return;
      await commands.play();
      if (requested === intent) commands.move(resume ? commands.position() : start, true);
    });
  }

  function stop(): void {
    intent += 1;
    const start = startAt(target);
    paused = false;
    commands.playing(false);
    commands.move(start, false);
    enqueue(async () => {
      await commands.stop();
      await commands.seek(start);
    });
  }

  function pause(): void {
    intent += 1;
    paused = true;
    const position = commands.position();
    commands.playing(false);
    commands.move(position, false);
    enqueue(() => commands.stop());
  }

  function seek(position: number): void {
    const sample = Number.isFinite(position) ? Math.max(0, Math.round(position)) : 0;
    const requested = ++seekIntent;
    // Start markers belong to musical time, so tempo/device changes keep the bar.
    starts[target] = sample / commands.samplesPerBeat();
    // Preserve Pause, so Ctrl+Space resumes at a cursor moved while paused.
    commands.move(sample, commands.isPlaying());
    enqueue(async () => {
      if (requested === seekIntent) await commands.seek(sample);
    });
  }

  return {
    setMode, play, stop, pause, seek,
    busy: () => queued > 0,
    resetStart(mode: PlayMode): void { starts[mode] = 0; },
    reset(): void {
      starts.pat = 0;
      starts.song = 0;
      seekIntent += 1;
      stop();
    },
  };
}
