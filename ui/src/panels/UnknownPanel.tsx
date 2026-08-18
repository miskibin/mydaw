import type { JSX } from 'solid-js';
import type { PanelProps } from './types';

/**
 * Rendered when a plugin contributes a panel whose `component` key has no
 * entry in the registry -- i.e. the Rust side is newer than the bundled UI.
 * Failing visibly here beats silently dropping the panel.
 */
export function UnknownPanel(props: PanelProps): JSX.Element {
  return (
    <div class="unknown-panel">
      <div class="unknown-panel__badge">unknown panel</div>
      <p class="unknown-panel__text">
        No UI component is registered for <code>{props.manifest.component}</code>.
      </p>
      <p class="unknown-panel__hint">
        Contributed by <code>{props.manifest.id}</code>. Register the key in
        <code>src/panels/registry.ts</code>.
      </p>
    </div>
  );
}
