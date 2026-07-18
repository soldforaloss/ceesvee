import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AnnotationsView,
  DbExportPreview,
  DictionaryView,
  DocumentMeta,
  ProjectMeta,
} from "../types";
import type { ExportForm } from "../lib/dbExport";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ destroy: vi.fn(), onCloseRequested: vi.fn() }),
}));
vi.mock("../lib/tauri", () => ({
  closeDocument: vi.fn().mockResolvedValue(undefined),
  probeOpen: vi.fn(),
  openFile: vi.fn(),
  startOpenIndexed: vi.fn(),
  getMeta: vi.fn(),
  find: vi.fn().mockResolvedValue([]),
  cancelJob: vi.fn().mockResolvedValue(true),
  setSettings: vi.fn().mockResolvedValue(undefined),
  listArchiveEntries: vi.fn(),
  startArchiveExtract: vi.fn(),
  pendingArchiveEstimate: vi.fn(),
  openArchiveDocument: vi.fn(),
  discardArchive: vi.fn(),
  applyDictionaryImport: vi.fn(),
  // F40 annotation persistence (sidecar vs project).
  annotationsEditRow: vi.fn(),
  annotationsSaveSidecar: vi.fn().mockResolvedValue(undefined),
  annotationsLoadSidecar: vi.fn(),
  annotationsView: vi.fn(),
  annotationsGetExport: vi.fn(),
  annotationsLoadExport: vi.fn().mockResolvedValue(undefined),
  // F35 database export preview.
  dbExportPreview: vi.fn(),
}));

import * as api from "../lib/tauri";
import { INDEXED_FIND_LIMIT, useStore, type DbExportState } from "./useStore";

function meta(id: number, backing: DocumentMeta["backing"] = "editable"): DocumentMeta {
  return {
    id,
    path: `C:/data/doc-${id}.csv`,
    fileName: `doc-${id}.csv`,
    rowCount: 100,
    totalRowCount: 100,
    filtered: false,
    colCount: 3,
    headers: ["a", "b", "c"],
    columnIds: ["c0", "c1", "c2"],
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
    backing,
    archive: null,
  };
}

