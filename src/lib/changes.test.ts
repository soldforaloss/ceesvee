import { describe, expect, it } from "vitest";

import type { ChangeSummary } from "../types";
import { changeKindLabel, changeReportJson, changeTime } from "./changes";

const change = (id: number): ChangeSummary => ({
  id,
  epochSecs: 1_700_000_000,
  kind: "cellEdits",
  cellsAffected: 1,
  sample: [{ row: 0, col: 0, old: "a", new: "b" }],
  structural: false,
  revertible: true,
  blockedReason: null,
});

describe("change helpers", () => {
  it("labels known kinds and passes unknown ones through", () => {
    expect(changeKindLabel("cellEdits")).toBe("Cell edits");
    expect(changeKindLabel("deleteRows")).toBe("Delete rows");
    expect(changeKindLabel("somethingNew")).toBe("somethingNew");
  });

  it("renders a time only for real timestamps", () => {
    expect(changeTime(0)).toBe("");
    expect(changeTime(1_700_000_000)).not.toBe("");
  });

  it("exports exactly the reported operations", () => {
    const json = changeReportJson("orders.csv", [change(1), change(2)]);
    const parsed = JSON.parse(json) as { document: string; changes: ChangeSummary[] };
    expect(parsed.document).toBe("orders.csv");
    expect(parsed.changes).toHaveLength(2);
    expect(parsed.changes[0].sample[0].old).toBe("a");
  });
});
