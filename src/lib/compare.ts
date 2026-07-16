// Pure helpers for the compare dialog (F09): automatic column mapping.

/**
 * Propose a (left, right) column mapping: same-name columns pair first
 * (case-insensitive, trimmed); remaining columns pair by position when both
 * sides still have unmatched columns at the same index. Left order preserved.
 */
export function autoMapColumns(leftHeaders: string[], rightHeaders: string[]): [number, number][] {
  const norm = (h: string) => h.trim().toLowerCase();
  const rightByName = new Map<string, number>();
  for (let r = rightHeaders.length - 1; r >= 0; r--) {
    const key = norm(rightHeaders[r]);
    if (key) rightByName.set(key, r); // earliest right column wins
  }

  const usedRight = new Set<number>();
  const mapping: [number, number][] = [];
  const unmatchedLeft: number[] = [];

  for (let l = 0; l < leftHeaders.length; l++) {
    const key = norm(leftHeaders[l]);
    const r = key ? rightByName.get(key) : undefined;
    if (r !== undefined && !usedRight.has(r)) {
      mapping.push([l, r]);
      usedRight.add(r);
    } else {
      unmatchedLeft.push(l);
    }
  }

  // Positional fallback for name-less pairs.
  for (const l of unmatchedLeft) {
    if (l < rightHeaders.length && !usedRight.has(l)) {
      mapping.push([l, l]);
      usedRight.add(l);
    }
  }

  mapping.sort((a, b) => a[0] - b[0]);
  return mapping;
}
