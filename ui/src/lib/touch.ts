/**
 * Reads each value so a Solid effect records it as a dependency.
 * Call it as a statement inside an effect; the return value is not meaningful.
 */
export const touch = (...values: unknown[]): number => values.length;
