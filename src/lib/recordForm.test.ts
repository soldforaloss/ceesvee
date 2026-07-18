import { describe, expect, it } from "vitest";

import {
  addGroup,
  assignToGroup,
  changedFields,
  clampRecord,
  fieldChanged,
  isDraftDirty,
  layoutSections,
  parseGoto,
  removeGroup,
  saveBlocked,
  stepRecord,
  toggleHidden,
  type RecordDraft,
} from "./recordForm";
import type { DraftValidation, RecordField, RecordLayout } from "../types";

/** A minimal RecordField for the reducer/layout tests. */
function field(col: number, columnId: string, raw: string): RecordField {
  return { col, columnId, header: columnId, raw, display: raw, class: "valid", valid: true };
}

const FIELDS: RecordField[] = [field(0, "c0", "1"), field(1, "c1", "N/A"), field(2, "c2", "hello")];

// ----- draft reducer ---------------------------------------------------------

describe("draft reducer", () => {
  it("marks a field changed only when the draft differs from the stored value", () => {
    const draft: RecordDraft = { 0: "2", 1: "N/A" };
    // col 0 drafted to a new value → changed.
    expect(fieldChanged(FIELDS[0], draft)).toBe(true);
    // col 1 drafted back to its stored token → NOT a change.
    expect(fieldChanged(FIELDS[1], draft)).toBe(false);
    // col 2 has no draft entry → not changed.
    expect(fieldChanged(FIELDS[2], draft)).toBe(false);
  });

  it("distinguishes a null token from an empty string as distinct drafts", () => {
    // Stored is "N/A"; drafting "" (blank) is a real change, drafting "N/A" is not.
    expect(fieldChanged(FIELDS[1], { 1: "" })).toBe(true);
    expect(fieldChanged(FIELDS[1], { 1: "N/A" })).toBe(false);
  });

  it("collects only changed fields as the pre-check payload", () => {
    const draft: RecordDraft = { 0: "2", 1: "N/A", 2: "world" };
    expect(changedFields(FIELDS, draft)).toEqual([
      { col: 0, value: "2" },
      { col: 2, value: "world" },
    ]);
    expect(isDraftDirty(FIELDS, draft)).toBe(true);
    expect(isDraftDirty(FIELDS, { 1: "N/A" })).toBe(false);
  });

  it("yields no commit payload when nothing changed (caller must not save)", () => {
    // The commit sends only the changed fields (display-row → absolute-row
    // remap now happens server-side under the revision guard); a clean draft
    // produces an empty payload the caller uses to skip the save.
    expect(changedFields(FIELDS, {})).toEqual([]);
    expect(changedFields(FIELDS, { 1: "N/A" })).toEqual([]); // reverted-to-stored
  });
});

// ----- visible-record navigation mapping ------------------------------------

describe("visible-record navigation", () => {
  it("clamps a target into the visible range and rejects an empty document", () => {
    expect(clampRecord(-3, 10)).toBe(0);
    expect(clampRecord(5, 10)).toBe(5);
    expect(clampRecord(99, 10)).toBe(9);
    expect(clampRecord(0, 0)).toBeNull();
  });

  it("steps to the next/prev visible record, returning null at the edges", () => {
    expect(stepRecord(0, 1, 10)).toBe(1);
    expect(stepRecord(9, 1, 10)).toBeNull(); // already at last
    expect(stepRecord(0, -1, 10)).toBeNull(); // already at first
    expect(stepRecord(5, -1, 10)).toBe(4);
    expect(stepRecord(0, 1, 1)).toBeNull(); // single record
  });

  it("parses a 1-based go-to input into a 0-based clamped row", () => {
    expect(parseGoto("1", 10)).toBe(0);
    expect(parseGoto("10", 10)).toBe(9);
    expect(parseGoto("999", 10)).toBe(9); // clamped
    expect(parseGoto("0", 10)).toBe(0); // clamped up
    expect(parseGoto("", 10)).toBeNull();
    expect(parseGoto("abc", 10)).toBeNull();
    expect(parseGoto("3.5", 10)).toBeNull();
    expect(parseGoto("3", 0)).toBeNull(); // no records
  });
});

// ----- validation gating -----------------------------------------------------

