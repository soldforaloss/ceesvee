import { describe, expect, it } from "vitest";

import {
  annotationExportName,
  buildRecordIndex,
  cellNoteColumns,
  entryHasAnnotation,
  entryMatchesQuery,
  entryPassesKind,
  gutterGlyphCount,
  matchStatusLabel,
  normalizeTagName,
  noteTimeLabel,
  predicateForKind,
  predicateLabel,
  predicateMatches,
  sortEntries,
  tagColor,
} from "./annotations";
import type { AnnotationsView, RowAnnotationView } from "../types";

const note = (text: string, author?: string) => ({
  text,
  author,
  createdMs: 1_000,
  updatedMs: 2_000,
});

const entry = (over: Partial<RowAnnotationView>): RowAnnotationView => ({
  handle: 1,
  status: "matched",
  record: 0,
  anchorKind: "record",
  star: false,
  flag: false,
  createdMs: 1_000,
  updatedMs: 2_000,
  ...over,
});

const view = (entries: RowAnnotationView[]): AnnotationsView => ({
  annotationsRevision: 3,
  revision: 5,
  tags: [],
  matched: entries.filter((e) => e.status === "matched").length,
  ambiguous: entries.filter((e) => e.status === "ambiguous").length,
  orphaned: entries.filter((e) => e.status === "orphaned").length,
  entries,
});

describe("buildRecordIndex", () => {
  it("indexes matched entries by record and skips uncertain ones", () => {
    const e0 = entry({ handle: 1, record: 4, star: true });
    const ambiguous = entry({ handle: 2, status: "ambiguous", record: undefined });
    const orphaned = entry({ handle: 3, status: "orphaned", record: undefined });
    const idx = buildRecordIndex(view([e0, ambiguous, orphaned]));
    expect(idx.size).toBe(1);
    expect(idx.get(4)).toBe(e0);
  });

  it("returns an empty map for a null view", () => {
    expect(buildRecordIndex(null).size).toBe(0);
  });
});

describe("cellNoteColumns", () => {
  it("collects the column ids carrying a cell note", () => {
    const e = entry({
      cellNotes: [
        { columnId: "c-a", note: note("x") },
        { columnId: "c-b", note: note("y") },
      ],
    });
    expect([...cellNoteColumns(e)].sort()).toEqual(["c-a", "c-b"]);
  });

  it("is empty when there are no cell notes", () => {
    expect(cellNoteColumns(entry({})).size).toBe(0);
  });
});

describe("entryHasAnnotation / gutterGlyphCount", () => {
  it("detects each annotation kind", () => {
    expect(entryHasAnnotation(entry({}))).toBe(false);
    expect(entryHasAnnotation(entry({ star: true }))).toBe(true);
    expect(entryHasAnnotation(entry({ flag: true }))).toBe(true);
    expect(entryHasAnnotation(entry({ tags: ["t"] }))).toBe(true);
    expect(entryHasAnnotation(entry({ note: note("n") }))).toBe(true);
    expect(entryHasAnnotation(entry({ cellNotes: [{ columnId: "c", note: note("n") }] }))).toBe(
      true,
    );
  });

  it("counts up to three gutter glyph slots", () => {
    expect(gutterGlyphCount(entry({}))).toBe(0);
    expect(gutterGlyphCount(entry({ star: true, flag: true, note: note("n") }))).toBe(3);
    // tags collapse into the note/tag slot
    expect(gutterGlyphCount(entry({ tags: ["a", "b"] }))).toBe(1);
  });
});

