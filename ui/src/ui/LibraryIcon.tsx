import { type JSX, Show } from 'solid-js';

import type { LibrarySemantic, LibrarySemanticPrimary } from '../lib/librarySemantics';

type IconFamily = 'structure' | 'drum' | 'instrument' | 'voice' | 'fx' | 'musical' | 'generic';

export function LibraryIcon(props: {
  semantic: LibrarySemantic;
  kind: 'dir' | 'file';
  open?: boolean | undefined;
}): JSX.Element {
  return (
    <span
      class={`wv-library-icon wv-library-icon--${iconFamily(props.semantic.primary)}`}
      data-kind={props.kind}
      title={props.semantic.label}
      aria-hidden="true"
    >
      <svg viewBox="0 0 18 16">
        <Show when={props.kind === 'dir'}>
          <path
            class="wv-library-icon__shell"
            d={props.open ? 'M1.5 5h15l-2 9H3z M2.5 5V2.5h5l1.7 2' : 'M2 3h5l1.8 2H16v9H2z'}
          />
        </Show>
        <Show when={props.kind === 'file'}>
          <g class="wv-library-icon__glyph">
            <SemanticGlyph primary={props.semantic.primary} />
          </g>
        </Show>
      </svg>
    </span>
  );
}

function SemanticGlyph(props: { primary: LibrarySemanticPrimary }): JSX.Element {
  switch (props.primary) {
    case 'loop':
      return <path d="M6 8a3 3 0 0 1 5-2l1-1v3H9l1-1a1.8 1.8 0 0 0-3 .8 M12 10a3 3 0 0 1-5 2l-1 1v-3h3l-1 1a1.8 1.8 0 0 0 3-.8" />;
    case 'one-shot':
      return <path d="M6 11V8l1.3 2 1.2-5 1.2 6 1.2-3L12 10v1z" />;
    case 'stem':
      return <path d="M6 7h1v5H6zM8.5 5h1v7h-1zM11 8h1v4h-1z" />;
    case 'songstarter':
      return <path d="M6 6h2v2H6zm3 0h3v2H9zm-3 3h3v2H6zm4 0h2v2h-2z" />;
    case 'kick':
      return <path d="M9 5.5a3 3 0 1 0 0 6 3 3 0 0 0 0-6zm0 1.4a1.6 1.6 0 1 1 0 3.2 1.6 1.6 0 0 1 0-3.2" />;
    case '808':
      return <path d="M5.5 9c1-4 2 4 3 0s2 4 4 0M6 6.5h6M6 11.5h6" />;
    case 'snare':
      return <path d="M6 7h6v4H6zM6 8l6 2m-6 0 6-2" />;
    case 'clap':
      return <path d="M7 11L6 8m3 3V6m2 5 1-3M5.5 6.5 7 8m5.5-1.5L11 8" />;
    case 'rim':
      return <path d="M6 10.5h6M7 6l4 5M11.5 6.5l-5 4" />;
    case 'hi-hat':
      return <path d="M6 8h6L9 5zm3 0v4m-2 0h4" />;
    case 'cymbal':
      return <path d="M5.5 8.5c2-2 5-2 7 0zm3.5 0V12m-2 0h4M7 6.5h4" />;
    case 'tom':
      return <path d="M6 6.5h6l-.7 4H6.7zM7 11l-.5 1m4.5-1 .5 1" />;
    case 'percussion':
      return <path d="M5.5 7h3l-.5 4H6zm4 0h3l-.5 4h-2zM6 6h2m2 0h2" />;
    case 'shaker':
      return <path d="M6 11l5-5 1 1-5 5zM7.5 9.5l1 1m.5-2.5 1 1" />;
    case 'drum':
      return <path d="M6 8h6v3H6zM7 11v1m4-1v1M6.5 5.5l5 3m0-3-5 3" />;
    case 'fill':
      return <path d="M5.5 10h1l1-3 1.2 5 1.1-6 1 4h1.7" />;
    case 'bass':
      return <path d="M6 11c2-4 3-1 6-5M7 7h4m-5 2h4" />;
    case 'keys':
      return <path d="M5.5 6h7v6h-7zM7 6v3m2-3v3m2-3v3M8 9v3m2-3v3" />;
    case 'organ':
      return <path d="M6 11V7h1.5v4M8.5 11V5h1.5v6m1 0V8h1v3M5.5 11.5h7" />;
    case 'guitar':
      return <path d="M7 11c-2-1 0-4 2-3l2-3 1 1-3 2c1 2-1 4-2 3zm1-2h.1" />;
    case 'strings':
      return <path d="M7 11c-2-1 0-4 2-3l1-3m-2 7 3-7m-1 5 2 2" />;
    case 'brass':
      return <path d="M6 10h4l2-2v3l-2-1H6zm1-2V6h3v2" />;
    case 'woodwind':
      return <path d="M6 11l6-5M7 9l2 2m0-4 2 2M7.5 9.5h.1m2-2h.1" />;
    case 'bell':
      return <path d="M6 10h6l-1-1V7a2 2 0 0 0-4 0v2zm2 1h2" />;
    case 'vocal':
      return <path d="M7 6v3a2 2 0 0 0 4 0V6M6 9a3 3 0 0 0 6 0m-3 3v-1" />;
    case 'choir':
      return <path d="M9 6a1 1 0 1 0 0-2 1 1 0 0 0 0 2zm-3 2a1 1 0 1 0 0-2 1 1 0 0 0 0 2zm6 0a1 1 0 1 0 0-2 1 1 0 0 0 0 2zM5 11c0-2 2-2 2-2m4 0s2 0 2 2M7 12c0-3 4-3 4 0" />;
    case 'spoken':
      return <path d="M5.5 5.5h7v5h-4l-2 1.5v-1.5h-1zM7 7.5h4M7 9h2.5" />;
    case 'impact':
      return <path d="M9 5l.6 2.2L12 6l-1.2 2.2L13 9l-2.3.6L12 12l-2.4-1.2L9 13l-.7-2.2L6 12l1.2-2.4L5 9l2.2-.8L6 6l2.3 1.2z" />;
    case 'riser':
      return <path d="M6 11l2-2 1 1 3-4m-2 0h2v2" />;
    case 'faller':
      return <path d="M6 6l2 2 1-1 3 4m-2 0h2V9" />;
    case 'transition':
    case 'fx':
      return <path d="M5.5 10c2-5 4 3 7-3M6 7c2 3 4-2 6 2" />;
    case 'ambience':
      return <path d="M5.5 9c1.2-2 2.2 2 3.5 0s2.3 2 3.5 0M6 11h6" />;
    case 'pad':
      return <path d="M5.5 7.5h7v4h-7zM7 6h4m-4.5 3.5h5" />;
    case 'chord':
      return <path d="M6 11V7m3 4V6m3 5V8M6 7l3-1 3 2" />;
    case 'arp':
      return <path d="M6 11l2-4 2 3 2-5M6 11h6" />;
    case 'melody':
      return <path d="M8 6v5a1.5 1.5 0 1 1-1-1.4V7l5-1v4a1.5 1.5 0 1 1-1-1.4V5z" />;
    case 'lead':
      return <path d="M8 5.5v5.5a1.5 1.5 0 1 1-1-1.4V6.5l5-1v2z" />;
    case 'pluck':
      return <path d="M9 5c-3 4-2 6 0 7 2-1 3-3 0-7zm0 2v4" />;
    case 'synth':
      return <path d="M5.5 8c1-4 2 4 3 0s2 4 4 0M6 11h6" />;
    case 'audio':
      return <path d="M5.5 9h1l.8-2 1.2 4L10 6l.8 3h1.7" />;
  }
}

function iconFamily(primary: LibrarySemanticPrimary): IconFamily {
  if (primary === 'loop' || primary === 'one-shot' || primary === 'stem' || primary === 'songstarter') return 'structure';
  if (['drum', 'kick', 'snare', 'clap', 'rim', 'hi-hat', 'cymbal', 'tom', 'percussion', 'shaker', '808', 'fill'].includes(primary)) return 'drum';
  if (['bass', 'synth', 'lead', 'pad', 'pluck', 'keys', 'organ', 'guitar', 'strings', 'brass', 'woodwind', 'bell'].includes(primary)) return 'instrument';
  if (primary === 'vocal' || primary === 'choir' || primary === 'spoken') return 'voice';
  if (['fx', 'impact', 'riser', 'faller', 'transition', 'ambience'].includes(primary)) return 'fx';
  if (primary === 'melody' || primary === 'chord' || primary === 'arp') return 'musical';
  return 'generic';
}
