import { onCleanup } from 'solid-js';

/** Activate the containing editor before a painting/knob gesture stops bubbling. */
export function bindPointerActivation(element: HTMLElement, activate: () => void): void {
  element.addEventListener('pointerdown', activate, true);
  onCleanup(() => element.removeEventListener('pointerdown', activate, true));
}
