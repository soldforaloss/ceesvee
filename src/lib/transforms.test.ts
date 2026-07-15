import { describe, expect, it } from "vitest";

import { buildTransformSpec, defaultValues, TRANSFORMS } from "./transforms";

describe("transform catalog", () => {
  it("covers all sixteen operations", () => {
    expect(TRANSFORMS).toHaveLength(16);
    const types = TRANSFORMS.map((t) => t.type);
    expect(new Set(types).size).toBe(16);
  });

  it("provides sensible defaults", () => {
    const dates = TRANSFORMS.find((t) => t.type === "normalizeDates")!;
    expect(defaultValues(dates)).toEqual({ format: "%Y-%m-%d" });
    const merge = TRANSFORMS.find((t) => t.type === "mergeColumns")!;
    expect(defaultValues(merge)).toEqual({ columns: [], separator: " " });
  });
});

describe("buildTransformSpec", () => {
  it("builds parameterless specs", () => {
    expect(buildTransformSpec("trim", {})).toEqual({ type: "trim" });
  });

  it("validates required parameters", () => {
    expect(buildTransformSpec("replaceText", { find: "", replace: "x" })).toEqual({
      error: "Enter the text to find",
    });
    expect(buildTransformSpec("mergeColumns", { columns: [1], separator: "-" })).toEqual({
      error: "Pick at least two columns to merge",
    });
    expect(buildTransformSpec("splitByDelimiter", { column: 2, delimiter: "" })).toEqual({
      error: "Enter a delimiter",
    });
  });

  it("assembles full specs with coerced values", () => {
    expect(
      buildTransformSpec("replaceText", { find: "a", replace: "b", caseSensitive: false }),
    ).toEqual({ type: "replaceText", find: "a", replace: "b", caseSensitive: false });
    expect(buildTransformSpec("mergeColumns", { columns: [2, 0], separator: ", " })).toEqual({
      type: "mergeColumns",
      columns: [2, 0],
      separator: ", ",
    });
    expect(buildTransformSpec("splitByDelimiter", { column: 1, delimiter: ";" })).toEqual({
      type: "splitByDelimiter",
      column: 1,
      delimiter: ";",
    });
  });
});
