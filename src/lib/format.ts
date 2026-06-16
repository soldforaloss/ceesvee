// Pure helpers for the status-bar selection summary. Kept free of React/Tauri
// so they're trivially unit-testable.

/** Parse a cell's text as a number, or `null` if it isn't numeric. */
export function parseNumber(value: string): number | null {
  const trimmed = value.trim();
  if (trimmed === "") return null;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : null;
}

export interface SelectionStats {
  count: number;
  numericCount: number;
  sum: number;
  avg: number | null;
  min: number | null;
  max: number | null;
}

/** Aggregate stats over a flat list of selected cell strings. */
export function selectionStats(values: string[]): SelectionStats {
  let numericCount = 0;
  let sum = 0;
  let min = Infinity;
  let max = -Infinity;

  for (const value of values) {
    const n = parseNumber(value);
    if (n !== null) {
      numericCount += 1;
      sum += n;
      if (n < min) min = n;
      if (n > max) max = n;
    }
  }

  return {
    count: values.length,
    numericCount,
    sum,
    avg: numericCount > 0 ? sum / numericCount : null,
    min: numericCount > 0 ? min : null,
    max: numericCount > 0 ? max : null,
  };
}

/** Format a number compactly for the status bar. */
export function formatNumber(n: number): string {
  if (Number.isInteger(n)) return n.toLocaleString();
  return n.toLocaleString(undefined, { maximumFractionDigits: 6 });
}
