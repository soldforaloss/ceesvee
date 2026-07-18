import { describe, expect, it } from "vitest";

import type {
  AnnotationsExport,
  DocumentMeta,
  NamedView,
  PlanEntry,
  ProjectSource,
  SourcePreviewEntry,
  SourceStatus,
} from "../types";
import {
  annotationsExportIsEmpty,
  availableOnlyChoices,
  buildAnnotationsSection,
  buildLayoutSection,
  buildResolutions,
  buildSources,
  buildTabsSection,
  buildViewsSection,
  canApply,
  canOpenInPlace,
  defaultChoice,
  deriveProjectDirty,
  gatingWarnings,
  hasBlockingSources,
  mintSourceId,
  orderTabsForPlan,
  panelsFromLayout,
  pathKey,
  projectSnapshot,
  projectSnapshotsEqual,
  restoreOpenRoute,
  statusDisplay,
  type PanelLayout,
  type SourceAnnotationsSection,
  type SourceViewsSection,
} from "./project";

function nv(id: string): NamedView {
  return {
    id,
    name: id,
    filter: null,
    filterColumnIds: [],
    sortKeys: [],
    hiddenColumnIds: [],
    pinnedColumnIds: [],
    columnOrder: [],
    columnWidths: {},
    wrapText: false,
  };
}

function meta(partial: Partial<DocumentMeta> & Pick<DocumentMeta, "id">): DocumentMeta {
  return {
    path: `C:\\data\\${partial.id}.csv`,
    fileName: `${partial.id}.csv`,
    rowCount: 3,
    totalRowCount: 3,
    filtered: false,
    colCount: 2,
    headers: ["id", "amount"],
    columnIds: ["c0", "c1"],
    viewSorted: false,
    hasHeaderRow: true,
    delimiter: ",",
    encoding: "UTF-8",
    hadBom: false,
    lineEnding: "lf",
    dirty: false,
    canUndo: false,
    canRedo: false,
    revision: 1,
    backing: "editable",
    archive: null,
    ...partial,
  };
}

function entry(
  partial: Partial<SourcePreviewEntry> & Pick<SourcePreviewEntry, "sourceId" | "status">,
): SourcePreviewEntry {
  return {
    sourceId: partial.sourceId,
    displayName: partial.displayName ?? `${partial.sourceId}.csv`,
    resolvedPath: partial.resolvedPath ?? `C:\\data\\${partial.sourceId}.csv`,
    status: partial.status,
    storedFingerprint: null,
    diskFingerprint: null,
    movedCandidate: partial.movedCandidate ?? null,
    reapplyViews: partial.reapplyViews ?? partial.status === "ok",
    warnings: partial.warnings ?? [],
  };
}

const layout: PanelLayout = { diagnostics: false, explorer: false, changes: false };

describe("pathKey", () => {
  it("normalises separators and case for identity comparison", () => {
    expect(pathKey("C:\\data\\A.csv")).toBe(pathKey("c:/data/a.csv"));
    expect(pathKey("C:/data/a.csv/")).toBe("c:/data/a.csv");
    expect(pathKey("C:\\data\\a.csv")).not.toBe(pathKey("C:\\data\\b.csv"));
  });
});

describe("restoreOpenRoute", () => {
  it("routes columnar sources to a non-interactive indexed open", () => {
    expect(restoreOpenRoute("C:\\data\\sales.parquet")).toBe("columnarIndexed");
    expect(restoreOpenRoute("/home/u/events.arrow")).toBe("columnarIndexed");
    expect(restoreOpenRoute("C:/data/EVENTS.FEATHER")).toBe("columnarIndexed");
    expect(restoreOpenRoute("C:/data/stream.arrows")).toBe("columnarIndexed");
    expect(restoreOpenRoute("C:/data/legacy.ipc")).toBe("columnarIndexed");
  });

  it("routes every other source through the ordinary open pipeline", () => {
    expect(restoreOpenRoute("C:\\data\\a.csv")).toBe("standard");
    expect(restoreOpenRoute("C:\\data\\a.json")).toBe("standard");
    expect(restoreOpenRoute("C:\\data\\a.jsonl")).toBe("standard");
    expect(restoreOpenRoute("C:\\data\\a.tsv")).toBe("standard");
    expect(restoreOpenRoute("C:\\data\\archive.zip")).toBe("standard");
    expect(restoreOpenRoute("C:\\data\\log.txt.gz")).toBe("standard");
  });
});

