// Helpers for round-tripping grid selections through per-document UI state
// (F08): the grid wants CompactSelection ranges, the store keeps plain
// index arrays.

/** Merge sorted-or-not indices into half-open [start, end) ranges. */
export function indicesToRanges(indices: number[]): [number, number][] {
  if (indices.length === 0) return [];
  const sorted = [...new Set(indices)].sort((a, b) => a - b);
  const ranges: [number, number][] = [];
  let start = sorted[0];
  let end = sorted[0] + 1;
  for (const i of sorted.slice(1)) {
    if (i === end) {
      end += 1;
    } else {
      ranges.push([start, end]);
      start = i;
      end = i + 1;
    }
  }
  ranges.push([start, end]);
  return ranges;
}
