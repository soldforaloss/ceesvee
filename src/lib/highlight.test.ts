import { describe, expect, it } from "vitest";

import {
  CONDITION_LABELS,
  EMPHASIS_LABELS,
  HIGHLIGHT_TONE_RGB,
  conditionColumnId,
  conditionReserved,
  conditionSupportsColumn,
  defaultCondition,
  describeCondition,
  highlightAccent,
  highlightBackground,
  highlightFontStyle,
  newHighlightRule,
  orderRulesByPriority,
  validateHighlightRule,
} from "./highlight";
import type { HighlightCondition, HighlightRule, HighlightTarget } from "../types";

function rule(
  id: string,
  priority: number,
  condition: HighlightCondition = { type: "blank", columnId: null },
  target: HighlightTarget = { type: "cell" },
): HighlightRule {
  return {
    id,
    name: id,
    condition,
    target,
    priority,
    decoration: { tone: "accent", emphasis: "normal", icon: null, textStyle: "normal" },
    enabled: true,
  };
}

describe("orderRulesByPriority", () => {
  it("orders by priority descending", () => {
    const ordered = orderRulesByPriority([rule("a", 1), rule("b", 9), rule("c", 5)]);
    expect(ordered.map((r) => r.id)).toEqual(["b", "c", "a"]);
  });

  it("breaks ties deterministically by ascending id (matches the backend)", () => {
    const ordered = orderRulesByPriority([rule("z", 5), rule("a", 5), rule("m", 5)]);
    expect(ordered.map((r) => r.id)).toEqual(["a", "m", "z"]);
  });

  it("does not mutate its input", () => {
    const input = [rule("a", 1), rule("b", 9)];
    const before = input.map((r) => r.id);
    orderRulesByPriority(input);
    expect(input.map((r) => r.id)).toEqual(before);
  });
});

describe("validateHighlightRule", () => {
  it("accepts a well-formed rule", () => {
    expect(validateHighlightRule(rule("a", 0))).toBeNull();
  });

  it("rejects an empty id", () => {
    expect(validateHighlightRule(rule("  ", 0))).toMatch(/id/i);
  });

  it("rejects an invalid regex", () => {
    const r = rule("a", 0, { type: "regex", columnId: null, pattern: "([", caseSensitive: false });
    expect(validateHighlightRule(r)).toMatch(/regular expression/i);
  });

  it("accepts a valid regex", () => {
    const r = rule("a", 0, {
      type: "regex",
      columnId: null,
      pattern: "^\\d+$",
      caseSensitive: false,
    });
    expect(validateHighlightRule(r)).toBeNull();
  });

  it("rejects a numeric range with min greater than max", () => {
    const r = rule("a", 0, {
      type: "numericRange",
      columnId: null,
      min: 10,
      max: 1,
      inclusive: true,
    });
    expect(validateHighlightRule(r)).toMatch(/≤ maximum/);
  });

  it("rejects a numeric range with no bounds", () => {
    const r = rule("a", 0, {
      type: "numericRange",
      columnId: null,
      min: null,
      max: null,
      inclusive: true,
    });
    expect(validateHighlightRule(r)).toMatch(/minimum|maximum/i);
  });

  it("rejects an unparseable date bound", () => {
    const r = rule("a", 0, { type: "dateRange", columnId: null, min: "not-a-date", max: null });
    expect(validateHighlightRule(r)).toMatch(/date/i);
  });

  it("rejects a date range with min after max", () => {
    const r = rule("a", 0, {
      type: "dateRange",
      columnId: null,
      min: "2025-01-01",
      max: "2024-01-01",
    });
    expect(validateHighlightRule(r)).toMatch(/≤ maximum/);
  });

  it("rejects an empty tag", () => {
    const r = rule("a", 0, { type: "tagged", tag: "  " });
    expect(validateHighlightRule(r)).toMatch(/tag/i);
  });

  it("rejects a columns target with no columns", () => {
    const r = rule("a", 0, { type: "blank", columnId: null }, { type: "columns", columnIds: [] });
    expect(validateHighlightRule(r)).toMatch(/at least one column/i);
  });

  it("rejects an equals condition with no value", () => {
    const r = rule("a", 0, { type: "equals", columnId: null, value: "", caseSensitive: false });
    expect(validateHighlightRule(r)).toMatch(/value/i);
  });
});

