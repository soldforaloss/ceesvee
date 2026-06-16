import { describe, expect, it } from "vitest";
import { formatNumber, parseNumber, selectionStats } from "./format";

describe("parseNumber", () => {
  it("parses plain numbers", () => {
    expect(parseNumber("42")).toBe(42);
    expect(parseNumber("-3.5")).toBe(-3.5);
    expect(parseNumber("  10  ")).toBe(10);
    expect(parseNumber("1e3")).toBe(1000);
  });

  it("rejects non-numbers", () => {
    expect(parseNumber("")).toBeNull();
    expect(parseNumber("abc")).toBeNull();
    expect(parseNumber("1,000")).toBeNull();
  });
});

describe("selectionStats", () => {
  it("aggregates numeric cells and ignores text", () => {
    const stats = selectionStats(["1", "2", "3", "x", ""]);
    expect(stats.count).toBe(5);
    expect(stats.numericCount).toBe(3);
    expect(stats.sum).toBe(6);
    expect(stats.avg).toBe(2);
    expect(stats.min).toBe(1);
    expect(stats.max).toBe(3);
  });

  it("returns null aggregates when nothing is numeric", () => {
    const stats = selectionStats(["a", "b"]);
    expect(stats.numericCount).toBe(0);
    expect(stats.avg).toBeNull();
    expect(stats.min).toBeNull();
    expect(stats.max).toBeNull();
  });
});

describe("formatNumber", () => {
  it("keeps integers integral", () => {
    expect(formatNumber(1000)).toBe((1000).toLocaleString());
  });

  it("bounds fractional precision", () => {
    expect(formatNumber(1 / 3)).not.toContain("0.3333333333");
  });
});
