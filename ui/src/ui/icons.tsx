import type { JSX } from 'solid-js';

/**
 * Every icon the shell draws, with the path data copied verbatim from
 * `.design/wave-shell-v2.html`.
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
    <svg
      width="11"
      height="11"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      aria-hidden="true"
    >
      <rect x="2" y="2" width="8" height="8" rx="1.4" />
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
