import { describe, expect, it } from "vitest";

import { resolveCopyTarget } from "./copyTarget";

describe("copy target resolution (F14)", () => {
  it("visible scope covers every visible row with all columns", () => {
    const target = resolveCopyTarget("visible", { x: 1, y: 2, width: 2, height: 2 }, [], [], 4);
    expect(target).toEqual({ rows: null, cols: [0, 1, 2, 3] });
  });

  it("selection scope prefers the active rectangle", () => {
    const target = resolveCopyTarget("selection", { x: 1, y: 5, width: 2, height: 3 }, [9], [2], 4);
    expect(target).toEqual({ rows: [5, 6, 7], cols: [1, 2] });
  });

  it("falls back to row markers, then column markers", () => {
    expect(resolveCopyTarget("selection", null, [3, 7], [], 3)).toEqual({
      rows: [3, 7],
      cols: [0, 1, 2],
    });
    expect(resolveCopyTarget("selection", null, [], [1], 3)).toEqual({
      rows: null,
      cols: [1],
    });
  });

  it("returns null when nothing is selected", () => {
    expect(resolveCopyTarget("selection", null, [], [], 3)).toBeNull();
  });
});
