import { describe, expect, it } from "vitest";

import type { FacetConversion, FacetResult, FacetSelection, FilterGroup } from "../types";
import {
  addFacet,
  anyFacetActive,
  clearSelection,
  createFacetSpec,
  countConditions,
  describeDropped,
  displayOrder,
  facetClipboardText,
  formatBucketCount,
  formatCount,
  formatPopulation,
  moveFacet,
  removeFacet,
  selectionActive,
  setMode,
  setRange,
  summarizeConversion,
  toggleMode,
  toggleValue,
  updateSelection,
} from "./facets";

function sel(over: Partial<FacetSelection> = {}): FacetSelection {
  return { mode: "include", values: [], range: {}, ...over };
}

describe("selection-state reducers", () => {
  it("toggleValue adds then removes a value (immutably)", () => {
    const a = sel();
    const b = toggleValue(a, "NYC");
    expect(b.values).toEqual(["NYC"]);
    expect(a.values).toEqual([]); // original untouched
    const c = toggleValue(b, "LA");
    expect(c.values).toEqual(["NYC", "LA"]);
    const d = toggleValue(c, "NYC");
    expect(d.values).toEqual(["LA"]);
  });

  it("setMode / toggleMode flip include ⇄ exclude", () => {
    expect(setMode(sel(), "exclude").mode).toBe("exclude");
    expect(toggleMode(sel({ mode: "include" })).mode).toBe("exclude");
    expect(toggleMode(sel({ mode: "exclude" })).mode).toBe("include");
  });

  it("setRange trims bounds and clears blank ones", () => {
    const r = setRange(sel(), " 25 ", "");
    expect(r.range).toEqual({ min: "25", max: null });
    const r2 = setRange(sel(), null, "100");
    expect(r2.range).toEqual({ min: null, max: "100" });
  });

  it("clearSelection empties values+range but keeps the mode", () => {
    const start = sel({ mode: "exclude", values: ["x"], range: { min: "1", max: "2" } });
    const cleared = clearSelection(start);
    expect(cleared).toEqual({ mode: "exclude", values: [], range: {} });
  });

  it("selectionActive reflects values or either range bound", () => {
    expect(selectionActive(sel())).toBe(false);
    expect(selectionActive(sel({ values: ["a"] }))).toBe(true);
    expect(selectionActive(sel({ range: { min: "1", max: null } }))).toBe(true);
    expect(selectionActive(sel({ range: { min: "  ", max: null } }))).toBe(false);
  });
});

describe("config updaters", () => {
  it("add / remove / updateSelection / anyFacetActive compose", () => {
    let cfg = { facets: [] as ReturnType<typeof createFacetSpec>[] };
    const city = createFacetSpec("text", "c0");
    cfg = addFacet(cfg, city);
    expect(cfg.facets).toHaveLength(1);
    expect(anyFacetActive(cfg)).toBe(false);

    cfg = updateSelection(cfg, city.id, (s) => toggleValue(s, "NYC"));
    expect(anyFacetActive(cfg)).toBe(true);
    expect(cfg.facets[0].selection.values).toEqual(["NYC"]);

    cfg = removeFacet(cfg, city.id);
    expect(cfg.facets).toHaveLength(0);
  });

  it("moveFacet reorders and is a no-op for out-of-range / equal indices", () => {
    const a = createFacetSpec("text", "c0");
    const b = createFacetSpec("number", "c1");
    const c = createFacetSpec("boolean", "c2");
    const cfg = { facets: [a, b, c] };
    expect(moveFacet(cfg, 0, 2).facets.map((f) => f.id)).toEqual([b.id, c.id, a.id]);
    expect(moveFacet(cfg, 2, 0).facets.map((f) => f.id)).toEqual([c.id, a.id, b.id]);
    expect(moveFacet(cfg, 1, 1)).toBe(cfg);
    expect(moveFacet(cfg, 5, 0)).toBe(cfg);
  });

  it("displayOrder floats pinned facets to the top preserving order", () => {
    const a = { ...createFacetSpec("text", "c0"), pinned: false };
    const b = { ...createFacetSpec("number", "c1"), pinned: true };
    const c = { ...createFacetSpec("boolean", "c2"), pinned: false };
    const order = displayOrder({ facets: [a, b, c] }).map((f) => f.id);
    expect(order).toEqual([b.id, a.id, c.id]);
  });
});

describe("count formatting", () => {
  it("formatCount groups digits deterministically", () => {
    expect(formatCount(0)).toBe("0");
    expect(formatCount(42)).toBe("42");
    expect(formatCount(1000)).toBe("1,000");
    expect(formatCount(1234567)).toBe("1,234,567");
    expect(formatCount(-2500)).toBe("-2,500");
  });

  it("formatBucketCount marks sampled counts with ≈", () => {
    expect(formatBucketCount(1200, false)).toBe("1,200");
    expect(formatBucketCount(1200, true)).toBe("≈ 1,200");
  });

  it("formatPopulation marks an estimated total", () => {
    expect(formatPopulation(3, 1000, false)).toBe("3 of 1,000 rows");
    expect(formatPopulation(3, 1000, true)).toBe("3 of ≈ 1,000 rows");
  });
});

describe("clipboard", () => {
  it("facetClipboardText emits value\\tcount lines", () => {
    const result = {
      buckets: [
        { key: "NYC", label: "NYC", count: 3, selected: true },
        { key: "LA", label: "LA", count: 2, selected: false },
      ],
    } as FacetResult;
    expect(facetClipboardText(result)).toBe("NYC\t3\nLA\t2");
  });
});

describe("conversion mapping", () => {
  function group(nodes: FilterGroup["nodes"]): FilterGroup {
    return { type: "group", id: "g", conjunction: "and", nodes };
  }

  it("countConditions walks nested groups", () => {
    const tree = group([
      { type: "condition", id: "1", column: 0, op: "equals", value: "NYC", caseSensitive: true },
      group([
        { type: "condition", id: "2", column: 1, op: "gte", value: "25", caseSensitive: false },
        { type: "condition", id: "3", column: 1, op: "lte", value: "40", caseSensitive: false },
      ]),
    ]);
    expect(countConditions(tree)).toBe(3);
  });

  it("summarizeConversion reports empty when no conditions, and carries dropped", () => {
    const empty: FacetConversion = {
      filter: group([]),
      dropped: [{ id: "sem", reason: "semantic facets have no column-filter equivalent" }],
    };
    const s = summarizeConversion(empty);
    expect(s.conditionCount).toBe(0);
    expect(s.empty).toBe(true);
    expect(s.dropped).toHaveLength(1);

    const nonEmpty: FacetConversion = {
      filter: group([
        { type: "condition", id: "1", column: 0, op: "equals", value: "x", caseSensitive: true },
      ]),
      dropped: [],
    };
    const s2 = summarizeConversion(nonEmpty);
    expect(s2.empty).toBe(false);
    expect(s2.conditionCount).toBe(1);
  });

  it("describeDropped pluralizes (or is empty)", () => {
    expect(describeDropped([])).toBe("");
    expect(describeDropped([{ id: "a", reason: "" }])).toContain("1 facet");
    expect(describeDropped([{ id: "a", reason: "" }])).toContain("was");
    expect(
      describeDropped([
        { id: "a", reason: "" },
        { id: "b", reason: "" },
      ]),
    ).toContain("2 facets");
  });
});
