import type { Component } from 'solid-js';
import type { PanelManifestDto } from '../api/types';

/** Every registered panel receives its own manifest and nothing else. */
export interface PanelProps {
  manifest: PanelManifestDto;
}

export type PanelComponent = Component<PanelProps>;
