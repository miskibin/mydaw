/**
 * Open a pattern's editor from anywhere in the shell.
 *
 * The destination (rack vs roll) is a pure function of the pattern's contents
 * and the surface it was opened from; this hook is the one place that then
 * talks to the engine and `view.tsx`.
 */

import { patternEditorTarget, rollChannelFromStrip, type PatternNavSurface } from '../lib/navigate';
import { useAppState } from '../state';
import { useView } from './view';

export function usePatternNav(): {
  openPattern: (
    patternId: number,
    opts: { altKey: boolean; surface: PatternNavSurface },
  ) => Promise<void>;
} {
  const app = useAppState();
  const view = useView();

  return {
    async openPattern(patternId, opts) {
      const summary = app.patterns().find((row) => row.id === patternId);
      const target = patternEditorTarget(
        { hitCount: summary?.hitCount ?? 0, noteCount: summary?.noteCount ?? 0 },
        opts,
      );
      await app.sequencerCommand((backend) => backend.setActivePattern(patternId));
      const channel = rollChannelFromStrip(view.strip(), app.mixer()?.selected ?? 0);
      if (target === 'roll') {
        app.promoteStepsToRoll(channel);
        view.openRoll(channel);
        return;
      }
      view.go('rack');
    },
  };
}
