import { describe, expect, it } from "vitest";

import { indicesToRanges } from "./gridSelection";

describe("indicesToRanges", () => {
  it("merges consecutive runs into half-open ranges", () => {
    expect(indicesToRanges([1, 2, 3, 7, 8, 12])).toEqual([
      [1, 4],
      [7, 9],
      [12, 13],
    ]);
  });

  it("handles unsorted input with duplicates", () => {
    expect(indicesToRanges([5, 3, 4, 5, 1])).toEqual([
      [1, 2],
      [3, 6],
    ]);
  });

  it("handles empty input", () => {
    expect(indicesToRanges([])).toEqual([]);
  });
});
