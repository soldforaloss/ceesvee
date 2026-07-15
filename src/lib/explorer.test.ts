import { describe, expect, it } from "vitest";

import type { FilterGroup } from "../types";
import { rangeConditions, specOf, valueCondition, withAndConditions } from "./explorer";

describe("valueCondition", () => {
  it("builds exact equals/notEquals conditions", () => {
    const only = valueCondition(3, "east", false);
    expect(only).toMatchObject({ column: 3, op: "equals", value: "east", caseSensitive: true });
    const exclude = valueCondition(3, "east", true);
    expect(exclude.op).toBe("notEquals");
  });
});

describe("rangeConditions", () => {
  it("emits bounds that are present", () => {
    expect(rangeConditions(1, "10", "20").map((c) => [c.op, c.value])).toEqual([
      ["gte", "10"],
      ["lte", "20"],
    ]);
    expect(rangeConditions(1, null, "20").map((c) => c.op)).toEqual(["lte"]);
    expect(rangeConditions(1, "10", "").map((c) => c.op)).toEqual(["gte"]);
    expect(rangeConditions(1, "", null)).toEqual([]);
  });
});

describe("withAndConditions", () => {
  const existing: FilterGroup = {
    type: "group",
    id: "root",
    conjunction: "and",
    nodes: [
      { type: "condition", id: "c1", column: 0, op: "contains", value: "x", caseSensitive: false },
    ],
  };

  it("appends to an AND root, preserving existing nodes", () => {
    const merged = withAndConditions(existing, [valueCondition(2, "a", false)]);
    expect(merged.conjunction).toBe("and");
    expect(merged.nodes).toHaveLength(2);
    expect(merged.nodes[0]).toBe(existing.nodes[0]);
  });

  it("wraps an OR root so the previous tree survives as a subgroup", () => {
    const orRoot: FilterGroup = { ...existing, conjunction: "or" };
    const merged = withAndConditions(orRoot, [valueCondition(2, "a", false)]);
    expect(merged.conjunction).toBe("and");
    expect(merged.nodes[0]).toEqual(orRoot);
    expect(merged.nodes).toHaveLength(2);
  });

  it("returns the spec untouched for no conditions", () => {
    expect(withAndConditions(existing, [])).toBe(existing);
  });
});

describe("specOf", () => {
  it("builds a fresh AND tree", () => {
    const spec = specOf([valueCondition(1, "v", false)]);
    expect(spec.conjunction).toBe("and");
    expect(spec.nodes).toHaveLength(1);
  });
});
