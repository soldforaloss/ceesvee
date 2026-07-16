import { describe, expect, it } from "vitest";

import { reshapeProblem } from "./reshape";

describe("reshapeProblem", () => {
  it("mirrors the backend unpivot checks", () => {
    expect(
      reshapeProblem({
        type: "unpivot",
        idColumns: [0],
        valueColumns: [],
        attributeName: "a",
        valueName: "v",
      }),
    ).not.toBeNull();
    expect(
      reshapeProblem({
        type: "unpivot",
        idColumns: [0],
        valueColumns: [0, 1],
        attributeName: "a",
        valueName: "v",
      }),
    ).not.toBeNull();
    expect(
      reshapeProblem({
        type: "unpivot",
        idColumns: [0],
        valueColumns: [1],
        attributeName: " ",
        valueName: "v",
      }),
    ).not.toBeNull();
    expect(
      reshapeProblem({
        type: "unpivot",
        idColumns: [0],
        valueColumns: [1],
        attributeName: "a",
        valueName: "v",
      }),
    ).toBeNull();
  });

  it("mirrors the backend pivot checks and passes transpose", () => {
    expect(
      reshapeProblem({
        type: "pivot",
        rowKeys: [],
        headerColumn: 1,
        valueColumn: 2,
        aggregation: "none",
      }),
    ).not.toBeNull();
    expect(
      reshapeProblem({
        type: "pivot",
        rowKeys: [1],
        headerColumn: 1,
        valueColumn: 2,
        aggregation: "sum",
      }),
    ).not.toBeNull();
    expect(
      reshapeProblem({
        type: "pivot",
        rowKeys: [0],
        headerColumn: 1,
        valueColumn: 2,
        aggregation: "sum",
      }),
    ).toBeNull();
    expect(reshapeProblem({ type: "transpose" })).toBeNull();
  });
});
