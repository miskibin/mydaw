import type { JSX } from 'solid-js';

export function IconView(props: { kind: 'playlist' | 'rack' | 'mixer' }): JSX.Element {
  const paths = {
    playlist: 'M2 3h3v2H2zM7 3h6v2H7zM2 7h3v2H2zM7 7h4v2H7zM2 11h3v2H2zM7 11h6v2H7z',
    rack: 'M2 3h3v2H2zM7 3h2v2H7zM11 3h2v2h-2zM2 7h3v2H2zM7 7h2v2H7zM11 7h2v2h-2zM2 11h3v2H2zM7 11h2v2H7zM11 11h2v2h-2z',
    mixer: 'M3 2v3m0 3v6M7.5 2v7m0 3v2M12 2v3m0 3v6M1.5 5h3v3h-3zM6 9h3v3H6zM10.5 5h3v3h-3z',
  };
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 15 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={paths[props.kind]} />
    </svg>
  );
}

export function IconWindow(props: {
  kind: 'minimize' | 'maximize' | 'restore' | 'close';
}): JSX.Element {
  const paths = {
    minimize: 'M2 7h10',
    maximize: 'M2.5 2.5h9v9h-9z',
    restore: 'M5 4V1.5h7.5V9H10M1.5 4.5h8v8h-8z',
    close: 'M2.5 2.5l9 9m0-9-9 9',
  };
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1"
      aria-hidden="true"
    >
      <path d={paths[props.kind]} />
    </svg>
  );
}

/**
 * The shell's SVG icons. The original set follows `.design/wave-shell-v2.html`;
 * everyday action glyphs use the same stroke weight and current-colour contract.
 *
 * Inline SVG rather than a font or a sprite sheet: there are fourteen of them,
 * they are all one or two paths, and `stroke="currentColor"` is what lets a
 * single icon be dim in a rail and accented in a card without a second copy.
 *
 * The viewBox of each is the mock's own, so an icon dropped into a 22 px chip
 * lands on the same pixels it does in the design.
 */

/** The WAVE mark in the top-left. Strokes with the accent. */
export function WaveMark(): JSX.Element {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 18 18"
      fill="none"
      stroke="var(--acc)"
      stroke-width="1.6"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <path d="M1 9h2.2l2-5.4 2.6 11L11 6.2l1.6 4.4L14.2 9H17" />
    </svg>
  );
}

export function IconRewind(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M11 2.5v9L4.5 7z" />
      <path d="M3 2.5v9" />
    </svg>
  );
}

export function IconPause(): JSX.Element {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <path d="M5 2.5v9M9 2.5v9" />
    </svg>
  );
}

export function IconPlay(): JSX.Element {
  return (
    <svg width="12" height="12" viewBox="0 0 14 14" fill="currentColor" aria-hidden="true">
      <path d="M4 2.4 11.6 7 4 11.6z" />
    </svg>
  );
}

export function IconStop(): JSX.Element {
  return (
    <svg width="11" height="11" viewBox="0 0 12 12" fill="currentColor" aria-hidden="true">
      <rect x="2" y="2" width="8" height="8" rx="1.4" />
    </svg>
  );
}

/** Small, consistent glyphs for the studio's everyday actions. */
export function IconUndo(props: { redo?: boolean }): JSX.Element {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      style={{ transform: props.redo ? 'scaleX(-1)' : undefined }}
    >
      <path d="M6 3 3 6l3 3M3 6h6a3 3 0 0 1 0 6H6" />
    </svg>
  );
}

export function IconMore(): JSX.Element {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
      <circle cx="8" cy="3" r="1" />
      <circle cx="8" cy="8" r="1" />
      <circle cx="8" cy="13" r="1" />
    </svg>
  );
}

export function IconRefresh(): JSX.Element {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M12.2 5.2A5 5 0 1 0 13 8M12.2 2v3.5H8.7" />
    </svg>
  );
}

export function IconWaveform(): JSX.Element {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M1 8h2l1.5-4 2.5 9 2-10 2.5 8L13 8h2" />
    </svg>
  );
}

export function IconVolume(props: { muted?: boolean }): JSX.Element {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M2 6h3l3-3v10l-3-3H2z" />
      {props.muted ? (
        <path d="m11 6 3 4m0-4-3 4" />
      ) : (
        <>
          <path d="M10.5 5.5a4 4 0 0 1 0 5" />
          <path d="M12.5 3.5a6 6 0 0 1 0 9" />
        </>
      )}
    </svg>
  );
}

