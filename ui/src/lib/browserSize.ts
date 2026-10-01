export const DEFAULT_BROWSER_WIDTH = 230;
export const MIN_BROWSER_WIDTH = 184;
export const MAX_BROWSER_WIDTH = 420;

/** Space that must remain for the actual editor, even with both side panes open. */
export const MIN_WORKSPACE_WIDTH = 320;
const OPEN_SIDE_PANEL_WIDTH = 306;
const COLLAPSED_SIDE_RAIL_WIDTH = 30;

export function maxBrowserWidth(viewportWidth: number, sideOpen: boolean, playlistPicker = false): number {
  const sideWidth = sideOpen ? OPEN_SIDE_PANEL_WIDTH : COLLAPSED_SIDE_RAIL_WIDTH;
  const pickerWidth = playlistPicker ? (viewportWidth <= 1050 ? 144 : 176) : 0;
  return Math.max(
    MIN_BROWSER_WIDTH,
    Math.min(MAX_BROWSER_WIDTH, Math.floor(viewportWidth - sideWidth - pickerWidth - MIN_WORKSPACE_WIDTH)),
  );
}

export function clampBrowserWidth(
  width: number,
  viewportWidth: number,
  sideOpen: boolean,
  playlistPicker = false,
): number {
  return Math.round(
    Math.min(maxBrowserWidth(viewportWidth, sideOpen, playlistPicker), Math.max(MIN_BROWSER_WIDTH, width)),
  );
}
