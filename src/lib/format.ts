// Number formatting for the status bar. Selection aggregates themselves are
// computed in Rust (see the `selection_stats` command) so they scale to any
// selection size; this just renders the resulting values.

/** Format a number compactly: integers get grouping, fractions are bounded. */
export function formatNumber(n: number): string {
  if (Number.isInteger(n)) return n.toLocaleString();
  return n.toLocaleString(undefined, { maximumFractionDigits: 6 });
}
