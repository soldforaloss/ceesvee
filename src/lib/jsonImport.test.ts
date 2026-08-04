import { describe, expect, it } from "vitest";

import {
  canApplyImport,
  describeShape,
  escapeJsonKey,
  explodingFields,
  isIgnored,
  needsMultiArrayChoice,
  pathSegments,
  previewReflectsOptions,
  projectColumns,
  splitJsonPath,
  toggleIgnorePath,
  validateImportOptions,
  defaultImportOptions,
} from "./jsonImport";
import type { ArrayFieldInfo, JsonImportOptions, JsonImportPreview, PreviewColumn } from "../types";

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
  maxRecordDims: 0,
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

  it("requires a cartesian/zip choice only when a record co-occurs 2+ arrays", () => {
    // Two array dimensions in a single record (maxRecordDims >= 2).
    expect(
      needsMultiArrayChoice(preview({ maxRecordDims: 2 }), {
        arrayPolicy: "explode",
        multiArray: undefined,
      }),
    ).toBe(true);
    // Two array FIELDS that never co-occur in one record (maxRecordDims 1):
    // no choice needed even though the document-wide list has two entries.
    expect(
      needsMultiArrayChoice(preview({ maxRecordDims: 1, arrayFields: [arr("x"), arr("y")] }), {
        arrayPolicy: "explode",
        multiArray: undefined,
      }),
    ).toBe(false);
    // A chosen mode clears the requirement.
    expect(
      needsMultiArrayChoice(preview({ maxRecordDims: 2 }), {
        arrayPolicy: "explode",
        multiArray: "zip",
      }),
    ).toBe(false);
    // A non-explode policy never needs the choice.
    expect(
      needsMultiArrayChoice(preview({ maxRecordDims: 2 }), {
        arrayPolicy: "preserveJson",
        multiArray: undefined,
      }),
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

  it("blocks a dual-array explode without a mode (per-record co-occurrence)", () => {
    const opts = { ...defaultImportOptions(), arrayPolicy: "explode" as const };
    const errs = validateImportOptions(opts, preview({ maxRecordDims: 2 }));
    expect(errs.some((e) => /cartesian or zip/.test(e))).toBe(true);
  });

  it("passes a clean single-array explode", () => {
    const opts = { ...defaultImportOptions(), arrayPolicy: "explode" as const };
    expect(validateImportOptions(opts, preview({ maxRecordDims: 1 }))).toEqual([]);
  });

  it("does not block two array fields that never co-occur in one record", () => {
    const opts = { ...defaultImportOptions(), arrayPolicy: "explode" as const };
    // Document-wide there are two array fields, but no single record explodes
    // both at once (maxRecordDims === 1), so the import must not be blocked.
    const errs = validateImportOptions(
      opts,
      preview({ maxRecordDims: 1, arrayFields: [arr("x"), arr("y")] }),
    );
    expect(errs).toEqual([]);
  });
});

describe("import-apply gate", () => {
  // A gate that is runnable by default, so each case flips exactly one input.
  const scanned: JsonImportOptions = defaultImportOptions();
  const runnable = () => ({
    importing: false,
    scanning: false,
    scanError: null as string | null,
    errors: [] as string[],
    hasColumns: true,
    editedOptions: { ...scanned },
    scannedOptions: { ...scanned },
  });

  it("allows import when the preview reflects the edited options", () => {
    expect(canApplyImport(runnable())).toBe(true);
  });

  it("blocks import while a scan is running or an import is in flight", () => {
    expect(canApplyImport({ ...runnable(), scanning: true })).toBe(false);
    expect(canApplyImport({ ...runnable(), importing: true })).toBe(false);
  });

  it("blocks import on validation errors or when there is nothing to import", () => {
    expect(canApplyImport({ ...runnable(), errors: ["bad"] })).toBe(false);
    expect(canApplyImport({ ...runnable(), hasColumns: false })).toBe(false);
  });

  it("blocks import while the edited options differ from the scanned preview", () => {
    // The reviewer's case: an option changed but the shown preview still has
    // columns (within the debounce / rescan). Importing would apply options
    // the preview never represented.
    const stale = { ...runnable(), editedOptions: { ...scanned, arrayPolicy: "explode" as const } };
    expect(previewReflectsOptions(stale.editedOptions, stale.scannedOptions)).toBe(false);
    expect(canApplyImport(stale)).toBe(false);
  });

  it("blocks import after a failed rescan, and before the first scan", () => {
    expect(canApplyImport({ ...runnable(), scanError: "scan failed" })).toBe(false);
    expect(canApplyImport({ ...runnable(), scannedOptions: null })).toBe(false);
  });

  it("re-enables import once the preview catches up to the options", () => {
    const changed = { ...scanned, arrayPolicy: "explode" as const };
    // Stale while only the edited side changed…
    expect(previewReflectsOptions(changed, scanned)).toBe(false);
    // …and current again once the scan records the same options.
    expect(previewReflectsOptions(changed, changed)).toBe(true);
    expect(canApplyImport({ ...runnable(), editedOptions: changed, scannedOptions: changed })).toBe(
      true,
    );
  });
});

describe("shape labels", () => {
  it("maps every shape to a human label", () => {
    expect(describeShape("jsonLines")).toMatch(/NDJSON/);
    expect(describeShape("objectArray")).toBe("Array of objects");
  });
});