export function IconNoteOperation(props: {
  kind: 'quantize' | 'legato' | 'lengthen' | 'shorten' | 'chord' | 'humanize';
}): JSX.Element {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {props.kind === 'quantize' && (
        <>
          <path d="M3 3v10M8 3v10M13 3v10" />
          <path d="M1 6h4M6 10h4M11 6h4" />
        </>
      )}
      {props.kind === 'legato' && (
        <>
          <path d="m6 10 4-4M6.5 5l1.8-1.8a2.5 2.5 0 0 1 3.5 3.5L10 8.5M9.5 11l-1.8 1.8a2.5 2.5 0 0 1-3.5-3.5L6 7.5" />
        </>
      )}
      {props.kind === 'lengthen' && <path d="M2 8h12M5 5 2 8l3 3m6-6 3 3-3 3" />}
      {props.kind === 'shorten' && <path d="M1 8h5m4 0h5M3 5l3 3-3 3m10-6-3 3 3 3" />}
      {props.kind === 'chord' && (
        <>
          <path d="M5 3h8M5 8h8M5 13h8" />
          <path d="M2 3v.1M2 8v.1M2 13v.1" />
        </>
      )}
      {props.kind === 'humanize' && (
        <>
          <path d="M2 5h2l2 3 3-5 3 4h2M2 12h12" />
          <path d="M5 10v4m6-4v4" />
        </>
      )}
    </svg>
  );
}

export function IconRecord(): JSX.Element {
  return (
    <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" aria-hidden="true">
      <circle cx="6" cy="6" r="3.6" />
    </svg>
  );
}

export function IconLoop(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M3 5.5h6.5a2 2 0 0 1 0 4H4.5" />
      <path d="M5.6 3.4 3.4 5.5l2.2 2.1" />
    </svg>
  );
}

export function IconSettings(): JSX.Element {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.35"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M6.6 1.8h2.8l.4 1.7c.4.15.8.38 1.14.66l1.65-.62 1.4 2.42-1.25 1.2c.05.4.05.82 0 1.22l1.25 1.2-1.4 2.42-1.65-.62c-.34.28-.74.5-1.14.66l-.4 1.7H6.6l-.4-1.7a5 5 0 0 1-1.14-.66l-1.65.62-1.4-2.42 1.25-1.2a5 5 0 0 1 0-1.22l-1.25-1.2 1.4-2.42 1.65.62c.34-.28.74-.5 1.14-.66z" />
      <circle cx="8" cy="7.8" r="2.05" />
    </svg>
  );
}

export function IconSun(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <circle cx="7" cy="7" r="2.6" />
      <path d="M7 1.4v1.4M7 11.2v1.4M1.4 7h1.4M11.2 7h1.4M3.1 3.1l1 1M9.9 9.9l1 1M10.9 3.1l-1 1M4.1 9.9l-1 1" />
    </svg>
  );
}

export function IconMoon(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <path d="M11.4 8.6A5 5 0 0 1 5.4 2.6a5 5 0 1 0 6 6z" />
    </svg>
  );
}

/** The chevron used by every collapse control. `direction` picks which way. */
export function IconChevron(props: { direction: 'left' | 'right' }): JSX.Element {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {props.direction === 'left' ? (
        <path d="M7.2 2.4 3.6 6l3.6 3.6" />
      ) : (
        <path d="M4.8 2.4 8.4 6l-3.6 3.6" />
      )}
    </svg>
  );
}

export function IconSearch(): JSX.Element {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <circle cx="5.2" cy="5.2" r="3.2" />
      <path d="M7.6 7.6 10 10" />
    </svg>
  );
}

export function IconSelectTool(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M3 2.2l7.4 4.6-3.2.7-1.1 3.3z" />
    </svg>
  );
}

export function IconDrawTool(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M2.6 11.4h2l6-6-2-2-6 6z" />
    </svg>
  );
}

export function IconCutTool(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <circle cx="4" cy="10.4" r="1.7" />
      <circle cx="10" cy="10.4" r="1.7" />
      <path d="M5.2 9 10 2.4M8.8 9 4 2.4" />
    </svg>
  );
}

/** The "open this plugin's own window" arrows. */
export function IconPopOut(): JSX.Element {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M7.2 1.8h3v3M4.8 10.2h-3v-3M10.2 1.8 6.6 5.4M1.8 10.2l3.6-3.6" />
    </svg>
  );
}

/** The F7 maximise glyph on the bottom panel's tab bar. */
export function IconMaximise(): JSX.Element {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M4.4 1.8H1.8v2.6M7.6 10.2h2.6V7.6" />
    </svg>
  );
}