describe("statusDisplay", () => {
  const cases: [SourceStatus, "ok" | "warn" | "error"][] = [
    ["ok", "ok"],
    ["missing", "error"],
    ["movedCandidate", "warn"],
    ["changedFingerprint", "warn"],
    ["schemaIncompatible", "warn"],
  ];
  it("maps every status to a label, tone and hint", () => {
    for (const [status, tone] of cases) {
      const d = statusDisplay(status);
      expect(d.tone).toBe(tone);
      expect(d.label.length).toBeGreaterThan(0);
      expect(d.hint.length).toBeGreaterThan(0);
    }
  });
});

describe("canOpenInPlace", () => {
  it("is true only when the file is present at its stored path", () => {
    expect(canOpenInPlace("ok")).toBe(true);
    expect(canOpenInPlace("changedFingerprint")).toBe(true);
    expect(canOpenInPlace("schemaIncompatible")).toBe(true);
    expect(canOpenInPlace("missing")).toBe(false);
    expect(canOpenInPlace("movedCandidate")).toBe(false);
  });
});

describe("defaultChoice", () => {
  it("opens present files", () => {
    expect(defaultChoice(entry({ sourceId: "a", status: "ok" }))).toEqual({ action: "open" });
    expect(defaultChoice(entry({ sourceId: "a", status: "changedFingerprint" }))).toEqual({
      action: "open",
    });
  });

  it("relinks a moved file to its found candidate", () => {
    const e = entry({ sourceId: "a", status: "movedCandidate", movedCandidate: "C:\\new\\a.csv" });
    expect(defaultChoice(e)).toEqual({ action: "locate", locatePath: "C:\\new\\a.csv" });
  });

  it("leaves a missing file out by default (never blocks the rest)", () => {
    expect(defaultChoice(entry({ sourceId: "a", status: "missing" }))).toEqual({ action: "skip" });
  });

  it("skips a moved file that has no candidate", () => {
    expect(defaultChoice(entry({ sourceId: "a", status: "movedCandidate" }))).toEqual({
      action: "skip",
    });
  });
});

describe("buildResolutions / canApply", () => {
  const entries = [
    entry({ sourceId: "a", status: "ok" }),
    entry({ sourceId: "b", status: "missing" }),
    entry({ sourceId: "c", status: "movedCandidate", movedCandidate: "C:\\new\\c.csv" }),
  ];

  it("fills unset choices with their defaults", () => {
    const res = buildResolutions(entries, {});
    expect(res).toEqual([
      { sourceId: "a", action: "open" },
      { sourceId: "b", action: "skip" },
      { sourceId: "c", action: "locate", path: "C:\\new\\c.csv" },
    ]);
  });

  it("honours explicit per-source choices", () => {
    const res = buildResolutions(entries, {
      a: { action: "remove" },
      b: { action: "locate", locatePath: "C:\\else\\b.csv" },
    });
    expect(res[0]).toEqual({ sourceId: "a", action: "remove" });
    expect(res[1]).toEqual({ sourceId: "b", action: "locate", path: "C:\\else\\b.csv" });
  });

  it("blocks apply when a locate choice has no path", () => {
    expect(canApply(entries, { b: { action: "locate" } })).toBe(false);
    expect(canApply(entries, { b: { action: "skip" } })).toBe(true);
  });

  it("blocks apply when open is chosen for an absent file", () => {
    expect(canApply(entries, { b: { action: "open" } })).toBe(false);
  });

  it("accepts the all-default resolution set", () => {
    expect(canApply(entries, {})).toBe(true);
  });
});

describe("availableOnlyChoices / hasBlockingSources", () => {
  const entries = [
    entry({ sourceId: "a", status: "ok" }),
    entry({ sourceId: "b", status: "missing" }),
    entry({ sourceId: "c", status: "changedFingerprint" }),
  ];

  it("opens present files and skips the rest", () => {
    expect(availableOnlyChoices(entries)).toEqual({
      a: { action: "open" },
      b: { action: "skip" },
      c: { action: "open" },
    });
  });

  it("detects when a decision is required", () => {
    expect(hasBlockingSources(entries)).toBe(true);
    expect(hasBlockingSources([entry({ sourceId: "a", status: "ok" })])).toBe(false);
  });
});

describe("gatingWarnings", () => {
  it("lists sources whose views were not reapplied", () => {
    const entries: PlanEntry[] = [
      planEntry("a", true, []),
      planEntry("b", false, ["columns changed"]),
      planEntry("c", false, []),
    ];
    const warns = gatingWarnings(entries);
    expect(warns.map((w) => w.sourceId)).toEqual(["b"]);
    expect(warns[0].warnings).toEqual(["columns changed"]);
  });
});

