import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { useLayout } from './layout';
import { trackColor } from '../theme/tokens';
import { IconChevron, IconPopOut } from '../ui/icons';
import type { ClapTarget, MasterDto, MissingPlugin, TrackDto } from '../api/types';

/**
 * The right pane: what is selected, and what is on it.
 *
 * MYDAW's device model is deliberately small today -- one instrument at the head
 * of each track, one effect on the master insert -- and this pane shows exactly
 * that rather than a chain it cannot hold. The mock's per-track effect chain,
 * its layer stack and its sends are drawn as disabled sections so the shape of
 * the finished thing is visible, with a tooltip that says which of them the
 * engine does not do yet.
 *
 * The one control that matters here is the pop-out: a hosted plugin's editor is
 * a **native OS window**, not a region of this webview, so this button asks Rust
 * to open one and nothing is drawn in-app. A built-in instrument has no editor
 * at all, and its button says so rather than failing when pressed.
 */
export function Inspector(): JSX.Element {
  const layout = useLayout();

  return (
    <Show when={layout.inspectorOpen()} fallback={<InspectorRail />}>
      <InspectorBody />
    </Show>
  );
}

function InspectorRail(): JSX.Element {
  const layout = useLayout();
  return (
    <button
      type="button"
      class="wv-rail wv-rail--right"
      title="Show the inspector"
      aria-label="Show the inspector"
      onClick={() => layout.toggleInspector()}
    >
      <IconChevron direction="left" />
      <span class="wv-rail__label">INSPECTOR</span>
    </button>
  );
}

function InspectorBody(): JSX.Element {
  const app = useAppState();
  const layout = useLayout();

  const selection = () => app.selection();
  const track = () => {
    const current = selection();
    return current.kind === 'track' ? app.mixer()?.tracks[current.index] : undefined;
  };
  const master = () => app.mixer()?.master;

  const isMaster = () => selection().kind === 'master';

  const name = () => (isMaster() ? 'Master' : (track()?.name ?? 'no selection'));
  const kind = () => (isMaster() ? 'master' : 'track');
  const dotColor = () => {
    const current = track();
    return isMaster() ? 'var(--txt2)' : current ? trackColor(current.id) : 'var(--dim)';
  };
  const meta = () => {
    if (isMaster()) return 'sum of all channels';
    const current = track();
    if (!current) return '';
    return `channel ${current.index} · out master`;
  };

  return (
    <aside class="wv-inspector">
      <div class="wv-pane-head">
        <span class="wv-pane-head__title">INSPECTOR</span>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-pane-head__btn"
          title="Collapse the inspector"
          aria-label="Collapse the inspector"
          onClick={() => layout.toggleInspector()}
        >
          <IconChevron direction="right" />
        </button>
      </div>

      <div class="wv-inspector__sel">
        <div class="wv-inspector__selrow">
          <span class="wv-inspector__dot" style={{ background: dotColor() }} />
          <span class="wv-inspector__name" title={name()}>
            {name()}
          </span>
          <span class="wv-inspector__kind">{kind()}</span>
        </div>
        <span class="wv-inspector__meta">{meta()}</span>
      </div>

      <div class="wv-inspector__body">
        <Show when={!isMaster() && track()}>{(current) => <InstrumentCard track={current()} />}</Show>

        <Show when={isMaster() && master()}>{(current) => <MasterInsertCard master={current()} />}</Show>

        <div class="wv-card wv-disabled" title="Effect chains — coming soon">
          <div class="wv-card__head">
            <span class="wv-card__label">
              {isMaster() ? 'MASTER CHAIN' : 'TRACK EFFECTS'}
            </span>
          </div>
          <div class="wv-card__body">
            <span class="wv-note" style={{ padding: 0 }}>
              MYDAW holds one effect, on the master insert. Per-track chains are
              coming.
            </span>
          </div>
        </div>

        <div class="wv-card wv-disabled" title="Sends and return buses — coming soon">
          <div class="wv-card__head">
            <span class="wv-card__label">SENDS TO RETURNS</span>
          </div>
          <div class="wv-card__body">
            <span class="wv-note" style={{ padding: 0 }}>
              No return buses yet.
            </span>
          </div>
        </div>
      </div>

      <div class="wv-inspector__foot">
        <Show when={isMaster() ? insertName(master()) : track()?.sourceLabel}>
          {(label) => (
            <span class="wv-chip" title={label()}>
              <span class="wv-chip__dot" style={{ background: '#4ec08a' }} />
              <span class="wv-chip__label">{label()}</span>
            </span>
          )}
        </Show>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-adddevice"
          title="Put an effect on the master insert"
          onClick={() => {
            app.ensureDevices();
            layout.setPaletteOpen(true);
          }}
        >
          + Device <kbd>Ctrl+K</kbd>
        </button>
      </div>
    </aside>
  );
}

