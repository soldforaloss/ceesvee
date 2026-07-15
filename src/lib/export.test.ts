import { describe, expect, it } from "vitest";

import { buildSplit, scopeChoices } from "./export";

describe("scopeChoices", () => {
  it("always offers all rows, adding others as they apply", () => {
    expect(scopeChoices(false, null, [], []).map((c) => c.scope.type)).toEqual(["all"]);

    const rect = { x: 0, y: 0, width: 2, height: 3 };
    const all = scopeChoices(true, rect, [1, 2], [0]);
    expect(all.map((c) => c.scope.type)).toEqual([
      "all",
      "visibleRows",
      "selectedRows",
      "selectedColumns",
      "selectedRange",
    ]);
  });

  it("skips single-cell ranges", () => {
    const rect = { x: 0, y: 0, width: 1, height: 1 };
    expect(scopeChoices(false, rect, [], []).map((c) => c.scope.type)).toEqual(["all"]);
  });

  it("carries the selection payloads through", () => {
    const choices = scopeChoices(false, null, [5, 7], [2, 0]);
    const rows = choices.find((c) => c.scope.type === "selectedRows")!.scope;
    expect(rows).toEqual({ type: "selectedRows", rows: [5, 7] });
    const cols = choices.find((c) => c.scope.type === "selectedColumns")!.scope;
    expect(cols).toEqual({ type: "selectedColumns", columns: [2, 0] });
  });
});

describe("buildSplit", () => {
  it("validates row counts", () => {
    expect(buildSplit("maxRows", 0, 0, -1)).toEqual({ error: "Rows per file must be at least 1" });
    expect(buildSplit("maxRows", 250.7, 0, -1)).toEqual({ type: "maxRows", rowsPerFile: 250 });
  });

  it("converts megabytes to bytes", () => {
    expect(buildSplit("approximateBytes", 0, 10, -1)).toEqual({
      type: "approximateBytes",
      maxBytes: 10 * 1024 * 1024,
    });
    expect(buildSplit("approximateBytes", 0, 0, -1)).toEqual({
      error: "File size must be positive",
    });
  });

  it("passes group column through", () => {
    expect(buildSplit("groupByColumn", 0, 0, 3)).toEqual({ type: "groupByColumn", column: 3 });
    expect(buildSplit("groupByColumn", 0, 0, -1)).toEqual({ error: "Pick a column to group by" });
    expect(buildSplit("none", 0, 0, -1)).toEqual({ type: "none" });
  });
});