function planEntry(sourceId: string, reapplyViews: boolean, viewWarnings: string[]): PlanEntry {
  return {
    sourceId,
    path: `C:\\data\\${sourceId}.csv`,
    displayName: `${sourceId}.csv`,
    open: {},
    status: "ok",
    reapplyViews,
    viewWarnings,
    views: [],
    activeViewId: null,
  };
}

describe("mintSourceId", () => {
  it("mints collision-free ids", () => {
    const used = new Set<string>(["src-1"]);
    expect(mintSourceId(used)).toBe("src-2");
    expect(mintSourceId(used)).toBe("src-3");
    expect(used.has("src-2")).toBe(true);
  });
});

describe("buildSources", () => {
  it("reuses ids by path and mints ids for new documents, no cell data", () => {
    const existing: ProjectSource[] = [
      { id: "src-1", path: "C:\\data\\1.csv", fingerprint: { size: 10, modifiedAtMs: 5 } },
    ];
    const tabs = [
      meta({ id: 1, path: "C:\\data\\1.csv" }),
      meta({ id: 2, path: "C:/data/2.csv", headers: ["x", "y"], columnIds: ["c0", "c1"] }),
      meta({ id: 3, path: null, fileName: "untitled" }),
    ];
    const sources = buildSources(tabs, existing, { 1: { size: 11, modifiedAtMs: 9 }, 2: null });
    // Unsaved buffer (id 3) is not referenceable.
    expect(sources.map((s) => s.id)).toEqual(["src-1", "src-2"]);
    // Existing id reused by path (case/separator-insensitive), fresh fingerprint captured.
    expect(sources[0].fingerprint).toEqual({ size: 11, modifiedAtMs: 9 });
    // New source falls back to the prior fingerprint (null here) and mints an id.
    expect(sources[1].id).toBe("src-2");
    // Columns are id + header snapshots only — never cell values.
    expect(sources[1].columns).toEqual([
      { id: "c0", name: "x" },
      { id: "c1", name: "y" },
    ]);
    const json = JSON.stringify(sources);
    for (const key of ["cells", "rows", "records", "values", "cellValues"]) {
      expect(json.includes(`"${key}"`)).toBe(false);
    }
  });

  it("preserves an existing source whose file is not open (left out, not removed)", () => {
    const existing: ProjectSource[] = [
      { id: "src-1", path: "C:\\data\\open.csv" },
      { id: "src-2", path: "C:\\data\\leftout.csv", displayName: "leftout.csv" },
    ];
    const tabs = [meta({ id: 1, path: "C:\\data\\open.csv" })];
    const sources = buildSources(tabs, existing, { 1: { size: 3, modifiedAtMs: 1 } });
    // Both stay referenced; the closed one is untouched.
    expect(sources.map((s) => s.id)).toEqual(["src-1", "src-2"]);
    expect(sources[1]).toEqual(existing[1]);
    expect(sources[0].fingerprint).toEqual({ size: 3, modifiedAtMs: 1 });
  });
});

describe("buildTabsSection", () => {
  it("captures open order and active tab as source ids", () => {
    const sources: ProjectSource[] = [
      { id: "src-1", path: "C:\\data\\1.csv" },
      { id: "src-2", path: "C:\\data\\2.csv" },
    ];
    const tabs = [
      meta({ id: 2, path: "C:\\data\\2.csv" }),
      meta({ id: 1, path: "C:\\data\\1.csv" }),
    ];
    const section = buildTabsSection(sources, tabs, 1);
    expect(section.open).toEqual(["src-2", "src-1"]);
    expect(section.active).toBe("src-1");
  });

  it("has no active id when the active tab is an unsaved buffer", () => {
    const sources: ProjectSource[] = [{ id: "src-1", path: "C:\\data\\1.csv" }];
    const tabs = [meta({ id: 1, path: "C:\\data\\1.csv" }), meta({ id: 9, path: null })];
    const section = buildTabsSection(sources, tabs, 9);
    expect(section.open).toEqual(["src-1"]);
    expect(section.active).toBeNull();
  });
});

