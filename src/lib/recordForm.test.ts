import { describe, expect, it } from "vitest";

import {
  addGroup,
  assignToGroup,
  changedFields,
  clampRecord,
  fieldCellNotes,
  fieldChanged,
  isDraftDirty,
  layoutSections,
  parseGoto,
  recordViewCurrent,
  recordViewToken,
  removeGroup,
  saveBlocked,
  stepRecord,
  toggleHidden,
  type RecordDraft,
} from "./recordForm";
import type {
  AnnotationNote,
  DraftValidation,
  RecordField,
  RecordLayout,
  RowAnnotationView,
} from "../types";

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

// ----- stale-record guard (no wrong-row edit window) -------------------------

describe("stale-record guard", () => {
  // The form loads a record via an async fetch, so after the user moves the
  // target (navigation, or a switch to another document/tab) the previously
  // loaded view lingers until the new fetch resolves. `recordViewCurrent`
  // compares the token the loaded view answered to against the live target;
  // while it is false the form shows "Loading record…" and `commit` refuses to
  // write, so an edit+save can never land on the record the form has left.
  // These assertions model that window as a sequence — the "loading state"
  // (form not current) is exactly `recordViewCurrent(...) === false`.

  it("stays stale from a navigation until the matching fetch resolves", () => {
    const doc = 1;
    // Showing record 0: the loaded view answers doc 0, target is doc 0.
    let loaded = recordViewToken(doc, 0);
    expect(recordViewCurrent(loaded, recordViewToken(doc, 0))).toBe(true);

    // User navigates to record 1. The target moves immediately, but the async
    // fetch for row 1 has NOT resolved — the loaded view still answers row 0,
    // so the form is NOT current: it shows loading and blocks the commit that
    // would otherwise write the draft onto row 0 (the P1 bug).
    expect(recordViewCurrent(loaded, recordViewToken(doc, 1))).toBe(false);

    // The fetch for row 1 resolves and stamps its view → current again.
    loaded = recordViewToken(doc, 1);
    expect(recordViewCurrent(loaded, recordViewToken(doc, 1))).toBe(true);
  });

  it("treats a coincidental row match on another document/tab as stale", () => {
    // The previous tab's view answered row 5 of docA; the form now points at
    // row 5 of docB. The row indices (and, in the wild, the revisions) coincide,
    // but keying the token on the DOCUMENT — not the revision — defeats the
    // "revisions happen to match" trap the reviewer flagged: still not current.
    const loaded = recordViewToken(1, 5);
    expect(recordViewCurrent(loaded, recordViewToken(2, 5))).toBe(false);
    // Only once document 2's own fetch resolves does the form become current.
    expect(recordViewCurrent(recordViewToken(2, 5), recordViewToken(2, 5))).toBe(true);
  });

  it("is not current before the first fetch resolves or with no active document", () => {
    // No view loaded yet (first open) → loading, never a stale-editable form.
    expect(recordViewCurrent(null, recordViewToken(1, 0))).toBe(false);
    // No active document (form target null) → not current.
    expect(recordViewCurrent(recordViewToken(1, 0), null)).toBe(false);
    expect(recordViewCurrent(null, null)).toBe(false);
  });

  it("builds an unambiguous token that never conflates two documents/rows", () => {
    // Distinct (doc, row) pairs must never collide, or a stale view could look
    // current. Neighbouring rows and lookalike ids stay distinct.
    expect(recordViewToken(7, 1)).not.toBe(recordViewToken(7, 2));
    expect(recordViewToken(1, 0)).not.toBe(recordViewToken(2, 0));
    expect(recordViewToken(7, 3)).toBe(recordViewToken(7, 3));
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

// ----- field cell-note mapping (F40 annotation reuse) ------------------------

describe("field cell-note mapping", () => {
  /** A dated note; timestamps are irrelevant to the mapping. */
  function note(text: string): AnnotationNote {
    return { text, createdMs: 1, updatedMs: 1 };
  }
  /** A matched annotation entry carrying the given cell notes. */
  function entry(cellNotes: RowAnnotationView["cellNotes"]): RowAnnotationView {
    return {
      handle: 1,
      status: "matched",
      record: 4,
      anchorKind: "record",
      star: false,
      flag: false,
      cellNotes,
      createdMs: 1,
      updatedMs: 1,
    };
  }

  it("maps each cell note to its column id → text, keeping presence checkable", () => {
    const map = fieldCellNotes(
      entry([
        { columnId: "c0", note: note("check this") },
        { columnId: "c2", note: note("verified") },
      ]),
    );
    // Presence (the field's note indicator) and text (the editor prefill).
    expect(map.has("c0")).toBe(true);
    expect(map.get("c0")).toBe("check this");
    expect(map.get("c2")).toBe("verified");
    // A field with no cell note is absent → no indicator.
    expect(map.has("c1")).toBe(false);
    expect(map.get("c1")).toBeUndefined();
    expect(map.size).toBe(2);
  });

  it("yields an empty map for an unannotated, note-less, or null entry", () => {
    expect(fieldCellNotes(undefined).size).toBe(0);
    expect(fieldCellNotes(null).size).toBe(0);
    // A row starred/flagged but with no cell notes maps nothing.
    expect(fieldCellNotes(entry(undefined)).size).toBe(0);
    expect(fieldCellNotes(entry([])).size).toBe(0);
  });
});
