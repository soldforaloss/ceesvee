import { describe, expect, it } from "vitest";

import type { ColumnSemantics, SemanticType } from "../types";
import { actionsForType, applyOverrides, isFilterable, upsertOverride } from "./semantics";

const col = (column: number, detected: SemanticType | null): ColumnSemantics => ({
  column,
  detected,
  bestCandidate: detected,
  confidence: detected ? 1 : 0,
  matching: detected ? 10 : 0,
  conflicting: 0,
  nonBlank: 10,
});

describe("applyOverrides", () => {
  const headers = ["email", "notes", "id"];
  const columns = [col(0, "email"), col(1, null), col(2, "uuid")];

  it("keeps detection when no override matches", () => {
    const out = applyOverrides(columns, headers, []);
    expect(out.map((c) => c.effective)).toEqual(["email", null, "uuid"]);
    expect(out.every((c) => !c.overridden)).toBe(true);
  });

  it("override wins over detection, keyed by column name", () => {
    const out = applyOverrides(columns, headers, [["notes", "categorical"]]);
    expect(out[1].effective).toBe("categorical");
    expect(out[1].overridden).toBe(true);
    expect(out[0].effective).toBe("email");
  });

  it("freeText forces a detected column back to plain text", () => {
    const out = applyOverrides(columns, headers, [["id", "freeText"]]);
    expect(out[2].effective).toBe("freeText");
    expect(out[2].overridden).toBe(true);
  });

  it("overrides survive column reordering because they key on the name", () => {
    const reordered = ["id", "email", "notes"];
    const cols2 = [col(0, "uuid"), col(1, "email"), col(2, null)];
    const out = applyOverrides(cols2, reordered, [["id", "freeText"]]);
    expect(out[0].effective).toBe("freeText");
    expect(out[1].effective).toBe("email");
  });

  it("blank header names never match an override", () => {
    const out = applyOverrides([col(0, null)], [""], [["", "email"]]);
    expect(out[0].effective).toBeNull();
    expect(out[0].overridden).toBe(false);
  });
});

describe("upsertOverride", () => {
  it("inserts, replaces, and removes", () => {
    let list = upsertOverride([], "email", "email");
    expect(list).toEqual([["email", "email"]]);
    list = upsertOverride(list, "email", "freeText");
    expect(list).toEqual([["email", "freeText"]]);
    list = upsertOverride(list, "notes", "categorical");
    expect(list).toHaveLength(2);
    list = upsertOverride(list, "email", null);
    expect(list).toEqual([["notes", "categorical"]]);
  });
});

describe("action catalogue", () => {
  it("phone and postal columns offer NO mutating actions", () => {
    expect(actionsForType("phoneNumber")).toEqual([]);
    expect(actionsForType("postalCode")).toEqual([]);
  });

  it("per-type actions match the closed backend set", () => {
    expect(actionsForType("email")).toEqual(["normalize", "extractEmailDomain"]);
    expect(actionsForType("url")).toEqual(["extractUrlHost"]);
    expect(actionsForType("percentage")).toEqual(["percentToDecimal"]);
    expect(actionsForType("uuid")).toEqual(["normalize"]);
  });

  it("valid/invalid filters exist only for per-cell pattern types", () => {
    expect(isFilterable("email")).toBe(true);
    expect(isFilterable("categorical")).toBe(false);
    expect(isFilterable("freeText")).toBe(false);
  });
});