describe("buildViewsSection", () => {
  const sources: ProjectSource[] = [
    { id: "src-1", path: "C:\\data\\a.csv" },
    { id: "src-2", path: "C:\\data\\b.csv" },
  ];

  it("captures each open source's views + active view and preserves closed ones", () => {
    const tabs = [meta({ id: 1, path: "C:\\data\\a.csv" })]; // only a.csv is open
    const existing: SourceViewsSection[] = [
      { sourceId: "src-2", views: [nv("old")], activeViewId: "old" },
    ];
    const section = buildViewsSection(
      sources,
      tabs,
      existing,
      (tab) => (tab.id === 1 ? [nv("v1"), nv("v2")] : []),
      (tab) => (tab.id === 1 ? "v1" : null),
    );
    expect(section).toEqual([
      { sourceId: "src-1", views: [nv("v1"), nv("v2")], activeViewId: "v1" },
      // A referenced-but-closed source keeps its previously-saved views.
      { sourceId: "src-2", views: [nv("old")], activeViewId: "old" },
    ]);
  });

  it("omits an open source that has no views and no active view", () => {
    const tabs = [meta({ id: 1, path: "C:\\data\\a.csv" })];
    expect(
      buildViewsSection(
        sources,
        tabs,
        [],
        () => [],
        () => null,
      ),
    ).toEqual([]);
  });

  it("drops preserved views for a source the project no longer references", () => {
    const existing: SourceViewsSection[] = [
      { sourceId: "gone", views: [nv("x")], activeViewId: "x" },
    ];
    expect(
      buildViewsSection(
        sources,
        [],
        existing,
        () => [],
        () => null,
      ),
    ).toEqual([]);
  });

  it("captures no cell data", () => {
    const tabs = [meta({ id: 1, path: "C:\\data\\a.csv" })];
    const section = buildViewsSection(
      sources,
      tabs,
      [],
      () => [nv("v1")],
      () => "v1",
    );
    const json = JSON.stringify(section);
    for (const key of ["cells", "rows", "records", "values", "cellValues"]) {
      expect(json.includes(`"${key}"`)).toBe(false);
    }
  });
});

describe("buildAnnotationsSection (F40)", () => {
  const sources: ProjectSource[] = [
    { id: "src-1", path: "C:\\data\\a.csv" },
    { id: "src-2", path: "C:\\data\\b.csv" },
  ];
  const exp = (over: Partial<AnnotationsExport> = {}): AnnotationsExport => ({
    version: 1,
    entries: [{ handle: 0 }],
    ...over,
  });

  it("annotationsExportIsEmpty ignores version but honours content and config", () => {
    expect(annotationsExportIsEmpty({ version: 1 })).toBe(true);
    expect(annotationsExportIsEmpty({ version: 1, entries: [], tags: [] })).toBe(true);
    expect(annotationsExportIsEmpty(exp())).toBe(false);
    expect(annotationsExportIsEmpty({ version: 1, tags: [{ name: "keep" }] })).toBe(false);
    expect(annotationsExportIsEmpty({ version: 1, author: "Dana" })).toBe(false);
    expect(annotationsExportIsEmpty({ version: 1, keySpec: { columns: ["c0"] } })).toBe(false);
  });

  it("captures each open source's export and preserves closed ones", () => {
    const tabs = [meta({ id: 1, path: "C:\\data\\a.csv" })]; // only a.csv is open
    const existing: SourceAnnotationsSection[] = [
      { sourceId: "src-2", annotations: exp({ author: "prior" }) },
    ];
    const section = buildAnnotationsSection(sources, tabs, existing, (tab) =>
      tab.id === 1 ? exp({ author: "live" }) : null,
    );
    expect(section).toEqual([
      { sourceId: "src-1", annotations: exp({ author: "live" }) },
      // A referenced-but-closed source keeps its previously-saved annotations.
      { sourceId: "src-2", annotations: exp({ author: "prior" }) },
    ]);
  });

  it("omits an open source whose annotations are all cleared (no stale fallback)", () => {
    const tabs = [meta({ id: 1, path: "C:\\data\\a.csv" })];
    // The source WAS saved before, but its live store is now empty.
    const existing: SourceAnnotationsSection[] = [
      { sourceId: "src-1", annotations: exp({ author: "stale" }) },
    ];
    const section = buildAnnotationsSection(sources, tabs, existing, () => ({ version: 1 }));
    expect(section).toEqual([]);
  });

  it("drops preserved annotations for a source the project no longer references", () => {
    const existing: SourceAnnotationsSection[] = [{ sourceId: "gone", annotations: exp() }];
    expect(buildAnnotationsSection(sources, [], existing, () => null)).toEqual([]);
  });

  it("keys the section by source id, never by path (no on-disk locations leaked)", () => {
    const tabs = [meta({ id: 1, path: "C:\\data\\a.csv" })];
    const section = buildAnnotationsSection(sources, tabs, [], () => exp());
    expect(section.map((s) => s.sourceId)).toEqual(["src-1"]);
    expect(JSON.stringify(section).includes("C:\\\\data")).toBe(false);
  });
});

