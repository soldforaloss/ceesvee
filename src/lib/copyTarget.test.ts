import { describe, expect, it } from "vitest";

import { resolveCopyTarget } from "./copyTarget";

describe("copy target resolution (F14)", () => {
  it("visible scope covers every visible row with all columns", () => {
    const target = resolveCopyTarget("visible", { y: 2, height: 2, cols: [1, 2] }, [], [], 4);
    expect(target).toEqual({ rows: null, cols: [0, 1, 2, 3] });
  });

  it("selection scope prefers the active rectangle", () => {
    const target = resolveCopyTarget("selection", { y: 5, height: 3, cols: [1, 2] }, [9], [2], 4);
    expect(target).toEqual({ rows: [5, 6, 7], cols: [1, 2] });
  });

  it("keeps the caller's pre-translated physical column order (F12 layouts)", () => {
    // Display order "c2 before c0": the rect's columns arrive already
    // translated; the copy must preserve that on-screen order.
    const target = resolveCopyTarget("selection", { y: 0, height: 1, cols: [2, 0] }, [], [], 3);
    expect(target).toEqual({ rows: [0], cols: [2, 0] });
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
