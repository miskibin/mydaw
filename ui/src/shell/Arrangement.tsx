import { For, Index, Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { trackColor } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { IconCutTool, IconDrawTool, IconSelectTool } from '../ui/icons';
import { currentTick, FALLBACK_SAMPLE_RATE, patternTicks } from '../lib/pattern';

/**
 * The arrangement: the track list, and the space the clips will go in.
 *
 * The **left column is real** -- these are the project's tracks, selecting one
 * moves the keyboard piano, and M and S are the engine's mute and solo. The
 * **lanes are not**: MYDAW has no arrangement timeline yet, only a looping
 * pattern, so there is nothing to draw in them and a clip rectangle here would
 * be a promise the app cannot keep. They are rendered disabled rather than
 * omitted, because the shape of the finished application is worth showing.
 *
 * The playhead over them *is* real. It runs across the width of one time round
 * the loop, driven by the same interpolated position the transport clock uses --
 * which is why the lane area is a truthful picture even while it is empty.
 */
export function Arrangement(): JSX.Element {
  const app = useAppState();

  const tracks = () => app.mixer()?.tracks ?? [];
  const bars = () => Math.max(1, Math.round((app.pattern()?.steps ?? 16) / 16));

  let playheadEl: HTMLDivElement | undefined;

  onFrame(() => {
    if (!playheadEl) return;
    const pattern = app.pattern();
    if (!pattern || !app.playing()) {
      playheadEl.style.opacity = '0';
      return;
    }
    const rate = app.playhead.getSampleRate() || FALLBACK_SAMPLE_RATE;
    const tick = currentTick(app.playhead.positionSamples(), rate, app.bpm(), pattern.steps);
    if (tick === null) {
      playheadEl.style.opacity = '0';
      return;
    }
    playheadEl.style.opacity = '1';
    playheadEl.style.left = `${(tick / patternTicks(pattern)) * 100}%`;
  });

  return (
    <div class="wv-arrange">
      <div class="wv-arrange__head">
        <span class="wv-arrange__title">ARRANGEMENT</span>
        <div class="wv-tools wv-disabled" title="Editing tools — coming soon">
          <span class="wv-tool is-active">
            <IconSelectTool />
          </span>
          <span class="wv-tool">
            <IconDrawTool />
          </span>
          <span class="wv-tool">
            <IconCutTool />
          </span>
        </div>
        <span class="wv-snap wv-disabled" title="Arrangement snap — coming soon">
          1/4
        </span>
        <span class="wv-spacer" />
        <span class="wv-arrange__sel">
          {bars()} {bars() === 1 ? 'bar' : 'bars'} · loop
        </span>
      </div>

      <div class="wv-sections">
        <div class="wv-sections__gutter">
          <span class="wv-sections__counts">
            {tracks().length} {tracks().length === 1 ? 'track' : 'tracks'}
          </span>
        </div>
        <div
          class="wv-sections__strip wv-disabled"
          title="Song sections — coming soon. These are the pattern's bars."
        >
          <div class="wv-sections__labels">
            <For each={Array.from({ length: bars() }, (_, index) => index)}>
              {(index) => (
                <div class="wv-section" style={{ flex: 1, 'margin-right': '2px' }}>
                  BAR {index + 1}
                </div>
              )}
            </For>
          </div>
          <div class="wv-sections__bars">
            <For each={Array.from({ length: bars() }, (_, index) => index)}>
              {(index) => <span class="wv-section__bar" style={{ flex: 1 }}>{index * 16 + 1}</span>}
            </For>
          </div>
        </div>
      </div>

      <div class="wv-arrange__rows">
        <div class="wv-trackcol">
          {/* `Index` rather than `For`: a mixer reply is a whole new array of
              new objects, and keying on identity would rebuild every row for a
              single mute. A row's identity is its position. */}
          <Index each={tracks()}>
            {(track) => {
              const selected = () => {
                const selection = app.selection();
                return selection.kind === 'track' && selection.index === track().index;
              };
              return (
                <div
                  class="wv-trackrow"
                  classList={{ 'is-selected': selected(), 'is-muted': track().muted }}
                  role="button"
                  tabindex="0"
                  aria-pressed={selected()}
                  onClick={() => app.select({ kind: 'track', index: track().index })}
                  onKeyDown={(event) => {
                    if (event.key !== 'Enter' && event.key !== ' ') return;
                    event.preventDefault();
                    app.select({ kind: 'track', index: track().index });
                  }}
                >
                  <span
                    class="wv-trackrow__bar"
                    style={{
                      background: trackColor(track().id),
                      opacity: track().muted ? 0.35 : 1,
                    }}
                  />
                  <span class="wv-trackrow__text">
                    <span class="wv-trackrow__name">{track().name}</span>
                    <span class="wv-trackrow__dev">{track().sourceLabel}</span>
                  </span>
                  <span class="wv-trackrow__btns">
                    <span class="wv-arm wv-disabled" title="Record arm — coming soon" />
                    <button
                      type="button"
                      class="wv-pill wv-pill--solo"
                      classList={{ 'is-on': track().soloed }}
                      title={`Solo ${track().name}`}
                      aria-pressed={track().soloed}
                      onClick={(event) => {
                        event.stopPropagation();
                        app.mixerCommand((api) =>
                          api.setTrackSolo(track().index, !track().soloed),
                        );
                      }}
                    >
                      S
                    </button>
                    <button
                      type="button"
                      class="wv-pill wv-pill--mute"
                      classList={{ 'is-on': track().muted }}
                      title={`Mute ${track().name}`}
                      aria-pressed={track().muted}
                      onClick={(event) => {
                        event.stopPropagation();
                        app.mixerCommand((api) =>
                          api.setTrackMute(track().index, !track().muted),
                        );
                      }}
                    >
                      M
                    </button>
                  </span>
                </div>
              );
            }}
          </Index>
          <div class="wv-trackcol__filler">
            <Show when={(app.mixer()?.tracks.length ?? 0) < (app.mixer()?.maxTracks ?? 0)}>
              <button
                type="button"
                class="wv-dashed"
                style={{ margin: '6px 10px', width: 'calc(100% - 20px)' }}
                onClick={() => app.addTrack()}
              >
                + Track
              </button>
            </Show>
          </div>
        </div>

        <div class="wv-lanes">
          <Index each={tracks()}>
            {(track) => {
              const selected = () => {
                const selection = app.selection();
                return selection.kind === 'track' && selection.index === track().index;
              };
              return (
                <div
                  class="wv-lane wv-disabled"
                  classList={{ 'is-selected': selected(), 'is-muted': track().muted }}
                  title="Clips and the arrangement timeline — coming soon"
                />
              );
            }}
          </Index>
          <div class="wv-playhead" ref={playheadEl} style={{ left: '0%', opacity: 0 }} />
        </div>
      </div>
    </div>
  );
}