describe("per-document UI state (F08)", () => {
  beforeEach(() => {
    useStore.setState({
      tabs: [meta(1), meta(2)],
      activeId: 1,
      uiStates: {},
      find: { ...useStore.getState().find, query: "", open: false, matches: [], index: 0 },
      columnWidths: {},
      frozenColumnCount: 0,
      selectionRect: null,
      selectedRows: [],
      selectedCols: [],
      scrollPosition: { row: 0, column: 0 },
      columnLayout: null,
      wrapText: false,
      activeViewId: null,
      viewSortKeys: [],
      viewWarning: null,
      error: null,
    });
  });

  it("keeps find, widths, frozen, selection and scroll independent per tab", () => {
    const s = useStore.getState();

    // Configure document 1's view.
    s.updateFind({ query: "alpha", open: true });
    s.setColumnWidth(0, 333);
    s.setFrozenCols(2);
    useStore.setState({
      selectionRect: { x: 1, y: 2, width: 3, height: 4 },
      selectedRows: [2, 3],
      selectedCols: [1],
      scrollPosition: { row: 50, column: 3 },
    });

    // Switching to document 2 exposes fresh defaults, not document 1's state.
    useStore.getState().setActive(2);
    let now = useStore.getState();
    expect(now.find.query).toBe("");
    expect(now.columnWidths).toEqual({});
    expect(now.frozenColumnCount).toBe(0);
    expect(now.selectionRect).toBeNull();
    expect(now.scrollPosition).toEqual({ row: 0, column: 0 });

    // Configure document 2 differently.
    useStore.getState().updateFind({ query: "beta" });
    useStore.getState().setColumnWidth(1, 99);

    // Back to document 1: everything restored exactly.
    useStore.getState().setActive(1);
    now = useStore.getState();
    expect(now.find.query).toBe("alpha");
    expect(now.find.open).toBe(true);
    expect(now.columnWidths).toEqual({ 0: 333 });
    expect(now.frozenColumnCount).toBe(2);
    expect(now.selectionRect).toEqual({ x: 1, y: 2, width: 3, height: 4 });
    expect(now.selectedRows).toEqual([2, 3]);
    expect(now.selectedCols).toEqual([1]);
    expect(now.scrollPosition).toEqual({ row: 50, column: 3 });

    // And document 2's own state is intact too.
    useStore.getState().setActive(2);
    now = useStore.getState();
    expect(now.find.query).toBe("beta");
    expect(now.columnWidths).toEqual({ 1: 99 });
  });

  it("filter-builder contents do not leak between documents", () => {
    const s = useStore.getState();
    s.updateFilterSpec({
      type: "group",
      id: "root",
      conjunction: "and",
      nodes: [
        {
          type: "condition",
          id: "c0",
          column: 2,
          op: "equals",
          value: "doc1-only",
          caseSensitive: false,
        },
      ],
    });
    useStore.getState().setActive(2);
    const specB = useStore.getState().filter.spec;
    expect(JSON.stringify(specB)).not.toContain("doc1-only");
    useStore.getState().setActive(1);
    const specA = useStore.getState().filter.spec;
    expect(JSON.stringify(specA)).toContain("doc1-only");
  });

  it("closing a tab removes its transient state", async () => {
    const s = useStore.getState();
    s.updateFind({ query: "alpha" });
    s.setActive(2);
    expect(useStore.getState().uiStates[1]).toBeDefined();

    await useStore.getState().closeTab(1);
    const now = useStore.getState();
    expect(now.uiStates[1]).toBeUndefined();
    expect(now.tabs.map((t) => t.id)).toEqual([2]);
    expect(now.activeId).toBe(2);
  });

  it("closing the active tab restores the neighbour's state", async () => {
    // Configure doc 1, switch to doc 2, configure it, then close doc 2.
    useStore.getState().updateFind({ query: "alpha" });
    useStore.getState().setActive(2);
    useStore.getState().updateFind({ query: "beta" });

    await useStore.getState().closeTab(2);
    const now = useStore.getState();
    expect(now.activeId).toBe(1);
    expect(now.find.query).toBe("alpha");
    expect(now.uiStates[2]).toBeUndefined();
  });

  it("keeps the F12 column layout, wrap and view state per tab", async () => {
    const s = useStore.getState();
    // Hide column b, pin c, wrap text on document 1.
    s.setColumnHidden(1, true);
    s.pinColumn(2, true);
    s.setWrapText(true);
    useStore.setState({ activeViewId: "view-x", viewWarning: "missing" });

    useStore.getState().setActive(2);
    let now = useStore.getState();
    expect(now.columnLayout).toBeNull();
    expect(now.wrapText).toBe(false);
    expect(now.activeViewId).toBeNull();
    expect(now.viewWarning).toBeNull();

    useStore.getState().setActive(1);
    now = useStore.getState();
    expect(now.columnLayout?.hiddenColumnIds).toEqual(["c1"]);
    expect(now.columnLayout?.pinnedColumnIds).toEqual(["c2"]);
    expect(now.wrapText).toBe(true);
    expect(now.activeViewId).toBe("view-x");
    expect(now.viewWarning).toBe("missing");

    // Closing drops every trace, layout included.
    useStore.getState().setActive(2);
    await useStore.getState().closeTab(1);
    expect(useStore.getState().uiStates[1]).toBeUndefined();
  });

  it("refuses to hide the last visible column", () => {
    const s = useStore.getState();
    s.setColumnHidden(0, true);
    s.setColumnHidden(1, true);
    s.setColumnHidden(2, true);
    const now = useStore.getState();
    expect(now.columnLayout?.hiddenColumnIds).toEqual(["c0", "c1"]);
    expect(now.error).toContain("At least one column");
  });

  it("reorders display columns into an ID-based order", () => {
    const s = useStore.getState();
    // Natural display = [a, b, c]; move display 0 to display 2.
    s.reorderColumns(0, 2);
    const now = useStore.getState();
    expect(now.columnLayout?.columnOrder).toEqual(["c1", "c2", "c0"]);
    expect(now.columnLayout?.pinnedColumnIds).toEqual([]);
  });
});

