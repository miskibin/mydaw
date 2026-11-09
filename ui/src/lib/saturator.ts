// Saturator helpers shared by the mock and project migration (crates/engine/src/dsp/saturator.rs).

/** dsp::saturator::drive_db: the Drive knob (0–100 %) is quadratic, 36 dB at the top; Punish adds 20 dB. */
export function satDriveDb(p: Record<string, number>): number {
  const pos = Math.min(1, Math.max(0, (p.drive ?? 40) / 100));
  return 36 * pos * pos + ((p.punish ?? 0) >= 0.5 ? 20 : 0);
}

/**
 * dsp::saturator::migrate_params: `sat` params saved before the character saturator (drive
 * 0–36 dB, mix, out) → same gain into the stage, tape voicing, auto gain off. Returns
 * whether anything changed; a current parameter set is untouched.
 */
export function migrateSatParams(p: Record<string, number>): boolean {
  if ('style' in p) return false;
  const old = Math.min(36, Math.max(0, p.drive ?? 6));
  p.drive = Math.sqrt(old / 36) * 100;
  p.style = 0;
  p.auto = 0;
  p.out = Math.min(12, Math.max(-24, (p.out ?? 0) - old * 0.5));
  return true;
}
