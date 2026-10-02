export interface WindowGeometry { x: number; y: number; w: number; h: number }

/** Keep the title, controls and resize handle reachable after a viewport resize. */
export function fitWindow(
  held: WindowGeometry,
  host: { w: number; h: number },
  minHeight = 320,
): WindowGeometry {
  const maxW = Math.max(1, host.w - 16);
  const maxH = Math.max(1, host.h - 16);
  const w = Math.min(maxW, Math.max(Math.min(480, maxW), held.w));
  const h = Math.min(maxH, Math.max(Math.min(minHeight, maxH), held.h));
  return {
    x: Math.max(8, Math.min(held.x, host.w - w - 8)),
    y: Math.max(8, Math.min(held.y, host.h - h - 8)),
    w, h,
  };
}
