import { describe, expect, it } from "vitest";

import {
  describeShape,
  escapeJsonKey,
  explodingFields,
  isIgnored,
  needsMultiArrayChoice,
  pathSegments,
  projectColumns,
  splitJsonPath,
  toggleIgnorePath,
  validateImportOptions,
  defaultImportOptions,
} from "./jsonImport";
import type { ArrayFieldInfo, JsonImportPreview, PreviewColumn } from "../types";

const col = (name: string, over: Partial<PreviewColumn> = {}): PreviewColumn => ({
  name,
  inferredType: "text",
  present: 1,
  nulls: 0,
  missing: 0,
  ...over,
});

const arr = (path: string, over: Partial<ArrayFieldInfo> = {}): ArrayFieldInfo => ({
  path,
  occurrences: 1,
  maxLen: 2,
  primitivesOnly: true,
  ...over,
});

const preview = (over: Partial<JsonImportPreview> = {}): JsonImportPreview => ({
  shape: "objectArray",
  pointer: "",
  needsPointer: false,
  candidates: [],
  recordKind: "object",
  columns: [],
  nestedObjectPaths: [],
  arrayFields: [],
  recordCount: 1,
  projectedRows: 1,
  projectedColumns: 0,
  sampleRows: [],
  exploded: false,
  warnings: [],
  ...over,
});

describe("path escaping (mirrors the Rust engine)", () => {
  it("escapes dots and backslashes in keys", () => {
    expect(escapeJsonKey("plain")).toBe("plain");
    expect(escapeJsonKey("a.b")).toBe("a\\.b");
    expect(escapeJsonKey("a\\b")).toBe("a\\\\b");
    expect(escapeJsonKey("a.b\\c")).toBe("a\\.b\\\\c");
  });

  it("splits flattened paths back into original segments", () => {
    expect(splitJsonPath("a.b.c")).toEqual(["a", "b", "c"]);
    // An escaped dot is a literal key, not a separator.
    expect(splitJsonPath("a\\.b")).toEqual(["a.b"]);
    expect(splitJsonPath("a\\\\b")).toEqual(["a\\b"]);
    expect(splitJsonPath("")).toEqual([""]);
  });

  it("round-trips a single escaped key", () => {
    for (const key of ["a.b", "weird\\key", "x.y\\z", "plain"]) {
      expect(splitJsonPath(escapeJsonKey(key))).toEqual([key]);
    }
  });

  it("exposes segments for display", () => {
    expect(pathSegments("user\\.name.first")).toEqual(["user.name", "first"]);
  });
});

describe("ignore-path projection", () => {
  it("ignores a path and everything nested under it, but not sibling prefixes", () => {
    expect(isIgnored("a", ["a"])).toBe(true);
    expect(isIgnored("a.b", ["a"])).toBe(true);
    expect(isIgnored("ab", ["a"])).toBe(false);
    expect(isIgnored("a", ["b"])).toBe(false);
    expect(isIgnored("x", [""])).toBe(false);
  });

  it("derives projected columns by dropping ignored paths", () => {
    const columns = [col("id"), col("addr.city"), col("addr.zip"), col("name")];
    expect(projectColumns(columns, ["addr"]).map((c) => c.name)).toEqual(["id", "name"]);
    expect(projectColumns(columns, []).map((c) => c.name)).toEqual([
      "id",
      "addr.city",
      "addr.zip",
      "name",
    ]);
  });

  it("toggles ignore entries idempotently", () => {
    expect(toggleIgnorePath([], "a")).toEqual(["a"]);
    expect(toggleIgnorePath(["a", "b"], "a")).toEqual(["b"]);
  });
});

describe("array explosion policy state", () => {
  it("only the explode policy produces exploding fields", () => {
    const fields = [arr("tags"), arr("scores")];
    expect(explodingFields(fields, "preserveJson", [])).toEqual([]);
    expect(explodingFields(fields, "join", [])).toEqual([]);
    expect(explodingFields(fields, "explode", []).map((f) => f.path)).toEqual(["tags", "scores"]);
  });

  it("ignored array fields do not explode", () => {
    const fields = [arr("tags"), arr("scores")];
    expect(explodingFields(fields, "explode", ["scores"]).map((f) => f.path)).toEqual(["tags"]);
  });

  it("requires a cartesian/zip choice for two exploding fields", () => {
    const fields = [arr("tags"), arr("scores")];
    expect(
      needsMultiArrayChoice(fields, {
        arrayPolicy: "explode",
        ignorePaths: [],
        multiArray: undefined,
      }),
    ).toBe(true);
    // One exploding field needs no choice.
    expect(
      needsMultiArrayChoice(fields, {
        arrayPolicy: "explode",
        ignorePaths: ["scores"],
        multiArray: undefined,
      }),
    ).toBe(false);
    // A chosen mode clears the requirement.
    expect(
      needsMultiArrayChoice(fields, { arrayPolicy: "explode", ignorePaths: [], multiArray: "zip" }),
    ).toBe(false);
  });
});

describe("import option validation", () => {
  it("rejects equal null and missing tokens", () => {
    const opts = { ...defaultImportOptions(), nullToken: "x", missingToken: "x" };
    expect(validateImportOptions(opts, null)).toHaveLength(1);
    expect(validateImportOptions(opts, null)[0]).toMatch(/must differ/);
  });

  it("requires a pointer when the preview asks for one", () => {
    const opts = { ...defaultImportOptions(), pointer: undefined };
    const errs = validateImportOptions(opts, preview({ needsPointer: true }));
    expect(errs.some((e) => /JSON Pointer/.test(e))).toBe(true);
    // The empty-string root pointer satisfies it.
    const ok = validateImportOptions({ ...opts, pointer: "" }, preview({ needsPointer: true }));
    expect(ok.some((e) => /JSON Pointer/.test(e))).toBe(false);
  });

  it("blocks a dual-array explode without a mode", () => {
    const opts = { ...defaultImportOptions(), arrayPolicy: "explode" as const };
    const errs = validateImportOptions(
      opts,
      preview({ arrayFields: [arr("tags"), arr("scores")] }),
    );
    expect(errs.some((e) => /cartesian or zip/.test(e))).toBe(true);
  });

  it("passes a clean single-array explode", () => {
    const opts = { ...defaultImportOptions(), arrayPolicy: "explode" as const };
    expect(validateImportOptions(opts, preview({ arrayFields: [arr("tags")] }))).toEqual([]);
  });
});

describe("shape labels", () => {
  it("maps every shape to a human label", () => {
    expect(describeShape("jsonLines")).toMatch(/NDJSON/);
    expect(describeShape("objectArray")).toBe("Array of objects");
  });
});