describe("indexed read-only open flow (F10)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useStore.setState({
      tabs: [],
      activeId: null,
      uiStates: {},
      openDecision: null,
      indexing: null,
      busy: false,
      error: null,
    });
  });

  const estimate = (needsDecision: boolean) => ({
    fileSize: 2_000_000_000,
    estimatedRows: 20_000_000,
    estimatedMemory: 6_000_000_000,
    needsDecision,
    encoding: "UTF-8",
  });

  it("pauses large opens on the mode decision instead of loading", async () => {
    vi.mocked(api.probeOpen).mockResolvedValue(estimate(true));
    await useStore.getState().openPath("C:/data/huge.csv");
    const s = useStore.getState();
    expect(s.openDecision?.path).toBe("C:/data/huge.csv");
    expect(s.busy).toBe(false);
    expect(api.openFile).not.toHaveBeenCalled();
  });

  it("opens small files directly after the probe", async () => {
    vi.mocked(api.probeOpen).mockResolvedValue(estimate(false));
    vi.mocked(api.openFile).mockResolvedValue(meta(7));
    await useStore.getState().openPath("C:/data/small.csv");
    const s = useStore.getState();
    expect(s.openDecision).toBeNull();
    expect(s.tabs.map((t) => t.id)).toEqual([7]);
    expect(api.openFile).toHaveBeenCalledWith("C:/data/small.csv");
  });

  it("confirmOpenEditable forces the in-memory load", async () => {
    useStore.setState({ openDecision: { path: "C:/data/huge.csv", estimate: estimate(true) } });
    vi.mocked(api.openFile).mockResolvedValue(meta(3));
    await useStore.getState().confirmOpenEditable();
    expect(api.openFile).toHaveBeenCalledWith("C:/data/huge.csv", { forceInMemory: true });
    expect(useStore.getState().tabs.map((t) => t.id)).toEqual([3]);
  });

  it("confirmOpenIndexed tracks the job and adds the tab on finish", async () => {
    useStore.setState({ openDecision: { path: "C:/data/huge.csv", estimate: estimate(true) } });
    vi.mocked(api.startOpenIndexed).mockResolvedValue({ jobId: 41, docId: 9 });
    await useStore.getState().confirmOpenIndexed();
    let s = useStore.getState();
    expect(s.openDecision).toBeNull();
    expect(s.indexing).toMatchObject({ jobId: 41, docId: 9, kind: "openIndexed" });

    const indexedMeta = meta(9, "indexedReadOnly");
    vi.mocked(api.getMeta).mockResolvedValue(indexedMeta);
    await useStore.getState().handleJobFinished({
      jobId: 41,
      docId: 9,
      kind: "openIndexed",
      status: "done",
      error: null,
    });
    s = useStore.getState();
    expect(s.indexing).toBeNull();
    expect(s.tabs.map((t) => t.id)).toEqual([9]);
    expect(s.activeId).toBe(9);
    expect(s.tabs[0].backing).toBe("indexedReadOnly");
  });

  it("surfaces a failed indexing job as an error", async () => {
    useStore.setState({
      indexing: {
        jobId: 5,
        docId: 2,
        kind: "openIndexed",
        path: "C:/x.csv",
        processed: 0,
        total: null,
      },
    });
    await useStore.getState().handleJobFinished({
      jobId: 5,
      docId: 2,
      kind: "openIndexed",
      status: "failed",
      error: "disk error",
    });
    const s = useStore.getState();
    expect(s.indexing).toBeNull();
    expect(s.error).toBe("disk error");
    expect(s.tabs).toHaveLength(0);
  });

  it("caps find matches for indexed documents only", async () => {
    useStore.setState({
      tabs: [meta(1, "indexedReadOnly"), meta(2)],
      activeId: 1,
      find: { ...useStore.getState().find, query: "x", open: true },
    });
    await useStore.getState().runFind();
    expect(vi.mocked(api.find).mock.calls[0][1].limit).toBe(INDEXED_FIND_LIMIT);

    useStore.getState().setActive(2);
    useStore.setState({ find: { ...useStore.getState().find, query: "x" } });
    await useStore.getState().runFind();
    expect(vi.mocked(api.find).mock.calls[1][1].limit).toBeUndefined();
  });
});

