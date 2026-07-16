import { describe, expect, it } from "vitest";

import type { RepairPreview } from "../types";
import { parseNullTokens, repairApplyLabel, repairIsNoop } from "./repair";

const preview = (patch: Partial<RepairPreview>): RepairPreview => ({
  revision: 1,
  cellsAffected: 0,
  rowsRemoved: 0,
  columnsRemoved: 0,
  fillValues: [],
  invalidNumeric: 0,
  examples: [],
  ...patch,
});

describe("parseNullTokens", () => {
  it("trims, splits on commas, and drops blanks", () => {
    expect(parseNullTokens(" NA , N/A ,, null ,")).toEqual(["NA", "N/A", "null"]);
    expect(parseNullTokens("   ")).toEqual([]);
  });
});

describe("repairApplyLabel", () => {
  it("names removals explicitly and counts cells otherwise", () => {
    expect(repairApplyLabel(null)).toBe("Apply");
    expect(repairApplyLabel(preview({ cellsAffected: 3 }))).toBe("Apply to 3 cells");
    expect(repairApplyLabel(preview({ cellsAffected: 1 }))).toBe("Apply to 1 cell");
    expect(repairApplyLabel(preview({ rowsRemoved: 2 }))).toBe("Remove 2 rows");
    expect(repairApplyLabel(preview({ columnsRemoved: 1 }))).toBe("Remove 1 column");
  });
});

describe("repairIsNoop", () => {
  it("is true only for an all-zero preview", () => {
    expect(repairIsNoop(null)).toBe(false);
    expect(repairIsNoop(preview({}))).toBe(true);
    expect(repairIsNoop(preview({ cellsAffected: 1 }))).toBe(false);
    expect(repairIsNoop(preview({ rowsRemoved: 1 }))).toBe(false);
  });
});
