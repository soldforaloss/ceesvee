import { describe, expect, it } from "vitest";

import { aggregateNeedsColumn, normalizeAggregates, usesConcat } from "./groupby";

describe("group-by helpers", () => {
  it("only the row count needs no column", () => {
    expect(aggregateNeedsColumn("count")).toBe(false);
    expect(aggregateNeedsColumn("sum")).toBe(true);
    expect(aggregateNeedsColumn("concatDistinct")).toBe(true);
  });

  it("detects concat aggregates", () => {
    expect(usesConcat([{ aggregate: "sum", column: 0 }])).toBe(false);
    expect(usesConcat([{ aggregate: "concat", column: 0 }])).toBe(true);
    expect(usesConcat([{ aggregate: "concatDistinct", column: 0 }])).toBe(true);
  });

  it("normalizes specs: count drops its column, others default to 0", () => {
    const out = normalizeAggregates([
      { aggregate: "count", column: 3 },
      { aggregate: "sum", column: null },
    ]);
    expect(out[0].column).toBeNull();
    expect(out[1].column).toBe(0);
  });
});
