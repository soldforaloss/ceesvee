import { describe, expect, it } from "vitest";

import {
  buildRebuildMapping,
  defaultJsonExportOptions,
  dottedPath,
  findPathConflict,
  jsonFormatExtension,
  outputPathFor,
  suggestJsonFileName,
} from "./jsonExport";

describe("output path derivation", () => {
  it("keeps flat top-level keys when not rebuilding", () => {
    expect(outputPathFor("a.b", false)).toEqual(["a.b"]);
    expect(outputPathFor("a.b", true)).toEqual(["a", "b"]);
  });

  it("re-escapes segments for display", () => {
    expect(dottedPath(["a", "b"])).toBe("a.b");
    expect(dottedPath(["a.b", "c"])).toBe("a\\.b.c");
  });
});

describe("duplicate / conflicting path detection (mirrors the engine)", () => {
  it("passes distinct nested paths", () => {
    expect(findPathConflict(["addr.city", "addr.zip", "id"], true)).toBeNull();
  });

  it("rejects two columns rebuilding to the same path", () => {
    const conflict = findPathConflict(["a.b", "a.b"], true);
    expect(conflict?.kind).toBe("duplicate");
    expect(conflict?.columns).toEqual(["a.b", "a.b"]);
  });

  it("rejects a value that collides with an object prefix (both directions)", () => {
    const c1 = findPathConflict(["a", "a.b"], true);
    expect(c1?.kind).toBe("prefix");
    const c2 = findPathConflict(["a.b", "a"], true);
    expect(c2?.kind).toBe("prefix");
  });

  it("treats an escaped dot as a literal key — no conflict with the real path", () => {
    // "a\.b" is the single key "a.b"; "a.b" is nested a → b. Different paths.
    expect(findPathConflict(["a\\.b", "a.b"], true)).toBeNull();
  });

  it("still catches genuinely duplicate flat column names without rebuild", () => {
    expect(findPathConflict(["dup", "dup"], false)?.kind).toBe("duplicate");
    expect(findPathConflict(["a", "b"], false)).toBeNull();
    // Without rebuild, dotted names are literal keys and never nest.
    expect(findPathConflict(["a", "a.b"], false)).toBeNull();
  });
});

describe("rebuild mapping preview", () => {
  it("maps headers to nested paths and flags conflicting columns", () => {
    const { rows, conflict } = buildRebuildMapping(["a", "a.b"], true);
    expect(conflict?.kind).toBe("prefix");
    expect(rows.map((r) => r.conflict)).toEqual([true, true]);
    expect(rows[1].segments).toEqual(["a", "b"]);
    expect(rows[1].path).toBe("a.b");
  });

  it("has no conflicts for a clean mapping", () => {
    const { rows, conflict } = buildRebuildMapping(["id", "addr.city"], true);
    expect(conflict).toBeNull();
    expect(rows.every((r) => !r.conflict)).toBe(true);
  });
});

describe("format helpers", () => {
  it("chooses the right extension", () => {
    expect(jsonFormatExtension("objects")).toBe("json");
    expect(jsonFormatExtension("arrays")).toBe("json");
    expect(jsonFormatExtension("jsonLines")).toBe("jsonl");
  });

  it("suggests a file name matching the format", () => {
    expect(suggestJsonFileName("data.csv", "objects")).toBe("data.json");
    expect(suggestJsonFileName("data.csv", "jsonLines")).toBe("data.jsonl");
    expect(suggestJsonFileName("noext", "objects")).toBe("noext.json");
  });

  it("has round-trip-friendly defaults (null token, empty missing token)", () => {
    const d = defaultJsonExportOptions();
    expect(d.nullToken).toBe("null");
    expect(d.missingToken).toBe("");
    expect(d.typed).toBe(true);
  });
});