/** The name of whatever is on the master insert, or nothing. */
function insertName(master: MasterDto | undefined): string | undefined {
  return master?.clap?.name ?? master?.vst3?.name ?? undefined;
}

function InstrumentCard(props: { track: TrackDto }): JSX.Element {
  const app = useAppState();

  const target = (): ClapTarget => ({ slot: 'track', index: props.track.index });
  const slot = () => props.track.clap ?? props.track.vst3;
  const hosted = () => slot() !== null && slot() !== undefined;
  const hasEditor = () => slot()?.hasEditor === true;
  const guiOpen = () => slot()?.guiOpen === true;

  const buttonTitle = () => {
    if (!hosted()) return 'Built-in instruments have no editor window yet';
    if (!hasEditor()) return 'This plugin has no editor';
    return guiOpen() ? 'Close the plugin window' : 'Open the plugin window';
  };

  return (
    <div class="wv-card">
      <div class="wv-card__head">
        <span class="wv-card__label">INSTRUMENT</span>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-card__btn"
          classList={{ 'wv-disabled': !hosted() || !hasEditor() }}
          disabled={!hosted() || !hasEditor()}
          title={buttonTitle()}
          aria-label={buttonTitle()}
          onClick={() => app.toggleGui(target(), !guiOpen())}
        >
          <IconPopOut />
        </button>
      </div>
      <div class="wv-card__body">
        <div class="wv-device">
          <span class="wv-device__dot" />
          <span class="wv-device__name">{props.track.sourceLabel}</span>
          <span class="wv-spacer" />
          <span class="wv-device__meta">
            {props.track.clap ? 'CLAP' : props.track.vst3 ? 'VST3' : 'built-in'}
          </span>
        </div>
        <Show when={props.track.missing}>
          {(missing) => <MissingBadge missing={missing()} />}
        </Show>
        <span class="wv-dashed wv-disabled" title="Instrument layers — coming soon">
          + Layer
        </span>
      </div>
    </div>
  );
}

function MasterInsertCard(props: { master: MasterDto }): JSX.Element {
  const app = useAppState();
  const layout = useLayout();

  const target = (): ClapTarget => ({ slot: 'master' });
  const slot = () => props.master.clap ?? props.master.vst3;
  const hasEditor = () => slot()?.hasEditor === true;
  const guiOpen = () => slot()?.guiOpen === true;

  return (
    <div class="wv-card">
      <div class="wv-card__head">
        <span class="wv-card__label">MASTER INSERT</span>
        <span class="wv-spacer" />
        <Show when={slot()}>
          <button
            type="button"
            class="wv-card__btn"
            classList={{ 'wv-disabled': !hasEditor() }}
            disabled={!hasEditor()}
            title={
              hasEditor()
                ? guiOpen()
                  ? 'Close the plugin window'
                  : 'Open the plugin window'
                : 'This plugin has no editor'
            }
            onClick={() => app.toggleGui(target(), !guiOpen())}
          >
            <IconPopOut />
          </button>
        </Show>
      </div>
      <div class="wv-card__body">
        <Show
          when={slot()}
          fallback={<span class="wv-note" style={{ padding: 0 }}>Nothing on the insert.</span>}
        >
          {(current) => (
            <div class="wv-device">
              <span class="wv-device__dot" />
              <span class="wv-device__name">{current().name}</span>
              <span class="wv-spacer" />
              <span class="wv-device__meta">{props.master.clap ? 'CLAP' : 'VST3'}</span>
            </div>
          )}
        </Show>
        <Show when={props.master.missing}>
          {(missing) => <MissingBadge missing={missing()} />}
        </Show>
        <div style={{ display: 'flex', gap: '6px' }}>
          <button
            type="button"
            class="wv-dashed"
            onClick={() => {
              app.ensureDevices();
              layout.setPaletteOpen(true);
            }}
          >
            {slot() ? 'Replace…' : '+ Effect'}
          </button>
          <Show when={slot()}>
            <button
              type="button"
              class="wv-dashed"
              onClick={() => app.mixerCommand((api) => api.clearMasterEffect())}
            >
              Remove
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
}

/** The "this project asked for a plugin it did not get" badge. */
function MissingBadge(props: { missing: MissingPlugin }): JSX.Element {
  return (
    <div
      class="wv-warn"
      title={`${props.missing.path}\n${props.missing.reason}`}
      role="status"
    >
      <span>
        {props.missing.name} ({props.missing.format}) could not be loaded
      </span>
    </div>
  );
}