describe("shortcut overrides and modals (F11)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useStore.setState({
      settings: { version: 1, profiles: [] },
      activeModal: null,
      paletteOpen: false,
      paletteArgCommandId: null,
      error: null,
    });
  });

  it("persists rebinds, unbinds, and resets through settings", async () => {
    await useStore.getState().setShortcutOverride("file.save", "mod+shift+x");
    expect(useStore.getState().settings?.shortcutOverrides).toEqual({
      "file.save": "mod+shift+x",
    });

    await useStore.getState().setShortcutOverride("edit.undo", null);
    expect(useStore.getState().settings?.shortcutOverrides).toEqual({
      "file.save": "mod+shift+x",
      "edit.undo": null,
    });

    await useStore.getState().setShortcutOverride("file.save", undefined);
    expect(useStore.getState().settings?.shortcutOverrides).toEqual({ "edit.undo": null });
    expect(vi.mocked(api.setSettings)).toHaveBeenCalledTimes(3);
  });

  it("keeps the store state on a failed settings write", async () => {
    vi.mocked(api.setSettings).mockRejectedValueOnce(new Error("disk full"));
    await useStore.getState().setShortcutOverride("file.save", "mod+1");
    expect(useStore.getState().settings?.shortcutOverrides).toBeUndefined();
    expect(useStore.getState().error).toContain("disk full");
  });

  it("opening the palette in argument mode records the command", () => {
    useStore.getState().openPaletteForArg("nav.goToRow");
    expect(useStore.getState().paletteOpen).toBe(true);
    expect(useStore.getState().paletteArgCommandId).toBe("nav.goToRow");
    useStore.getState().setPaletteOpen(false);
    expect(useStore.getState().paletteArgCommandId).toBeNull();
  });

  it("modals are owned by the store, one at a time", () => {
    useStore.getState().setModal("sort");
    expect(useStore.getState().activeModal).toBe("sort");
    useStore.getState().setModal("export");
    expect(useStore.getState().activeModal).toBe("export");
    useStore.getState().setModal(null);
    expect(useStore.getState().activeModal).toBeNull();
  });
});

describe("compressed file open flow (F17)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useStore.setState({
      tabs: [],
      activeId: null,
      uiStates: {},
      openDecision: null,
      indexing: null,
      archivePick: null,
      archiveLargeConfirm: null,
      busy: false,
      error: null,
    });
  });

  it("routes .zip opens through the entry chooser when several entries exist", async () => {
    const entries = [
      {
        name: "a.csv",
        compressedSize: 10,
        uncompressedSize: 100,
        ratio: 10,
        encrypted: false,
        likelyDelimiter: ",",
        likelyEncoding: "UTF-8",
      },
      {
        name: "b.csv",
        compressedSize: 10,
        uncompressedSize: 100,
        ratio: 10,
        encrypted: false,
        likelyDelimiter: ",",
        likelyEncoding: "UTF-8",
      },
    ];
    vi.mocked(api.listArchiveEntries).mockResolvedValue(entries);
    await useStore.getState().openPath("C:/data/bundle.zip");
    expect(useStore.getState().archivePick?.entries).toHaveLength(2);
    expect(api.startArchiveExtract).not.toHaveBeenCalled();
  });

  it("extracts .gz directly and tracks the job", async () => {
    vi.mocked(api.startArchiveExtract).mockResolvedValue({ jobId: 7, token: 3 });
    await useStore.getState().openPath("C:/data/log.csv.gz");
    const s = useStore.getState();
    expect(s.indexing).toMatchObject({
      jobId: 7,
      kind: "archiveExtract",
      archiveToken: 3,
    });
    expect(api.startArchiveExtract).toHaveBeenCalledWith("C:/data/log.csv.gz", null, false);
  });

  it("a suspicious-ratio failure surfaces the confirm instead of an error", async () => {
    useStore.setState({
      indexing: {
        jobId: 9,
        docId: 0,
        kind: "archiveExtract",
        path: "C:/x.zip",
        processed: 0,
        total: null,
        archiveToken: 5,
        archiveEntry: "big.csv",
      },
    });
    await useStore.getState().handleJobFinished({
      jobId: 9,
      docId: null,
      kind: "archiveExtract",
      status: "failed",
      error: "invalid argument: suspicious compression ratio (over 200:1)",
    });
    const s = useStore.getState();
    expect(s.archiveLargeConfirm).toEqual({ path: "C:/x.zip", entry: "big.csv" });
    expect(s.error).toBeNull();
  });

  it("a finished extraction below the threshold opens editable directly", async () => {
    useStore.setState({
      indexing: {
        jobId: 11,
        docId: 0,
        kind: "archiveExtract",
        path: "C:/data/log.csv.gz",
        processed: 0,
        total: null,
        archiveToken: 8,
        archiveEntry: null,
      },
    });
    vi.mocked(api.pendingArchiveEstimate).mockResolvedValue({
      fileSize: 1000,
      estimatedRows: 10,
      estimatedMemory: 4000,
      needsDecision: false,
      encoding: "UTF-8",
    });
    vi.mocked(api.openArchiveDocument).mockResolvedValue({ jobId: 0, docId: 4 });
    vi.mocked(api.getMeta).mockResolvedValue(meta(4));
    await useStore.getState().handleJobFinished({
      jobId: 11,
      docId: null,
      kind: "archiveExtract",
      status: "done",
      error: null,
    });
    const s = useStore.getState();
    expect(api.openArchiveDocument).toHaveBeenCalledWith(8, "editable");
    expect(s.tabs.map((t) => t.id)).toEqual([4]);
  });

  it("dismissing an archive decision discards the pending extraction", async () => {
    vi.mocked(api.discardArchive).mockResolvedValue(undefined);
    useStore.setState({
      openDecision: {
        path: "C:/x.zip",
        estimate: {
          fileSize: 1,
          estimatedRows: 1,
          estimatedMemory: 1,
          needsDecision: true,
          encoding: "UTF-8",
        },
        archiveToken: 12,
      },
    });
    useStore.getState().dismissOpenDecision();
    expect(api.discardArchive).toHaveBeenCalledWith(12);
    expect(useStore.getState().openDecision).toBeNull();
  });
});