describe("semantic token mapping", () => {
  it("maps every tone to an rgb triple", () => {
    for (const tone of Object.keys(HIGHLIGHT_TONE_RGB) as (keyof typeof HIGHLIGHT_TONE_RGB)[]) {
      expect(HIGHLIGHT_TONE_RGB[tone]).toHaveLength(3);
    }
  });

  it("increases background opacity with emphasis", () => {
    const alpha = (s: string) => Number(s.slice(s.lastIndexOf(",") + 1, -1).trim());
    const subtle = alpha(highlightBackground("accent", "subtle", false));
    const normal = alpha(highlightBackground("accent", "normal", false));
    const strong = alpha(highlightBackground("accent", "strong", false));
    expect(subtle).toBeLessThan(normal);
    expect(normal).toBeLessThan(strong);
  });

  it("uses a heavier tint in dark mode than light for the same emphasis", () => {
    const alpha = (s: string) => Number(s.slice(s.lastIndexOf(",") + 1, -1).trim());
    expect(alpha(highlightBackground("warn", "normal", true))).toBeGreaterThan(
      alpha(highlightBackground("warn", "normal", false)),
    );
  });

  it("derives the tone background from its rgb triple", () => {
    const [r, g, b] = HIGHLIGHT_TONE_RGB.error;
    expect(highlightBackground("error", "subtle", false)).toContain(`rgba(${r}, ${g}, ${b}`);
  });

  it("lightens the accent colour for dark surfaces", () => {
    expect(highlightAccent("neutral", true)).not.toEqual(highlightAccent("neutral", false));
  });

  it("maps text styles to a font override, leaving normal undefined", () => {
    expect(highlightFontStyle("normal")).toBeUndefined();
    expect(highlightFontStyle("bold")).toContain("600");
    expect(highlightFontStyle("italic")).toContain("italic");
  });
});

describe("condition helpers", () => {
  it("labels every condition kind", () => {
    for (const kind of Object.keys(CONDITION_LABELS) as (keyof typeof CONDITION_LABELS)[]) {
      expect(CONDITION_LABELS[kind].length).toBeGreaterThan(0);
    }
  });

  it("knows which conditions carry a column scope", () => {
    expect(conditionSupportsColumn("equals")).toBe(true);
    expect(conditionSupportsColumn("outlier")).toBe(false);
    expect(conditionSupportsColumn("diagnostic")).toBe(false);
  });

  it("flags the reserved F40 annotation conditions", () => {
    expect(conditionReserved("bookmarked")).toBe(true);
    expect(conditionReserved("tagged")).toBe(true);
    expect(conditionReserved("equals")).toBe(false);
  });

  it("reads a condition's column scope", () => {
    expect(conditionColumnId({ type: "blank", columnId: "c1" })).toBe("c1");
    expect(conditionColumnId({ type: "blank", columnId: null })).toBeUndefined();
    expect(conditionColumnId({ type: "outlier" })).toBeUndefined();
  });

  it("builds fresh default conditions of each requested kind", () => {
    expect(defaultCondition("regex")).toMatchObject({ type: "regex", pattern: "" });
    expect(defaultCondition("numericRange")).toMatchObject({
      type: "numericRange",
      inclusive: true,
    });
    expect(defaultCondition("duplicate")).toMatchObject({ type: "duplicate", trim: true });
  });

  it("describes a condition using the column display name", () => {
    const text = describeCondition(
      { type: "equals", columnId: "c1", value: "x", caseSensitive: false },
      (id) => (id === "c1" ? "Status" : id),
    );
    expect(text).toContain("Status");
    expect(text).toContain("x");
  });
});

describe("newHighlightRule", () => {
  it("places a fresh rule above the current maximum priority", () => {
    const r = newHighlightRule([rule("a", 3), rule("b", 7)]);
    expect(r.priority).toBe(8);
    expect(EMPHASIS_LABELS[r.decoration.emphasis]).toBeDefined();
  });

  it("gives each new rule a distinct id", () => {
    expect(newHighlightRule([]).id).not.toEqual(newHighlightRule([]).id);
  });
});
