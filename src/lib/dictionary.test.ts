import { describe, expect, it } from "vitest";

import type { DictionaryField, DictionaryFieldKey, FieldConflict } from "../types";
import {
  allConflictsResolved,
  buildPerFieldResolution,
  bulkChoices,
  completeness,
  conflictKey,
  fieldValue,
  filledFieldCount,
  isDocumented,
  isSensitive,
  normalizeField,
  unresolvedCount,
} from "./dictionary";

const field = (over: Partial<DictionaryField> = {}): DictionaryField => ({
  columnId: "c0",
  ...over,
});

const conflict = (columnId: string, key: DictionaryFieldKey): FieldConflict => ({
  columnId,
  columnName: columnId,
  field: key,
  existing: "old",
  incoming: "new",
});

describe("fieldValue / presence", () => {
  it("treats whitespace-only text as absent, like the backend", () => {
    expect(fieldValue(field({ description: "  " }), "description")).toBeNull();
    expect(fieldValue(field({ description: "  hi " }), "description")).toBe("hi");
    expect(fieldValue(field(), "description")).toBeNull();
  });

  it("reads enum fields directly and joins non-blank allowed values", () => {
    expect(fieldValue(field({ role: "measure" }), "role")).toBe("measure");
    expect(fieldValue(field({ sensitivity: "restricted" }), "sensitivity")).toBe("restricted");
    expect(fieldValue(field({ allowedValues: ["a", " ", "b"] }), "allowedValues")).toBe("a, b");
    expect(fieldValue(field({ allowedValues: ["  "] }), "allowedValues")).toBeNull();
    expect(fieldValue(field({ allowedValues: [] }), "allowedValues")).toBeNull();
  });
});

describe("completeness", () => {
  it("counts only fields carrying a real value out of the ten", () => {
    const f = field({
      displayName: "Email",
      description: " ", // blank → not counted
      role: "dimension",
      allowedValues: ["x"],
    });
    expect(filledFieldCount(f)).toBe(3);
    const c = completeness(f);
    expect(c).toEqual({ filled: 3, total: 10, fraction: 0.3 });
  });

  it("is empty for an undocumented column and full when every field is set", () => {
    expect(isDocumented(field())).toBe(false);
    expect(completeness(field()).fraction).toBe(0);
    const full = field({
      displayName: "n",
      description: "d",
      role: "label",
      unit: "u",
      source: "s",
      sensitivity: "public",
      allowedValues: ["v"],
      example: "e",
      owner: "o",
      notes: "n2",
    });
    expect(completeness(full)).toEqual({ filled: 10, total: 10, fraction: 1 });
    expect(isDocumented(full)).toBe(true);
  });
});

describe("isSensitive", () => {
  it("flags only confidential and restricted", () => {
    expect(isSensitive("public")).toBe(false);
    expect(isSensitive("internal")).toBe(false);
    expect(isSensitive("confidential")).toBe(true);
    expect(isSensitive("restricted")).toBe(true);
    expect(isSensitive(undefined)).toBe(false);
  });
});

describe("normalizeField", () => {
  it("trims strings, drops blanks to undefined, and prunes blank allowed values", () => {
    const n = normalizeField(
      field({
        columnId: "c1",
        displayName: "  Email  ",
        description: "   ",
        role: "identifier",
        allowedValues: [" a ", "", "b"],
        owner: "",
      }),
    );
    expect(n.columnId).toBe("c1");
    expect(n.displayName).toBe("Email");
    expect(n.description).toBeUndefined();
    expect(n.role).toBe("identifier");
    expect(n.allowedValues).toEqual(["a", "b"]);
    expect(n.owner).toBeUndefined();
  });
});

describe("conflict reduction", () => {
  const conflicts = [conflict("c0", "description"), conflict("c1", "owner")];

  it("tracks unresolved conflicts and resolution completeness", () => {
    expect(unresolvedCount(conflicts, {})).toBe(2);
    expect(allConflictsResolved(conflicts, {})).toBe(false);

    const partial = { [conflictKey("c0", "description")]: "takeIncoming" as const };
    expect(unresolvedCount(conflicts, partial)).toBe(1);
    expect(allConflictsResolved(conflicts, partial)).toBe(false);

    const all = bulkChoices(conflicts, "keepExisting");
    expect(unresolvedCount(conflicts, all)).toBe(0);
    expect(allConflictsResolved(conflicts, all)).toBe(true);
  });

  it("builds a per-field resolution in plan order, omitting unresolved conflicts", () => {
    const choices = {
      [conflictKey("c1", "owner")]: "takeIncoming" as const,
      [conflictKey("c0", "description")]: "keepExisting" as const,
      // a stale key for a conflict not in the plan is ignored
      [conflictKey("gone", "unit")]: "takeIncoming" as const,
    };
    const res = buildPerFieldResolution(conflicts, choices);
    expect(res).toEqual({
      type: "perField",
      resolutions: [
        { columnId: "c0", field: "description", choice: "keepExisting" },
        { columnId: "c1", field: "owner", choice: "takeIncoming" },
      ],
    });
  });

  it("emits only the resolved subset so the backend still rejects a gap", () => {
    const choices = { [conflictKey("c0", "description")]: "takeIncoming" as const };
    const res = buildPerFieldResolution(conflicts, choices);
    // Only one of the two conflicts is present → the apply command will fail,
    // which is the guarantee (conflicts are never silently dropped).
    expect(res.type).toBe("perField");
    if (res.type === "perField") {
      expect(res.resolutions).toHaveLength(1);
      expect(res.resolutions[0].columnId).toBe("c0");
    }
  });
});
