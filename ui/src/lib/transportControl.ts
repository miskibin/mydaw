import type { PlayMode } from '../api/types';

/** Order editor changes before Play, including rapid focus changes over IPC. */
export function createTransportControl(commands: {
  mode: () => PlayMode;
  setMode: (mode: PlayMode) => Promise<void>;
  play: () => Promise<void>;
  stop: () => Promise<void>;
  playing: (value: boolean) => void;
  report: (error: unknown) => void;
}): { setMode: (mode: PlayMode) => void; play: () => void; stop: () => void } {
  let pending = Promise.resolve();
  let queued = 0;
  let target: PlayMode | null = null;
  let intent = 0;

  return {
    setMode(mode) {
      if (queued > 0 ? target === mode : commands.mode() === mode) return;
      target = mode;
      queued += 1;
      pending = pending.catch(() => {}).then(() => commands.setMode(mode));
      void pending.catch(commands.report).finally(() => {
        queued -= 1;
        if (queued === 0) target = null;
      });
    },
    play() {
      const requested = ++intent;
      commands.playing(true);
      void pending.then(() => {
        if (requested !== intent) return;
        return commands.play().catch((error: unknown) => {
          if (requested === intent) commands.playing(false);
          commands.report(error);
        });
      }, () => {
        if (requested === intent) commands.playing(false);
      });
    },
    stop() {
      intent += 1;
      commands.playing(false);
      void commands.stop().catch(commands.report);
    },
  };
}
