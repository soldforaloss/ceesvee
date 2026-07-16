import { describe, expect, it } from "vitest";

import { describeRecipeStep, parseRecipeJson, RECIPE_VERSION } from "./recipes";

describe("parseRecipeJson", () => {
  it("round-trips a valid recipe", () => {
    const json = JSON.stringify({
      version: RECIPE_VERSION,
      name: "clean",
      steps: [{ type: "sort", keys: [{ column: "id" }] }],
    });
    const recipe = parseRecipeJson(json);
    expect(recipe.name).toBe("clean");
    expect(recipe.steps).toHaveLength(1);
  });

  it("rejects bad JSON, bad shapes, and future versions", () => {
    expect(() => parseRecipeJson("{nope")).toThrow(/JSON/);
    expect(() => parseRecipeJson("{}")).toThrow(/valid recipe/);
    expect(() => parseRecipeJson(JSON.stringify({ version: 99, name: "x", steps: [] }))).toThrow(
      /version 99/,
    );
  });
});

describe("describeRecipeStep", () => {
  it("covers every step type", () => {
    expect(describeRecipeStep({ type: "selectColumns", columns: ["a", "b"] })).toContain("a, b");
    expect(
      describeRecipeStep({ type: "sort", keys: [{ column: "n", descending: true }] }),
    ).toContain("n desc");
    expect(
      describeRecipeStep({ type: "transform", spec: { type: "trim" }, columns: [] }),
    ).toContain("all columns");
    expect(
      describeRecipeStep({
        type: "deduplicate",
        spec: {
          keyColumns: [],
          trim: false,
          caseInsensitive: false,
          collapseWhitespace: false,
          blankKeysEqual: false,
          excludeBlankKeys: false,
        },
        keep: "first",
      }),
    ).toContain("first");
  });
});