describe("validation gating", () => {
  const strict: DraftValidation = {
    fields: [{ col: 0, valid: false, mode: "strict", blocks: true }],
    strictBlocks: true,
    advisoryWarnings: 0,
    revision: 1,
  };
  const advisory: DraftValidation = {
    fields: [{ col: 1, valid: false, mode: "advisory", blocks: false }],
    strictBlocks: false,
    advisoryWarnings: 1,
    revision: 1,
  };

  it("blocks a save iff a strict column is invalid", () => {
    expect(saveBlocked(strict)).toBe(true);
    expect(saveBlocked(advisory)).toBe(false); // advisory only warns
    expect(saveBlocked(null)).toBe(false); // nothing validated yet
  });
});

// ----- layout sectioning -----------------------------------------------------

describe("layout sections", () => {
  it("returns a single default section in schema order for a null layout", () => {
    const sections = layoutSections(FIELDS, null);
    expect(sections).toHaveLength(1);
    expect(sections[0].group).toBeNull();
    expect(sections[0].fields.map((f) => f.columnId)).toEqual(["c0", "c1", "c2"]);
  });

  it("places grouped fields in their group and the rest in a default section", () => {
    const layout: RecordLayout = {
      density: "compact",
      hiddenColumnIds: [],
      groups: [{ id: "g1", name: "Ids", columnIds: ["c2", "c0"] }],
    };
    const sections = layoutSections(FIELDS, layout);
    expect(sections).toHaveLength(2);
    // Group keeps its declared column order (c2 before c0).
    expect(sections[0].group?.name).toBe("Ids");
    expect(sections[0].fields.map((f) => f.columnId)).toEqual(["c2", "c0"]);
    // Ungrouped remainder in schema order.
    expect(sections[1].group).toBeNull();
    expect(sections[1].fields.map((f) => f.columnId)).toEqual(["c1"]);
  });

  it("omits hidden fields from every section", () => {
    const layout: RecordLayout = {
      density: "comfortable",
      hiddenColumnIds: ["c1"],
      groups: [],
    };
    const sections = layoutSections(FIELDS, layout);
    const shown = sections.flatMap((s) => s.fields.map((f) => f.columnId));
    expect(shown).toEqual(["c0", "c2"]);
  });

  it("never drops or duplicates a field when a persisted layout has drifted", () => {
    // A stale group references a column ID that no longer exists ("gone") and
    // the same field twice; a hidden ID that no longer exists is harmless.
    const layout: RecordLayout = {
      density: "comfortable",
      hiddenColumnIds: ["ghost"],
      groups: [{ id: "g1", name: "Stale", columnIds: ["gone", "c0", "c0"] }],
    };
    const sections = layoutSections(FIELDS, layout);
    const shown = sections.flatMap((s) => s.fields.map((f) => f.columnId)).sort();
    // Every real, non-hidden field appears exactly once, total.
    expect(shown).toEqual(["c0", "c1", "c2"]);
  });
});

// ----- layout mutation helpers ----------------------------------------------

describe("layout mutations", () => {
  it("toggles a field's hidden state, seeding a layout from null", () => {
    const a = toggleHidden(null, "c1");
    expect(a.hiddenColumnIds).toEqual(["c1"]);
    const b = toggleHidden(a, "c1");
    expect(b.hiddenColumnIds).toEqual([]);
  });

  it("moves a field between groups without leaving it in two places", () => {
    let layout = addGroup(null, "g1", "A");
    layout = addGroup(layout, "g2", "B");
    layout = assignToGroup(layout, "c0", "g1");
    expect(layout.groups[0].columnIds).toEqual(["c0"]);
    // Reassigning to g2 removes it from g1 first.
    layout = assignToGroup(layout, "c0", "g2");
    expect(layout.groups[0].columnIds).toEqual([]);
    expect(layout.groups[1].columnIds).toEqual(["c0"]);
    // Assigning to null (default section) removes it from all groups.
    layout = assignToGroup(layout, "c0", null);
    expect(layout.groups.every((g) => g.columnIds.length === 0)).toBe(true);
  });

  it("removes a group, dropping its assignments back to the default section", () => {
    let layout = addGroup(null, "g1", "A");
    layout = assignToGroup(layout, "c0", "g1");
    layout = removeGroup(layout, "g1");
    expect(layout.groups).toHaveLength(0);
    // c0 is no longer claimed by any group → default section.
    expect(layoutSections(FIELDS, layout)[0].fields.map((f) => f.columnId)).toEqual([
      "c0",
      "c1",
      "c2",
    ]);
  });
});