describe("dictionary import stale-plan guard (F38)", () => {
  const dictView = (rev: number): DictionaryView => ({
    dictionaryRevision: rev,
    revision: 1,
    entries: [],
    orphans: [],
  });

  beforeEach(() => {
    vi.clearAllMocks();
    useStore.setState({
      tabs: [meta(1)],
      activeId: 1,
      // The LIVE view has moved on (an edit was saved after the preview).
      dictionaryView: dictView(9),
      error: null,
    });
  });

  it("applies the plan-time revision, not the live view's, so a stale apply is guarded", async () => {
    vi.mocked(api.applyDictionaryImport).mockResolvedValue({
      matchedColumns: 0,
      newEntries: 0,
      updatedEntries: 0,
      fieldsAdded: 0,
      conflictsResolved: 0,
      unmatched: [],
      view: dictView(10),
    });

    // The dialog passes the revision captured when the plan was previewed (7),
    // even though the store's current view is at 9.
    await useStore
      .getState()
      .applyDictionaryImport("C:/x.dictionary.json", "auto", { type: "keepAllExisting" }, 7);

    const call = vi.mocked(api.applyDictionaryImport).mock.calls[0];
    // (docId, path, matchBy, resolution, expectedDictionaryRevision)
    expect(call[4]).toBe(7);
    expect(call[4]).not.toBe(9);
  });
});

describe("annotation persistence: sidecar vs project (F40)", () => {
  const annView = (rev: number): AnnotationsView => ({
    annotationsRevision: rev,
    revision: 1,
    tags: [],
    matched: 0,
    ambiguous: 0,
    orphaned: 0,
    entries: [],
  });

  const project = (): ProjectMeta => ({
    path: "C:/proj/work.ceesveeproj",
    name: "work",
    dirty: false,
    revision: 1,
    formatVersion: "1",
    appVersion: "0.4.0",
  });

  beforeEach(() => {
    vi.clearAllMocks();
    useStore.setState({
      tabs: [meta(1)],
      activeId: 1,
      project: null,
      annotationsView: annView(0),
      error: null,
    });
    vi.mocked(api.annotationsEditRow).mockResolvedValue(annView(1));
  });

  it("writes the sidecar on an edit when NO project is open", async () => {
    const ok = await useStore.getState().applyRowMarks([0], { star: true });
    expect(ok).toBe(true);
    expect(api.annotationsEditRow).toHaveBeenCalled();
    expect(api.annotationsSaveSidecar).toHaveBeenCalledWith(1, "C:/data/doc-1.csv");
  });

  it("does NOT touch the sidecar on an edit when a project IS open", async () => {
    useStore.setState({ project: project() });
    const ok = await useStore.getState().applyRowMarks([0], { star: true });
    expect(ok).toBe(true);
    // The edit still lands in the registry (captured into the project on save),
    // but the per-source sidecar is never written — the two are exclusive.
    expect(api.annotationsEditRow).toHaveBeenCalled();
    expect(api.annotationsSaveSidecar).not.toHaveBeenCalled();
  });

  it("hydrates the sidecar when an INDEXED open finishes", async () => {
    useStore.setState({
      tabs: [],
      activeId: null,
      project: null,
      indexing: {
        jobId: 41,
        docId: 9,
        kind: "openIndexed",
        path: "C:/data/doc-9.csv",
        processed: 0,
        total: null,
      },
    });
    vi.mocked(api.getMeta).mockResolvedValue(meta(9, "indexedReadOnly"));
    vi.mocked(api.annotationsLoadSidecar).mockResolvedValue(annView(0));

    await useStore.getState().handleJobFinished({
      jobId: 41,
      docId: 9,
      kind: "openIndexed",
      status: "done",
      error: null,
    });

    // The large read-only document reads its existing sidecar on open, so a
    // later edit merges instead of overwriting an empty store onto it.
    expect(api.annotationsLoadSidecar).toHaveBeenCalledWith(9, "C:/data/doc-9.csv");
  });
});