/** Frame the notes currently in the piano roll. */
export function IconFitNotes(): JSX.Element {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M3.4 1.8H1.8v1.6M8.6 1.8h1.6V3.4M1.8 8.6v1.6h1.6M10.2 8.6v1.6H8.6" />
      <path d="M5.1 8.4V3.8l2.6 1.2" />
    </svg>
  );
}

export function IconClose(): JSX.Element {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <path d="M3 3l6 6M9 3l-6 6" />
    </svg>
  );
}

/** The padlock a disabled control carries in its tooltip position. */
export function IconLock(): JSX.Element {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <rect x="2.6" y="5.2" width="6.8" height="5.2" rx="1.2" />
      <path d="M4.2 5.2V3.8a1.8 1.8 0 0 1 3.6 0v1.4" />
    </svg>
  );
}

// ---------------------------------------------------------------------------
// WAVE Studio additions
// ---------------------------------------------------------------------------
//
// Paths copied verbatim from `.design/wave-studio.html`, same as the ones
// above: the roll and playlist toolbars, the dropdown caret and the piano-roll
// button on a rack row.

/** The caret on a dropdown: the pattern selector, a channel's instrument menu. */
export function IconCaret(): JSX.Element {
  return (
    <svg
      width="9"
      height="9"
      viewBox="0 0 10 10"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M2.2 3.6 5 6.4l2.8-2.8" />
    </svg>
  );
}

/** A miniature keyboard: the button that opens a channel's piano roll. */
export function IconRoll(): JSX.Element {
  return (
    <svg width="14" height="12" viewBox="0 0 16 12" fill="none" aria-hidden="true">
      <rect
        x=".6"
        y=".6"
        width="14.8"
        height="10.8"
        rx="1.4"
        stroke="currentColor"
        stroke-width="1.1"
      />
      <path d="M4 .6v6.4M7 .6v6.4M10 .6v6.4M13 .6v6.4" stroke="currentColor" stroke-width="1" />
      <path
        d="M2.6 .6h1.8v5.2H2.6zM5.7 .6h1.8v5.2H5.7zM8.8 .6h1.8v5.2H8.8zM11.9 .6h1.8v5.2h-1.8z"
        fill="currentColor"
      />
    </svg>
  );
}

/** FL's typing-keyboard-to-piano toggle, next to the transport. */
export function IconTypingKeyboard(): JSX.Element {
  return (
    <svg width="16" height="12" viewBox="0 0 18 12" fill="none" aria-hidden="true">
      <rect
        x=".6"
        y=".6"
        width="16.8"
        height="10.8"
        rx="1.4"
        stroke="currentColor"
        stroke-width="1.1"
      />
      <path
        d="M4.2 .6v6.2M7.4 .6v6.2M10.6 .6v6.2M13.8 .6v6.2"
        stroke="currentColor"
        stroke-width="1"
      />
      <path
        d="M2.8 .6h1.7v5H2.8zM6 .6h1.7v5H6zM9.2 .6h1.7v5H9.2zM12.4 .6h1.7v5h-1.7z"
        fill="currentColor"
      />
    </svg>
  );
}

/** The paint tool: a brush, for painting clips and repeated notes. */
export function IconPaintTool(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M3 6.4h8V3.4H3zM6.2 6.4v2.2M6.2 11.6v-1.4" />
    </svg>
  );
}

/** The delete tool: an eraser. */
export function IconEraseTool(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M2.4 9.6 6 6l4 4-2 2H4.4zM7.4 4.6 9.6 2.4l2 2-2.2 2.2" />
    </svg>
  );
}

/** The slice tool: scissors. */
export function IconSliceTool(): JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <circle cx="4" cy="10.4" r="1.7" />
      <circle cx="10" cy="10.4" r="1.7" />
      <path d="M5.2 9 10 2.4M8.8 9 4 2.4" />
    </svg>
  );
}

/** A back arrow: leaving the fullscreen piano roll. */
export function IconBack(): JSX.Element {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M7.2 2.4 3.6 6l3.6 3.6" />
    </svg>
  );
}

/** A small × for a delete affordance inside a row. */
export function IconX(): JSX.Element {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <path d="M3 3l6 6M9 3l-6 6" />
    </svg>
  );
}

/** A warning triangle: a plugin the project asked for and did not get. */
export function IconWarn(): JSX.Element {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M6 1.6 11 10.4H1z" />
      <path d="M6 4.8v2.4M6 8.8v.1" />
    </svg>
  );
}
