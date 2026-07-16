import { describe, expect, it } from "vitest";

import type { DocumentMeta, FilterGroup, NamedView } from "../types";
import {
  describeView,
  hydrateFilter,
  newViewId,
  snapshotView,
  sortKeysToIds,
  uniqueViewName,
  upsertView,
} from "./views";

function meta(): DocumentMeta {
  return {
    id: 1,
    path: "C:\\data\\a.csv",
    fileName: "a.csv",
    rowCount: 3,
    totalRowCount: 3,
    filtered: false,
    colCount: 3,
    headers: ["a", "b", "c"],
    columnIds: ["c0", "c1", "c2"],
    viewSorted: false,
    hasHeaderRow: true,
    delimiter: ",",
    encoding: "UTF-8",
    hadBom: false,
    lineEnding: "lf",
    dirty: false,
    canUndo: false,
    canRedo: false,
    revision: 1,
    backing: "editable",
    archive: null,
  };
}

function view(partial: Partial<NamedView>): NamedView {
  return {
    id: "v1",
    name: "test",
    filter: null,
    filterColumnIds: [],
    sortKeys: [],
    hiddenColumnIds: [],
    pinnedColumnIds: [],
    columnOrder: [],
    columnWidths: {},
    wrapText: false,
    ...partial,
  };
}

describe("snapshotView", () => {
  it("captures the current state keyed by stable column IDs", () => {
    const v = snapshotView({
      name: "QA",
      meta: meta(),
      filter: null,
      viewSortKeys: [{ column: 2, descending: true }],
      layout: { hiddenColumnIds: ["c1"], pinnedColumnIds: ["c2"], columnOrder: [] },
      columnWidths: { 0: 120, 2: 300 },
      wrapText: true,
    });
    expect(v.sortKeys).toEqual([{ columnId: "c2", descending: true }]);
    expect(v.hiddenColumnIds).toEqual(["c1"]);
    expect(v.pinnedColumnIds).toEqual(["c2"]);
    expect(v.columnWidths).toEqual({ c0: 120, c2: 300 });
    expect(v.wrapText).toBe(true);
    expect(v.filterColumnIds).toEqual([]);
    expect(v.id).toBeTruthy();
  });

  it("snapshots the column-ID alignment only when a filter is saved", () => {
    const filter: FilterGroup = { type: "group", id: "g", conjunction: "and", nodes: [] };
    const v = snapshotView({
      name: "F",
      meta: meta(),
      filter,
      viewSortKeys: [],
      layout: null,
      columnWidths: {},
      wrapText: false,
    });
    expect(v.filterColumnIds).toEqual(["c0", "c1", "c2"]);
  });
});

describe("view CRUD helpers", () => {
  it("upserts by id", () => {
    const a = view({ id: "a", name: "A" });
    const views = upsertView([a], view({ id: "b", name: "B" }));
    expect(views.map((v) => v.name)).toEqual(["A", "B"]);
    const replaced = upsertView(views, view({ id: "a", name: "A2" }));
    expect(replaced.map((v) => v.name)).toEqual(["A2", "B"]);
  });

  it("generates unique names case-insensitively", () => {
    expect(uniqueViewName(["Report", "report (2)"], "report")).toBe("report (3)");
    expect(uniqueViewName([], "  ")).toBe("Untitled view");
  });

  it("mints distinct ids", () => {
    expect(newViewId()).not.toBe(newViewId());
  });
});

describe("hydrateFilter", () => {
  it("gives ids to nodes loaded from settings (which store none)", () => {
    const raw = {
      type: "group",
      conjunction: "and",
      nodes: [
        { type: "condition", column: 0, op: "notEmpty", value: "", caseSensitive: false },
        { type: "group", conjunction: "or", nodes: [] },
      ],
    } as unknown as FilterGroup;
    const hydrated = hydrateFilter(raw);
    expect(hydrated.id).toBeTruthy();
    expect(hydrated.nodes[0].id).toBeTruthy();
    expect(hydrated.nodes[1].id).toBeTruthy();
    expect(hydrated.nodes[0].id).not.toBe(hydrated.nodes[1].id);
  });
});

describe("describeView / sortKeysToIds", () => {
  it("summarises what a view changes", () => {
    expect(describeView(view({}))).toBe("layout only");
    expect(
      describeView(
        view({
          filter: { type: "group", id: "g", conjunction: "and", nodes: [] },
          sortKeys: [{ columnId: "c0" }, { columnId: "c1", descending: true }],
          hiddenColumnIds: ["c2"],
          wrapText: true,
        }),
      ),
    ).toBe("filter · sort ×2 · 1 hidden · wrap");
  });

  it("drops sort keys whose physical column has no id", () => {
    expect(sortKeysToIds([{ column: 9, descending: false }], ["c0"])).toEqual([]);
  });
});