describe("database export preview invalidation (F35)", () => {
  const exportForm = (over: Partial<ExportForm> = {}): ExportForm => ({
    path: "/tmp/out.sqlite",
    table: "customers",
    mode: "create",
    conflictPolicy: "abort",
    confirmReplace: false,
    overrides: {},
    ...over,
  });

  const preview = (over: Partial<DbExportPreview> = {}): DbExportPreview => ({
    revision: 3,
    tableExists: false,
    targetRows: null,
    columns: [],
    blocking: [],
    failures: [],
    failureCount: 0,
    rowsScanned: 10,
    scanComplete: true,
    ...over,
  });

  const exportState = (over: Partial<DbExportState> = {}): DbExportState => ({
    docId: 1,
    docName: "doc-1.csv",
    form: exportForm(),
    preview: null,
    previewLoading: false,
    previewError: null,
    jobId: null,
    processed: 0,
    total: null,
    result: null,
    error: null,
    ...over,
  });

  beforeEach(() => {
    vi.mocked(api.dbExportPreview).mockReset();
    useStore.setState({ dbExport: exportState() });
  });

  afterEach(() => {
    // Clears the debounce timer patchDbExportForm scheduled so it can never
    // fire into a later test.
    useStore.getState().closeDbExport();
  });

  it("clears an existing preview synchronously so Export cannot run against a changed form", () => {
    // A clean preview is present (Export would be enabled)…
    useStore.setState({ dbExport: exportState({ preview: preview() }) });
    expect(useStore.getState().dbExport?.preview).not.toBeNull();

    // …changing the form drops it immediately, before any new preview runs.
    useStore.getState().patchDbExportForm({ table: "renamed" });
    const st = useStore.getState().dbExport;
    expect(st?.preview).toBeNull();
    expect(st?.previewLoading).toBe(true);
    expect(st?.form.table).toBe("renamed");
    // The old, resolved-but-now-stale response never gets a chance to run.
    expect(api.dbExportPreview).not.toHaveBeenCalled();
  });

  it("ignores an in-flight preview response that resolves after the form changed", async () => {
    // Hold the preview invoke open so we can change the form while it is in
    // flight and only then let it resolve.
    const deferred: { resolve: (p: DbExportPreview) => void } = { resolve: () => {} };
    vi.mocked(api.dbExportPreview).mockImplementationOnce(
      () =>
        new Promise<DbExportPreview>((resolve) => {
          deferred.resolve = resolve;
        }),
    );

    const pending = useStore.getState().previewDbExport();
    expect(api.dbExportPreview).toHaveBeenCalledTimes(1);

    // The form changes while that first invoke is still pending.
    useStore.getState().patchDbExportForm({ table: "renamed" });

    // The stale invoke now resolves with a preview computed for the OLD form.
    deferred.resolve(preview({ revision: 99 }));
    await pending;

    // It must NOT be applied: the token bumped when the form changed, so the
    // preview stays cleared and Export stays disabled until the fresh preview
    // for the new form lands.
    const st = useStore.getState().dbExport;
    expect(st?.preview).toBeNull();
    expect(st?.form.table).toBe("renamed");
  });
});