describe("layout section round-trip", () => {
  it("captures and reads back panel flags", () => {
    const panels: PanelLayout = { diagnostics: true, explorer: false, changes: true };
    const section = buildLayoutSection(panels);
    expect(panelsFromLayout(section)).toEqual(panels);
  });

  it("treats a missing layout as all-closed", () => {
    expect(panelsFromLayout(null)).toEqual({ diagnostics: false, explorer: false, changes: false });
    expect(panelsFromLayout({})).toEqual({ diagnostics: false, explorer: false, changes: false });
  });
});

describe("projectSnapshot + deriveProjectDirty", () => {
  const tabs = [meta({ id: 1, path: "C:\\data\\1.csv" }), meta({ id: 2, path: "C:\\data\\2.csv" })];

  it("is clean against its own baseline", () => {
    const snap = projectSnapshot(tabs, 1, layout);
    expect(deriveProjectDirty(snap, projectSnapshot(tabs, 1, layout))).toBe(false);
  });

  it("never reports dirty without a baseline", () => {
    expect(deriveProjectDirty(null, projectSnapshot(tabs, 1, layout))).toBe(false);
  });

  it("dirties on a changed active tab", () => {
    const base = projectSnapshot(tabs, 1, layout);
    expect(deriveProjectDirty(base, projectSnapshot(tabs, 2, layout))).toBe(true);
  });

  it("dirties on a reordered / added / removed document", () => {
    const base = projectSnapshot(tabs, 1, layout);
    const reordered = [tabs[1], tabs[0]];
    expect(deriveProjectDirty(base, projectSnapshot(reordered, 1, layout))).toBe(true);
    const added = [...tabs, meta({ id: 3, path: "C:\\data\\3.csv" })];
    expect(deriveProjectDirty(base, projectSnapshot(added, 1, layout))).toBe(true);
    expect(deriveProjectDirty(base, projectSnapshot([tabs[0]], 1, layout))).toBe(true);
  });

  it("dirties on a panel-layout change", () => {
    const base = projectSnapshot(tabs, 1, layout);
    const next = projectSnapshot(tabs, 1, { ...layout, explorer: true });
    expect(deriveProjectDirty(base, next)).toBe(true);
  });

  it("dirties on a changed active named view for a document", () => {
    const base = projectSnapshot(tabs, 1, layout, { 1: "v1" });
    expect(deriveProjectDirty(base, projectSnapshot(tabs, 1, layout, { 1: "v1" }))).toBe(false);
    expect(deriveProjectDirty(base, projectSnapshot(tabs, 1, layout, { 1: "v2" }))).toBe(true);
    // Clearing a previously-active view is also a change.
    expect(deriveProjectDirty(base, projectSnapshot(tabs, 1, layout, {}))).toBe(true);
  });

  it("ignores unsaved buffers in the tab order", () => {
    const withBuffer = [...tabs, meta({ id: 5, path: null })];
    const base = projectSnapshot(tabs, 1, layout);
    expect(deriveProjectDirty(base, projectSnapshot(withBuffer, 1, layout))).toBe(false);
  });

  it("compares tab order case/separator-insensitively", () => {
    const base = projectSnapshot(tabs, 1, layout);
    const equivalent = [
      meta({ id: 1, path: "c:/data/1.csv" }),
      meta({ id: 2, path: "c:/data/2.csv" }),
    ];
    expect(projectSnapshotsEqual(base, projectSnapshot(equivalent, 1, layout))).toBe(true);
  });
});

describe("orderTabsForPlan", () => {
  it("orders tabs to match the plan, appending extras", () => {
    const tabs = [
      { id: 10, path: "C:\\data\\b.csv" },
      { id: 11, path: "C:\\data\\a.csv" },
      { id: 12, path: "C:\\extra.csv" },
    ];
    const entries = [planEntry2("C:\\data\\a.csv"), planEntry2("C:\\data\\b.csv")];
    expect(orderTabsForPlan(tabs, entries)).toEqual([11, 10, 12]);
  });
});

function planEntry2(path: string): PlanEntry {
  return {
    sourceId: path,
    path,
    displayName: null,
    open: {},
    status: "ok",
    reapplyViews: true,
    viewWarnings: [],
    views: [],
    activeViewId: null,
  };
}
