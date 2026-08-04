import { describe, expect, it } from "vitest";

import {
  chunkUnitLabel,
  columnDepth,
  columnarFormatExtension,
  columnarFormatLabel,
  columnarOpenPlan,
  compressionLabel,
  defaultColumnarExportOptions,
  defaultColumnarOpenOptions,
  effectivePolicy,
  estimatedMemoryLabel,
  explodeFields,
  isColumnarPath,
  leafName,
  setFieldPolicy,
  suggestColumnarFileName,
} from "./columnar";
import type { ColumnarInspection, ColumnarOpenOptions } from "../types";

function inspection(overrides: Partial<ColumnarInspection> = {}): ColumnarInspection {
  return {
    format: "parquet",
    rowCount: 100,
    chunkCount: 2,
    compression: "SNAPPY",
    columns: [],
    complexFields: [],
    estimatedMemory: 5 * 1024 * 1024,
    needsDecision: false,
    fileSize: 1024,
    ...overrides,
  };
}

describe("format & compression labels", () => {
  it("labels each container, noting Feather v2 for the Arrow IPC file", () => {
    expect(columnarFormatLabel("parquet")).toBe("Apache Parquet");
    expect(columnarFormatLabel("arrowFile")).toContain("Feather v2");
    expect(columnarFormatLabel("arrowStream")).toBe("Arrow IPC stream");
  });

  it("labels compression codecs", () => {
    expect(compressionLabel("uncompressed")).toMatch(/uncompressed/i);
    expect(compressionLabel("snappy")).toBe("Snappy");
    expect(compressionLabel("zstd")).toMatch(/zstd/i);
  });
});

describe("suggested export file names", () => {
  it("maps each format to its extension", () => {
    expect(columnarFormatExtension("parquet")).toBe("parquet");
    expect(columnarFormatExtension("arrowFile")).toBe("arrow");
    expect(columnarFormatExtension("arrowStream")).toBe("arrows");
  });

  it("swaps an existing extension for the format's own", () => {
    expect(suggestColumnarFileName("sales.csv", "parquet")).toBe("sales.parquet");
    expect(suggestColumnarFileName("sales.parquet", "arrowFile")).toBe("sales.arrow");
    expect(suggestColumnarFileName("no-ext", "arrowStream")).toBe("no-ext.arrows");
  });

  it("does not treat a dotted directory as an extension boundary", () => {
    expect(suggestColumnarFileName("v1.2/data", "parquet")).toBe("v1.2/data.parquet");
  });
});

describe("columnar path routing", () => {
  it("recognises every columnar extension, case-insensitively", () => {
    expect(isColumnarPath("C:/x.parquet")).toBe(true);
    expect(isColumnarPath("C:/x.ARROW")).toBe(true);
    expect(isColumnarPath("C:/x.feather")).toBe(true);
    expect(isColumnarPath("C:/x.ipc")).toBe(true);
    expect(isColumnarPath("C:/x.arrows")).toBe(true);
  });

  it("leaves CSV and JSON alone", () => {
    expect(isColumnarPath("C:/x.csv")).toBe(false);
    expect(isColumnarPath("C:/x.json")).toBe(false);
  });
});

describe("defaults", () => {
  it("open defaults preserve complex fields as JSON", () => {
    expect(defaultColumnarOpenOptions()).toEqual({
      complexPolicy: "preserveJson",
      fieldPolicies: {},
      cacheBudgetBytes: 0,
    });
  });

  it("export defaults match the Rust defaults (Snappy Parquet, typed)", () => {
    expect(defaultColumnarExportOptions()).toEqual({
      format: "parquet",
      compression: "snappy",
      typed: true,
      rowGroupRows: 0,
      backup: "none",
    });
  });
});

describe("complex-field policy state", () => {
  const base = defaultColumnarOpenOptions();

  it("falls back to the default policy when no override is set", () => {
    expect(effectivePolicy(base, "items")).toBe("preserveJson");
    expect(effectivePolicy({ complexPolicy: "reject" }, "items")).toBe("reject");
    expect(effectivePolicy({}, "items")).toBe("preserveJson");
  });

  it("honours a per-field override over the default", () => {
    const opts = setFieldPolicy(base, "items", "explode");
    expect(effectivePolicy(opts, "items")).toBe("explode");
    expect(effectivePolicy(opts, "other")).toBe("preserveJson");
  });

  it("setFieldPolicy is immutable and merges with existing overrides", () => {
    const a = setFieldPolicy(base, "items", "explode");
    const b = setFieldPolicy(a, "tags", "reject");
    expect(base.fieldPolicies).toEqual({});
    expect(a.fieldPolicies).toEqual({ items: "explode" });
    expect(b.fieldPolicies).toEqual({ items: "explode", tags: "reject" });
  });

  it("collects the fields effectively set to explode, in order", () => {
    const opts: ColumnarOpenOptions = { fieldPolicies: { b: "explode", a: "explode" } };
    expect(explodeFields(opts, ["a", "b", "c"])).toEqual(["a", "b"]);
  });
});

describe("open-mode plan", () => {
  const insp = inspection({ complexFields: ["items", "tags"] });

  it("keeps both modes available when nothing explodes", () => {
    const plan = columnarOpenPlan(insp, defaultColumnarOpenOptions());
    expect(plan.exploded).toEqual([]);
    expect(plan.requiresEditable).toBe(false);
    expect(plan.indexedDisabledReason).toBeNull();
    expect(plan.errors).toEqual([]);
  });

  it("one explode forces editable and disables the indexed mode", () => {
    const opts = setFieldPolicy(defaultColumnarOpenOptions(), "items", "explode");
    const plan = columnarOpenPlan(insp, opts);
    expect(plan.exploded).toEqual(["items"]);
    expect(plan.requiresEditable).toBe(true);
    expect(plan.tooManyExplode).toBe(false);
    expect(plan.indexedDisabledReason).toMatch(/editable/i);
    expect(plan.errors).toEqual([]);
  });

  it("two explodes are a blocking error for both modes", () => {
    let opts = setFieldPolicy(defaultColumnarOpenOptions(), "items", "explode");
    opts = setFieldPolicy(opts, "tags", "explode");
    const plan = columnarOpenPlan(insp, opts);
    expect(plan.tooManyExplode).toBe(true);
    expect(plan.errors).toHaveLength(1);
    expect(plan.errors[0]).toMatch(/one field/i);
  });
});

describe("inspection display", () => {
  it("labels the chunk unit per format and count", () => {
    expect(chunkUnitLabel("parquet", 1)).toBe("row group");
    expect(chunkUnitLabel("parquet", 3)).toBe("row groups");
    expect(chunkUnitLabel("arrowFile", 1)).toBe("record batch");
    expect(chunkUnitLabel("arrowStream", 2)).toBe("record batches");
  });

  it("formats the editable-memory estimate", () => {
    expect(estimatedMemoryLabel(inspection({ estimatedMemory: 5 * 1024 * 1024 }))).toBe("5.0 MB");
  });

  it("computes nesting depth and leaf name of a flattened path", () => {
    expect(columnDepth("id")).toBe(0);
    expect(columnDepth("addr.city")).toBe(1);
    expect(columnDepth("a.b.c")).toBe(2);
    expect(leafName("addr.city")).toBe("city");
    // An escaped dot is part of the leaf, not a separator.
    expect(columnDepth("weird\\.name")).toBe(0);
    expect(leafName("weird\\.name")).toBe("weird.name");
  });
});
