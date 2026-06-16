import { describe, expect, it } from "vitest";
import { formatNumber } from "./format";

describe("formatNumber", () => {
  it("keeps integers integral with locale grouping", () => {
    expect(formatNumber(1000)).toBe((1000).toLocaleString());
    expect(formatNumber(-5)).toBe((-5).toLocaleString());
  });

  it("bounds fractional precision", () => {
    expect(formatNumber(1 / 3)).not.toContain("0.3333333333");
    expect(formatNumber(2.5)).toContain("2.5");
  });
});
