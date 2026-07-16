import { describe, expect, it } from "vitest";

import type { JoinPreview } from "../types";
import { joinNeedsConfirmation, joinRunLabel } from "./joins";

const preview = (projectedRows: number): JoinPreview => ({
  outputColumns: ["a"],
  matchedPairs: 0,
  leftRows: 0,
  rightRows: 0,
  leftUnmatched: 0,
  rightUnmatched: 0,
  leftDuplicateKeys: 0,
  rightDuplicateKeys: 0,
  projectedRows,
  expands: false,
  lookupConflict: false,
});

describe("join confirmation gate", () => {
  it("requires confirmation only past the threshold", () => {
    expect(joinNeedsConfirmation(null, 100)).toBe(false);
    expect(joinNeedsConfirmation(preview(100), 100)).toBe(false);
    expect(joinNeedsConfirmation(preview(101), 100)).toBe(true);
  });

  it("labels the run button per state", () => {
    expect(joinRunLabel(preview(5), false, true, 100)).toBe("Joining…");
    expect(joinRunLabel(preview(5), false, false, 100)).toBe("Join into a new document");
    expect(joinRunLabel(preview(500), false, false, 100)).toContain("confirm");
    expect(joinRunLabel(preview(500), true, false, 100)).toBe("Join into a new document");
  });
});
