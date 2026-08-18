import { Dynamic } from 'solid-js/web';
import type { JSX } from 'solid-js';
import type { PanelManifestDto } from '../api/types';
import { resolvePanel } from './registry';
import { UnknownPanel } from './UnknownPanel';

/**
 * Chrome around a single contributed panel: title bar plus the resolved body.
 * Resolution failure is contained here so one stale manifest cannot blank the
 * surrounding dock.
 */
export function PanelFrame(props: { manifest: PanelManifestDto }): JSX.Element {
  const component = () => resolvePanel(props.manifest.component) ?? UnknownPanel;
  const known = () => resolvePanel(props.manifest.component) !== undefined;

  return (
    <section class="panel" classList={{ 'panel--unknown': !known() }}>
      <header class="panel__header">
        <h2 class="panel__title">{props.manifest.title}</h2>
        <span class="panel__key">{props.manifest.component}</span>
      </header>
      <div class="panel__body">
        <Dynamic component={component()} manifest={props.manifest} />
      </div>
    </section>
  );
}
