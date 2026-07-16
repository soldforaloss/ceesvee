import { describe, expect, it } from "vitest";

import type { PiiFinding } from "../types";
import { nonPiiColumns, redactionNeedsSecret } from "./pii";

const finding = (column: number): PiiFinding => ({
  detector: 0,
  detectorLabel: "email",
  validation: "pattern",
  column,
  count: 1,
  samples: [],
});

describe("nonPiiColumns", () => {
  it("returns only unflagged columns, or null when none are safe", () => {
    expect(nonPiiColumns([finding(1)], 3)).toEqual([0, 2]);
    expect(nonPiiColumns([], 2)).toEqual([0, 1]);
    expect(nonPiiColumns([finding(0), finding(1)], 2)).toBeNull();
  });
});

describe("redactionNeedsSecret", () => {
  it("only pseudonymize requires a non-blank secret", () => {
    expect(redactionNeedsSecret("pseudonymize", "")).toBe(true);
    expect(redactionNeedsSecret("pseudonymize", "  ")).toBe(true);
    expect(redactionNeedsSecret("pseudonymize", "k")).toBe(false);
    expect(redactionNeedsSecret("fullMask", "")).toBe(false);
  });
});
