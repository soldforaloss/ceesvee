import { describe, expect, it } from "vitest";

import type { CrossRule } from "../types";
import { describeRule, emptyRule, parseCombinations, ruleProblem } from "./crossval";

describe("describeRule", () => {
  it("covers every rule type", () => {
    const rules: CrossRule[] = [
      { type: "columnsEqual", left: "a", right: "b", negate: true },
      { type: "numericCompare", left: "net", op: "le", right: "gross" },
      { type: "dateOrder", earlier: "start", later: "end", allowEqual: true },
      {
        type: "conditionalRequired",
        whenColumn: "status",
        when: { type: "equals", value: "rejected" },
        thenRequired: "reason",
      },
      { type: "exactlyOne", columns: ["a", "b"] },
      { type: "atLeastOne", columns: ["a", "b"] },
      { type: "atMostOne", columns: ["a", "b"] },
      { type: "sumEquals", parts: ["net", "tax"], total: "gross", tolerance: 0.01 },
      { type: "allowedCombinations", columns: ["country"], allowed: [["US"]] },
    ];
    const texts = rules.map(describeRule);
    expect(texts[0]).toContain("differ");
    expect(texts[1]).toContain("≤");
    expect(texts[2]).toContain("on or before");
    expect(texts[3]).toContain('when "status"');
    expect(texts[7]).toContain("±0.01");
    expect(texts[8]).toContain("1 allowed combination");
    expect(new Set(texts).size).toBe(texts.length);
  });
});

describe("ruleProblem", () => {
  it("mirrors the backend shape checks", () => {
    expect(ruleProblem({ type: "columnsEqual", left: "a", right: "a" })).not.toBeNull();
    expect(ruleProblem({ type: "exactlyOne", columns: ["a"] })).not.toBeNull();
    expect(ruleProblem({ type: "sumEquals", parts: [], total: "t", tolerance: 0 })).not.toBeNull();
    expect(
      ruleProblem({ type: "sumEquals", parts: ["a"], total: "t", tolerance: -1 }),
    ).not.toBeNull();
    expect(
      ruleProblem({ type: "allowedCombinations", columns: ["a", "b"], allowed: [["x"]] }),
    ).not.toBeNull();
    expect(ruleProblem({ type: "numericCompare", left: "a", op: "lt", right: "b" })).toBeNull();
  });
});

describe("emptyRule", () => {
  it("seeds every type with real headers", () => {
    const headers = ["one", "two", "three"];
    const types: CrossRule["type"][] = [
      "columnsEqual",
      "numericCompare",
      "dateOrder",
      "conditionalRequired",
      "exactlyOne",
      "atLeastOne",
      "atMostOne",
      "sumEquals",
      "allowedCombinations",
    ];
    for (const t of types) {
      const rule = emptyRule(t, headers);
      expect(rule.type).toBe(t);
    }
    const eq = emptyRule("columnsEqual", headers);
    expect(eq).toMatchObject({ left: "one", right: "two" });
  });
});

describe("parseCombinations", () => {
  it("splits lines and trims values, skipping blanks", () => {
    expect(parseCombinations("US, USD\n\nDE,EUR\n  \n")).toEqual([
      ["US", "USD"],
      ["DE", "EUR"],
    ]);
  });
});