describe("predicates", () => {
  it("maps kinds to predicates (and null for all/review)", () => {
    expect(predicateForKind("starred")).toEqual({ type: "starred" });
    expect(predicateForKind("tagged", "urgent")).toEqual({ type: "tagged", tag: "urgent" });
    expect(predicateForKind("all")).toBeNull();
    expect(predicateForKind("review")).toBeNull();
  });

  it("labels predicates", () => {
    expect(predicateLabel({ type: "starred" })).toMatch(/star/i);
    expect(predicateLabel({ type: "tagged", tag: "x" })).toContain("x");
    expect(predicateLabel({ type: "tagged" })).toMatch(/tagged/i);
    expect(predicateLabel({ type: "anyAnnotation" })).toMatch(/annotated/i);
  });

  it("mirrors backend matching semantics", () => {
    const starred = entry({ star: true });
    expect(predicateMatches(starred, { type: "starred" })).toBe(true);
    expect(predicateMatches(starred, { type: "flagged" })).toBe(false);

    const tagged = entry({ tags: ["a", "b"] });
    expect(predicateMatches(tagged, { type: "tagged" })).toBe(true);
    expect(predicateMatches(tagged, { type: "tagged", tag: "a" })).toBe(true);
    expect(predicateMatches(tagged, { type: "tagged", tag: "z" })).toBe(false);

    const celled = entry({ cellNotes: [{ columnId: "c", note: note("n") }] });
    expect(predicateMatches(celled, { type: "hasCellNote" })).toBe(true);
    expect(predicateMatches(celled, { type: "hasNote" })).toBe(false);
    expect(predicateMatches(celled, { type: "anyAnnotation" })).toBe(true);
  });
});

describe("entryPassesKind", () => {
  it("passes everything for 'all' and only non-matched for 'review'", () => {
    const matched = entry({ status: "matched", star: true });
    const orphaned = entry({ status: "orphaned", record: undefined, note: note("n") });
    expect(entryPassesKind(matched, "all")).toBe(true);
    expect(entryPassesKind(orphaned, "all")).toBe(true);
    expect(entryPassesKind(matched, "review")).toBe(false);
    expect(entryPassesKind(orphaned, "review")).toBe(true);
    expect(entryPassesKind(matched, "starred")).toBe(true);
    expect(entryPassesKind(orphaned, "starred")).toBe(false);
  });
});

describe("entryMatchesQuery", () => {
  it("searches tags, row note text/author and cell note text", () => {
    const e = entry({
      tags: ["Urgent"],
      note: note("follow up with finance", "amy"),
      cellNotes: [{ columnId: "c", note: note("check the SKU") }],
    });
    expect(entryMatchesQuery(e, "")).toBe(true);
    expect(entryMatchesQuery(e, "urgent")).toBe(true);
    expect(entryMatchesQuery(e, "FINANCE")).toBe(true);
    expect(entryMatchesQuery(e, "amy")).toBe(true);
    expect(entryMatchesQuery(e, "sku")).toBe(true);
    expect(entryMatchesQuery(e, "nope")).toBe(false);
  });
});

describe("sortEntries", () => {
  it("orders matched by record, then review items by handle", () => {
    const a = entry({ handle: 10, record: 7 });
    const b = entry({ handle: 11, record: 2 });
    const o1 = entry({ handle: 30, status: "orphaned", record: undefined });
    const o2 = entry({ handle: 20, status: "orphaned", record: undefined });
    const sorted = sortEntries([o1, a, o2, b]);
    expect(sorted.map((e) => e.handle)).toEqual([11, 10, 20, 30]);
  });
});

describe("misc helpers", () => {
  it("normalizes tag names", () => {
    expect(normalizeTagName("  hi ")).toBe("hi");
    expect(normalizeTagName("   ")).toBeNull();
  });

  it("builds export file names", () => {
    expect(annotationExportName("orders.csv", "json")).toBe("orders.annotations.json");
    expect(annotationExportName("orders.tsv", "csv")).toBe("orders.annotations.csv");
    expect(annotationExportName("", "json")).toBe("annotations.annotations.json");
  });

  it("labels match statuses", () => {
    expect(matchStatusLabel("matched")).toBe("Matched");
    expect(matchStatusLabel("ambiguous")).toBe("Ambiguous");
    expect(matchStatusLabel("orphaned")).toBe("Orphaned");
  });

  it("uses a declared tag colour but is otherwise stable per name", () => {
    expect(tagColor("x", "#123456")).toBe("#123456");
    expect(tagColor("x", "  ")).toBe(tagColor("x"));
    expect(tagColor("repeatable")).toBe(tagColor("repeatable"));
  });

  it("formats a timestamp and tolerates zero", () => {
    expect(noteTimeLabel(0)).toBe("");
    expect(noteTimeLabel(1_700_000_000_000).length).toBeGreaterThan(0);
  });
});
