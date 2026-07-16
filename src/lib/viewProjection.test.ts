import { describe, expect, it } from "vitest";

import type { FilterGroup, ViewSortKey } from "../types";
import {
  contiguousRuns,
  emptyLayout,
  layoutIsTrivial,
  physicalToDisplay,
  projectColumns,
  remapFilterColumns,
  resolveSortKeys,
  widthsFromIds,
  widthsToIds,
  type ColumnLayout,
} from "./viewProjection";

const IDS = ["c0", "c1", "c2", "c3", "c4"];

function layout(partial: Partial<ColumnLayout>): ColumnLayout {
  return { ...emptyLayout(), ...partial };
}

describe("projectColumns", () => {
  it("is the identity for a trivial layout", () => {
    const p = projectColumns(IDS, null);
    expect(p.physical).toEqual([0, 1, 2, 3, 4]);
    expect(p.frozen).toBe(0);
    expect(p.identity).toBe(true);
    expect(layoutIsTrivial(emptyLayout())).toBe(true);
  });

  it("hides columns without touching the others' order", () => {
    const p = projectColumns(IDS, layout({ hiddenColumnIds: ["c1", "c3"] }));
    expect(p.physical).toEqual([0, 2, 4]);
    expect(p.identity).toBe(false);
  });

  it("pins arbitrary columns first, in pin order, and freezes them", () => {
    const p = projectColumns(IDS, layout({ pinnedColumnIds: ["c3", "c0"] }));
    expect(p.physical).toEqual([3, 0, 1, 2, 4]);
    expect(p.frozen).toBe(2);
  });

  it("orders unpinned columns by columnOrder, appending unlisted ones in file order", () => {
    const p = projectColumns(IDS, layout({ columnOrder: ["c2", "c0"] }));
    expect(p.physical).toEqual([2, 0, 1, 3, 4]);
  });

  it("composes pins, order and hidden; hidden wins over pin and order", () => {
    const p = projectColumns(
      IDS,
      layout({
        hiddenColumnIds: ["c2"],
        pinnedColumnIds: ["c4", "c2"],
        columnOrder: ["c3", "c2", "c1"],
      }),
    );
    expect(p.physical).toEqual([4, 3, 1, 0]);
    expect(p.frozen).toBe(1);
  });

  it("reports missing IDs recoverably instead of failing", () => {
    const p = projectColumns(
      IDS,
      layout({ hiddenColumnIds: ["gone"], pinnedColumnIds: ["c1", "gone2"] }),
    );
    expect(p.physical).toEqual([1, 0, 2, 3, 4]);
    expect(p.missing).toEqual(["gone", "gone2"]);
  });

  it("maps physical back to display, null for hidden columns", () => {
    const p = projectColumns(IDS, layout({ hiddenColumnIds: ["c1"], columnOrder: ["c4"] }));
    expect(p.physical).toEqual([4, 0, 2, 3]);
    expect(physicalToDisplay(p, 4)).toBe(0);
    expect(physicalToDisplay(p, 1)).toBeNull();
    expect(physicalToDisplay(p, 3)).toBe(3);
  });
});

describe("contiguousRuns", () => {
  it("splits sorted physical columns into rectangle-friendly runs", () => {
    expect(contiguousRuns([0, 1, 2])).toEqual([{ start: 0, len: 3 }]);
    expect(contiguousRuns([0, 2, 3, 7])).toEqual([
      { start: 0, len: 1 },
      { start: 2, len: 2 },
      { start: 7, len: 1 },
    ]);
    expect(contiguousRuns([])).toEqual([]);
  });
});

describe("remapFilterColumns", () => {
  const filter: FilterGroup = {
    type: "group",
    id: "g",
    conjunction: "and",
    nodes: [
      { type: "condition", id: "a", column: 0, op: "notEmpty", value: "", caseSensitive: false },
      {
        type: "group",
        id: "g2",
        conjunction: "or",
        nodes: [
          { type: "condition", id: "b", column: 2, op: "equals", value: "x", caseSensitive: false },
        ],
      },
    ],
  };

  it("remaps nested condition columns through the ID snapshot", () => {
    // Saved against [c0,c1,c2]; the document was later reordered to [c2,c0,c1].
    const { filter: mapped, missing } = remapFilterColumns(
      filter,
      ["c0", "c1", "c2"],
      ["c2", "c0", "c1"],
    );
    expect(missing).toEqual([]);
    expect(mapped).not.toBeNull();
    const group = mapped as FilterGroup;
    expect((group.nodes[0] as { column: number }).column).toBe(1); // c0 moved to index 1
    const nested = group.nodes[1] as FilterGroup;
    expect((nested.nodes[0] as { column: number }).column).toBe(0); // c2 moved to index 0
  });

  it("is all-or-nothing when a referenced column was deleted", () => {
    const { filter: mapped, missing } = remapFilterColumns(
      filter,
      ["c0", "c1", "c2"],
      ["c0", "c1"], // c2 deleted
    );
    expect(mapped).toBeNull();
    expect(missing).toEqual(["c2"]);
  });
});

describe("resolveSortKeys", () => {
  it("resolves IDs to physical columns and skips missing keys", () => {
    const keys: ViewSortKey[] = [
      { columnId: "c2", descending: true },
      { columnId: "gone" },
      { columnId: "c0" },
    ];
    const { keys: resolved, missing } = resolveSortKeys(keys, ["c0", "c1", "c2"]);
    expect(resolved).toEqual([
      { column: 2, descending: true },
      { column: 0, descending: false },
    ]);
    expect(missing).toEqual(["gone"]);
  });
});

describe("width mapping", () => {
  it("round-trips widths through IDs across a reorder", () => {
    const byId = widthsToIds({ 0: 100, 2: 240 }, ["c0", "c1", "c2"]);
    expect(byId).toEqual({ c0: 100, c2: 240 });
    // Column c2 moved to the front.
    expect(widthsFromIds(byId, ["c2", "c0", "c1"])).toEqual({ 0: 240, 1: 100 });
  });
});
