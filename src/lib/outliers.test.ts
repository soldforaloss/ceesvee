import { describe, expect, it } from "vitest";

import { actionAvailable, isNumericMethod, parseAllowedValues } from "./outliers";

describe("outlier helpers", () => {
  it("classifies numeric methods", () => {
    expect(isNumericMethod("iqr")).toBe(true);
    expect(isNumericMethod("mad")).toBe(true);
    expect(isNumericMethod("zScore")).toBe(true);
    expect(isNumericMethod("percentile")).toBe(true);
    expect(isNumericMethod("rareCategory")).toBe(false);
    expect(isNumericMethod("unexpectedCategory")).toBe(false);
    expect(isNumericMethod("patternMismatch")).toBe(false);
  });

  it("median and cap corrections require numeric methods", () => {
    expect(actionAvailable("iqr", "replaceMedian")).toBe(true);
    expect(actionAvailable("rareCategory", "replaceMedian")).toBe(false);
    expect(actionAvailable("rareCategory", "capToBounds")).toBe(false);
    expect(actionAvailable("rareCategory", "replaceBlank")).toBe(true);
    expect(actionAvailable("patternMismatch", "removeRows")).toBe(true);
  });

  it("parses allowed values across commas and newlines", () => {
    expect(parseAllowedValues("a, b\nc\r\n , d,")).toEqual(["a", "b", "c", "d"]);
    expect(parseAllowedValues("  ")).toEqual([]);
  });
});
