/** Monotonic revision helpers for discarding stale async UI responses. */

export function bumpRevision(current: number): number {
  return current + 1;
}

export function isCurrentRevision(expected: number, current: number): boolean {
  return expected === current;
}
