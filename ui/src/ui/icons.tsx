import type { FxKind } from '../api/types';
import { FX_ART } from './fxArt';
import type { TranslationKey } from '../lib/i18n';

export const ICONS = {
  draw: 'M4 20l4.5-1L19 8.5 15.5 5 5 15.5z M13.5 7l3.5 3.5',
  paint: 'M18 3l3 3-9 9-3-3z M9 12c-3 0-4 2-4 4s-1 3-2 4c4 0 8-1 8-5',
  erase: 'M8 20h12 M4.5 15.5L14 6l5 5-7.5 7.5H8z',
  mute: 'M4 9h4l5-4v14l-5-4H4z M17 9l4 6 M21 9l-4 6',
  slice: 'M12 2v20 M7 7l5-5 5 5',
  select: 'M4 4h4 M12 4h4 M20 4v4 M20 12v4 M20 20h-4 M12 20H8 M4 20v-4 M4 12V8',
  plus: 'M12 5v14 M5 12h14',
  gear: 'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z',
  vst: 'M4 6h16v12H4z M8 10v4 M12 9v6 M16 11v2',
};

export const FXMETA: Record<FxKind, { icon: string; color: string; /** translation key: show with t() */ tag: TranslationKey }> = {
  eq: { icon: 'M2 17h4c2.5 0 3.5-10 6-10s3.5 10 6 10h4', color: '#5aa9e6', tag: 'c.fxtag.tone' },
  comp: { icon: 'M4 12h16 M12 2v6 M9 5l3 3 3-3 M12 22v-6 M9 19l3-3 3 3', color: '#e0a44a', tag: 'c.fxtag.dynamics' },
  sat: { icon: 'M2 12c1.5-5 2.5-6 4.5-6h3c2 0 3 12 5 12h3c2 0 3-1 4.5-6', color: '#d9678f', tag: 'c.fxtag.warmth' },
  delay: { icon: 'M4 5v14 M9.5 8v11 M14.5 11v8 M19.5 14v5', color: '#7c8cf8', tag: 'c.fxtag.echo' },
  reverb: { icon: 'M4 12h.01 M8 8.5a5 5 0 0 1 0 7 M11.5 5.5a9 9 0 0 1 0 13 M15 2.5a13 13 0 0 1 0 19', color: '#47b8a8', tag: 'c.fxtag.space' },
  limit: { icon: 'M3 4h18 M3 20l3-12 3 12 3-12 3 12 3-12 3 12', color: '#e05263', tag: 'c.fxtag.loudness' },
  tune: { icon: 'M3 18h5v-5h5V8h8 M3 15.5c3 0 3.5-5.5 7-5.5s3.5-3.5 7-3.5', color: '#5fbf8a', tag: 'c.fxtag.tuning' },
  deess: { icon: 'M16.5 6.5C15.6 5 14 4.5 12 4.5c-2.8 0-4.5 1.4-4.5 3.4 0 4.6 9 2.8 9 7.8 0 2.2-2 3.8-4.6 3.8-2 0-3.6-.7-4.6-2.3 M3 12h18', color: '#6fd0e8', tag: 'c.fxtag.sibilance' },
  fet: { icon: 'M3.5 17a8.5 8.5 0 0 1 17 0 M12 17l4.5-6.5 M6.2 11.2l1.2 1 M17.8 11.2l-1.2 1 M12 8.5v1.5', color: '#f07f4f', tag: 'c.fxtag.fetComp' },
  denoise: { icon: 'M2 12h2l1.5-3 1.5 6 1.5-4 1.5 2h2 M13 12c1.5-4 3-6 4.5-6S20 9 22 12 M13 12c1.5 4 3 6 4.5 6S20 15 22 12', color: '#8fb3ff', tag: 'c.fxtag.denoising' },
  voice: { icon: 'M12 3a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z M5.5 11a6.5 6.5 0 0 0 13 0 M12 17.5V21 M17.5 3.5l3 2-3 2 M20.5 5.5h-2', color: '#c77ddb', tag: 'c.fxtag.voiceTimbre' },
  half: { icon: 'M2 12c1-4 2-4 3 0s2 4 3 0 M10 12c2.5-8 5-8 7 0s4.5 8 5 0', color: '#a3d65f', tag: 'c.fxtag.slowdown' },
  rider: { icon: 'M3 15c2 0 2-6 4-6s2 8 4 8 2-10 4-10 2 6 4 6h2 M3 12h18', color: '#e8c55a', tag: 'c.fxtag.leveling' },
  vst3: { icon: ICONS.vst, color: '#9a8cf0', tag: 'c.fxtag.plugin' },
};
export const FX_ORDER = ['denoise', 'tune', 'voice', 'eq', 'rider', 'deess', 'comp', 'fet', 'sat', 'delay', 'half', 'reverb', 'limit'] as const;
export const TC = ['#4f8ef7', '#5aa9e6', '#69c3d6', '#47b8a8', '#5fbf8a', '#7c8cf8', '#e0a44a', '#9a8cf0', '#d9678f', '#8b93a1'];

export function Icon(props: { d: string; size?: number; sw?: number }) {
  return (
    <svg width={props.size ?? 14} height={props.size ?? 14} viewBox="0 0 24 24" fill="none" stroke="currentColor"
      stroke-width={props.sw ?? 2} stroke-linecap="round" stroke-linejoin="round"><path d={props.d} /></svg>
  );
}

/** Coloured flat effect icon (no background); `dim` greys it out for bypassed effects. */
export function FxTile(props: { type: FxKind; size: number; icon?: number; dim?: boolean }) {
  return (
    <span class="fxart" classList={{ dim: !!props.dim }} style={{ width: props.size + 'px', height: props.size + 'px' }}
      innerHTML={FX_ART[props.type] ?? ''} />
  );
}
