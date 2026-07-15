import { create } from "zustand";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";

import * as api from "../lib/tauri";
import { applyReplace } from "../lib/replace";
import type {
  CellRect,
  ColumnSummary,
  DiagnosticsReport,
  DocumentMeta,
  ExportOptions,
  FilterGroup,
  FindMatch,
  FindOptions,
  JobFinished,
  JobProgress,
  OpenOptions,
  SortKey,
} from "../types";

export type ThemePref = "light" | "dark" | "system";

const RECENT_KEY = "ceesvee-recent";
const THEME_KEY = "ceesvee-theme";
const MAX_RECENT = 10;

// Debounce timer for the (backend-computed) selection statistics.
let statsTimer: ReturnType<typeof setTimeout> | null = null;
// Debounce timer for the (backend-computed) per-column summaries.
let summariesTimer: ReturnType<typeof setTimeout> | null = null;

const FILE_FILTERS = [
  { name: "Delimited text", extensions: ["csv", "tsv", "tab", "txt", "psv", "dat"] },
  { name: "All files", extensions: ["*"] },
];

export interface SelectionInfo {
  count: number;
  numericCount: number;
  sum: number;
  avg: number | null;
  min: number | null;
  max: number | null;
}

export interface FindState {
  open: boolean;
  query: string;
  replacement: string;
  regex: boolean;
  caseSensitive: boolean;
  wholeCell: boolean;
  inSelection: boolean;
  matches: FindMatch[];
  index: number;
}

const initialFind: FindState = {
  open: false,
  query: "",
  replacement: "",
  regex: false,
  caseSensitive: false,
  wholeCell: false,
  inSelection: false,
  matches: [],
  index: 0,
};

export interface FilterState {
  open: boolean;
  /** The query-builder tree (kept even while not applied, for editing). */
  spec: FilterGroup;
}

/** Diagnostics scan/report state for one document. */
export interface DiagnosticsDocState {
  /** Last completed report (kept visible, marked stale, while rescanning). */
  report: DiagnosticsReport | null;
  /** Running scan job, if any. */
  jobId: number | null;
  processed: number;
  total: number | null;
  /** Terminal error of the last scan, if it failed. */
  scanError: string | null;
}

const initialDiagnosticsDocState: DiagnosticsDocState = {
  report: null,
  jobId: null,
  processed: 0,
  total: null,
  scanError: null,
};

/** One-shot request for the grid to scroll to and select a cell. */
export interface JumpTarget {
  row: number;
  col: number;
  /** Distinguishes repeated jumps to the same cell. */
  nonce: number;
}

let jumpNonce = 0;

const initialFilter: FilterState = {
  open: false,
  spec: {
    type: "group",
    id: "root",
    conjunction: "and",
    nodes: [
      { type: "condition", id: "c0", column: 0, op: "contains", value: "", caseSensitive: false },
    ],
  },
};

interface Store {
  tabs: DocumentMeta[];
  activeId: number | null;
  recent: string[];
  theme: ThemePref;
  /** Bumped to invalidate the grid's row cache after structural changes. */
  dataVersion: number;
  busy: boolean;
  error: string | null;
  selection: SelectionInfo | null;
  selectionRect: CellRect | null;
  selectedRows: number[];
  selectedCols: number[];
  find: FindState;
  filter: FilterState;
  /** Per-document count of pinned leading columns, keyed by doc id. */
  frozenCols: Record<number, number>;
  /** Detected per-column type + summary for the active doc (null until loaded). */
  summaries: ColumnSummary[] | null;
  /** Which document `summaries` belong to (guards against cross-tab staleness). */
  summariesDocId: number | null;
  /** Whether the diagnostics side panel is shown. */
  diagnosticsOpen: boolean;
  /** Diagnostics reports and scan state, keyed by document id. */
  diagnostics: Record<number, DiagnosticsDocState>;
  /** One-shot cell-jump request consumed by the grid. */
  jumpTarget: JumpTarget | null;

  // lifecycle / chrome
  init: () => void;
  setTheme: (theme: ThemePref) => void;
  setError: (error: string | null) => void;
  setActive: (id: number) => void;
  setSelection: (rect: CellRect | null, rows: number[], cols: number[]) => void;
  setFrozenCols: (count: number) => void;
  loadSummaries: () => void;

  // documents
  openDialog: () => Promise<void>;
  openPath: (path: string) => Promise<void>;
  newDoc: () => Promise<void>;
  closeTab: (id: number) => Promise<void>;
  reparse: (options: OpenOptions) => Promise<void>;
  setHeaderMode: (hasHeader: boolean) => Promise<void>;

  // editing
  setCell: (row: number, col: number, value: string) => Promise<void>;
  pasteBlock: (row: number, col: number, block: string[][]) => Promise<void>;
  insertRows: (at: number, count: number) => Promise<void>;
  deleteRows: (indices: number[]) => Promise<void>;
  moveRow: (from: number, to: number) => Promise<void>;
  insertColumn: (at: number, name: string) => Promise<void>;
  deleteColumns: (indices: number[]) => Promise<void>;
  renameColumn: (col: number, name: string) => Promise<void>;
  sortBy: (keys: SortKey[]) => Promise<void>;
  undo: () => Promise<void>;
  redo: () => Promise<void>;
  saveActive: (saveAs: boolean, exportOptions?: Partial<ExportOptions>) => Promise<void>;

  // find / replace
  setFindOpen: (open: boolean) => void;
  updateFind: (patch: Partial<FindState>) => void;
  runFind: () => Promise<void>;
  gotoMatch: (delta: number) => void;
  replaceCurrent: () => Promise<void>;
  replaceAllMatches: () => Promise<void>;

  // filter
  setFilterOpen: (open: boolean) => void;
  updateFilterSpec: (spec: FilterGroup) => void;
  applyFilter: (spec: FilterGroup) => Promise<void>;
  clearFilter: () => Promise<void>;

  // diagnostics
  setDiagnosticsOpen: (open: boolean) => void;
  runDiagnosticsScan: () => Promise<void>;
  cancelDiagnosticsScan: () => Promise<void>;
  applyIssueFilter: (issueId: string) => Promise<void>;
  jumpToCell: (row: number, col: number) => Promise<void>;

  // background-job events (wired to the Tauri event listeners in App)
  handleJobProgress: (progress: JobProgress) => void;
  handleJobFinished: (finished: JobFinished) => Promise<void>;
}

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

function loadTheme(): ThemePref {
  const t = localStorage.getItem(THEME_KEY);
  return t === "light" || t === "dark" || t === "system" ? t : "system";
}

function applyThemeClass(theme: ThemePref) {
  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const dark = theme === "dark" || (theme === "system" && prefersDark);
  document.documentElement.classList.toggle("dark", dark);
}

export const useStore = create<Store>((set, get) => {
  // ----- internal helpers -------------------------------------------------

  const activeMeta = (): DocumentMeta | null => {
    const { tabs, activeId } = get();
    return tabs.find((t) => t.id === activeId) ?? null;
  };

  /** Replace a tab's metadata (dirty/undo flags) without reloading the grid. */
  const refreshMeta = (meta: DocumentMeta) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === meta.id ? meta : t)) }));

  /** Replace metadata AND invalidate the grid cache (structural change). */
  const reloadDoc = (meta: DocumentMeta) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === meta.id ? meta : t)),
      dataVersion: s.dataVersion + 1,
    }));

  const pushRecent = (path: string) => {
    const next = [path, ...get().recent.filter((p) => p !== path)].slice(0, MAX_RECENT);
    set({ recent: next });
    try {
      localStorage.setItem(RECENT_KEY, JSON.stringify(next));
    } catch {
      /* ignore */
    }
  };

  /** Run a structural mutation against the active doc with error handling. */
  const mutate = async (fn: (id: number) => Promise<DocumentMeta>, reload = true) => {
    const id = get().activeId;
    if (id == null) return;
    try {
      const meta = await fn(id);
      if (reload) reloadDoc(meta);
      else refreshMeta(meta);
    } catch (e) {
      set({ error: String(e) });
    }
  };

  /** Merge a partial update into one document's diagnostics state. */
  const patchDiagnostics = (docId: number, patch: Partial<DiagnosticsDocState>) => {
    set((s) => ({
      diagnostics: {
        ...s.diagnostics,
        [docId]: { ...(s.diagnostics[docId] ?? initialDiagnosticsDocState), ...patch },
      },
    }));
  };

  return {
    tabs: [],
    activeId: null,
    recent: [],
    theme: "system",
    dataVersion: 0,
    busy: false,
    error: null,
    selection: null,
    selectionRect: null,
    selectedRows: [],
    selectedCols: [],
    find: initialFind,
    filter: initialFilter,
    frozenCols: {},
    summaries: null,
    summariesDocId: null,
    diagnosticsOpen: false,
    diagnostics: {},
    jumpTarget: null,

    init: () => {
      const theme = loadTheme();
      applyThemeClass(theme);
      set({ recent: loadRecent(), theme });
      window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
        if (get().theme === "system") applyThemeClass("system");
      });
    },

    setTheme: (theme) => {
      try {
        localStorage.setItem(THEME_KEY, theme);
      } catch {
        /* ignore */
      }
      applyThemeClass(theme);
      set({ theme });
    },

    setError: (error) => set({ error }),

    setActive: (id) =>
      set({
        activeId: id,
        selection: null,
        selectionRect: null,
        selectedRows: [],
        selectedCols: [],
        summaries: null,
        summariesDocId: null,
        jumpTarget: null,
      }),

    setSelection: (rect, rows, cols) => {
      set({ selectionRect: rect, selectedRows: rows, selectedCols: cols });
      if (statsTimer !== null) clearTimeout(statsTimer);
      const id = get().activeId;
      if (id === null || !rect || rect.width * rect.height <= 1) {
        set({ selection: null });
        return;
      }
      // Compute aggregates in Rust over the full range (the front-end cache only
      // holds the visible window, so client-side stats would be wrong for large
      // selections). Debounced and guarded against stale results.
      statsTimer = setTimeout(() => {
        void api
          .selectionStats(id, rect)
          .then((stats) => {
            if (get().selectionRect === rect) set({ selection: stats });
          })
          .catch(() => undefined);
      }, 120);
    },

    setFrozenCols: (count) =>
      set((s) => {
        const id = s.activeId;
        if (id == null) return {};
        return { frozenCols: { ...s.frozenCols, [id]: Math.max(0, count) } };
      }),

    loadSummaries: () => {
      const id = get().activeId;
      if (id == null) {
        set({ summaries: null, summariesDocId: null });
        return;
      }
      // Computed in Rust over the full document (the front-end cache only holds
      // the visible window). Debounced and guarded against a tab switch.
      if (summariesTimer !== null) clearTimeout(summariesTimer);
      summariesTimer = setTimeout(() => {
        void api
          .columnSummaries(id)
          .then((summaries) => {
            if (get().activeId === id) set({ summaries, summariesDocId: id });
          })
          .catch(() => undefined);
      }, 150);
    },

    openDialog: async () => {
      const selected = await openFileDialog({ multiple: false, filters: FILE_FILTERS });
      if (typeof selected === "string") await get().openPath(selected);
    },

    openPath: async (path) => {
      const existing = get().tabs.find((t) => t.path === path);
      if (existing) {
        set({ activeId: existing.id });
        return;
      }
      set({ busy: true, error: null });
      try {
        const meta = await api.openFile(path);
        set((s) => ({ tabs: [...s.tabs, meta], activeId: meta.id, busy: false }));
        pushRecent(path);
      } catch (e) {
        set({ error: String(e), busy: false });
      }
    },

    newDoc: async () => {
      try {
        const meta = await api.newDocument(50, 4);
        set((s) => ({ tabs: [...s.tabs, meta], activeId: meta.id }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    closeTab: async (id) => {
      await api.closeDocument(id).catch(() => undefined);
      set((s) => {
        const tabs = s.tabs.filter((t) => t.id !== id);
        const closingActive = s.activeId === id;
        const activeId = closingActive
          ? tabs.length
            ? tabs[tabs.length - 1].id
            : null
          : s.activeId;
        const frozenCols = { ...s.frozenCols };
        delete frozenCols[id];
        const diagnostics = { ...s.diagnostics };
        delete diagnostics[id];
        // Only invalidate the grid cache when the active document actually
        // changed; closing a background tab must not refetch the active grid.
        return {
          tabs,
          activeId,
          frozenCols,
          diagnostics,
          dataVersion: closingActive ? s.dataVersion + 1 : s.dataVersion,
        };
      });
    },

    reparse: async (options) => {
      const id = get().activeId;
      if (id == null) return;
      set({ busy: true, summaries: null, summariesDocId: null });
      try {
        const meta = await api.reparse(id, options);
        reloadDoc(meta);
        set({ busy: false });
      } catch (e) {
        set({ error: String(e), busy: false });
      }
    },

    setHeaderMode: (hasHeader) => {
      // Promoting/demoting the header row re-interprets every column.
      set({ summaries: null, summariesDocId: null });
      return mutate((id) => api.setHeaderMode(id, hasHeader));
    },

    setCell: (row, col, value) => mutate((id) => api.setCell(id, row, col, value), false),
    pasteBlock: (row, col, block) => mutate((id) => api.paste(id, row, col, block)),
    insertRows: (at, count) => mutate((id) => api.insertRows(id, at, count)),
    deleteRows: (indices) => mutate((id) => api.deleteRows(id, indices)),
    moveRow: (from, to) => mutate((id) => api.moveRow(id, from, to)),
    insertColumn: (at, name) => {
      const id = get().activeId;
      if (id != null) {
        // Column identity shifts: invalidate summaries and keep the frozen
        // boundary on the same logical columns (shift it right if we inserted
        // within the frozen region).
        set((s) => {
          const frozen = s.frozenCols[id] ?? 0;
          return {
            summaries: null,
            summariesDocId: null,
            frozenCols: { ...s.frozenCols, [id]: at < frozen ? frozen + 1 : frozen },
          };
        });
      }
      return mutate((docId) => api.insertColumn(docId, at, name));
    },
    deleteColumns: (indices) => {
      const id = get().activeId;
      if (id != null) {
        set((s) => {
          const frozen = s.frozenCols[id] ?? 0;
          const removedBelow = indices.filter((c) => c < frozen).length;
          return {
            summaries: null,
            summariesDocId: null,
            frozenCols: { ...s.frozenCols, [id]: Math.max(0, frozen - removedBelow) },
          };
        });
      }
      return mutate((docId) => api.deleteColumns(docId, indices));
    },
    renameColumn: (col, name) => mutate((id) => api.renameColumn(id, col, name)),
    sortBy: (keys) => mutate((id) => api.sort(id, keys)),
    undo: () => mutate((id) => api.undo(id)),
    redo: () => mutate((id) => api.redo(id)),

    saveActive: async (saveAs, exportOptions) => {
      const meta = activeMeta();
      if (!meta) return;
      let path = meta.path;
      if (saveAs || !path) {
        const chosen = await saveFileDialog({
          defaultPath: meta.fileName,
          filters: FILE_FILTERS,
        });
        if (!chosen) return;
        path = chosen;
      }
      const options: ExportOptions = {
        delimiter: meta.delimiter || ",",
        encoding: meta.encoding || "UTF-8",
        quoteStyle: "minimal",
        lineEnding: meta.lineEnding,
        bom: meta.hadBom,
        includeHeaders: true,
        ...exportOptions,
      };
      set({ busy: true });
      try {
        const updated = await api.save(meta.id, path, options);
        refreshMeta(updated);
        if (updated.path) pushRecent(updated.path);
        set({ busy: false });
      } catch (e) {
        set({ error: String(e), busy: false });
      }
    },

    // ----- find / replace -------------------------------------------------

    setFindOpen: (open) =>
      set((s) => ({ find: { ...s.find, open, matches: open ? s.find.matches : [] } })),

    updateFind: (patch) => set((s) => ({ find: { ...s.find, ...patch } })),

    runFind: async () => {
      const id = get().activeId;
      const { find, selectionRect } = get();
      if (id == null || find.query === "") {
        set((s) => ({ find: { ...s.find, matches: [], index: 0 } }));
        return;
      }
      const options: FindOptions = {
        query: find.query,
        regex: find.regex,
        caseSensitive: find.caseSensitive,
        wholeCell: find.wholeCell,
        selection: find.inSelection && selectionRect ? selectionRect : undefined,
      };
      try {
        const matches = await api.find(id, options);
        set((s) => ({ find: { ...s.find, matches, index: 0 } }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    gotoMatch: (delta) =>
      set((s) => {
        const n = s.find.matches.length;
        if (n === 0) return {};
        const index = (s.find.index + delta + n) % n;
        return { find: { ...s.find, index } };
      }),

    replaceCurrent: async () => {
      const id = get().activeId;
      const { find, selectionRect } = get();
      const match = find.matches[find.index];
      if (id == null || !match) return;
      try {
        const win = await api.getRows(id, match.row, 1);
        const current = win.rows[0]?.[match.col] ?? "";
        const options: FindOptions = {
          query: find.query,
          regex: find.regex,
          caseSensitive: find.caseSensitive,
          wholeCell: find.wholeCell,
        };
        const next = applyReplace(current, options, find.replacement);
        if (next !== current) {
          const meta = await api.setCell(id, match.row, match.col, next);
          reloadDoc(meta);
        }
        // Recompute matches and advance to the first one AFTER the replaced cell,
        // so sequential Replace moves forward (and doesn't loop on a replacement
        // that still matches the query).
        const matches = await api.find(id, {
          ...options,
          selection: find.inSelection && selectionRect ? selectionRect : undefined,
        });
        let index = matches.findIndex(
          (m) => m.row > match.row || (m.row === match.row && m.col > match.col),
        );
        if (index < 0) index = 0;
        set((s) => ({ find: { ...s.find, matches, index: matches.length ? index : 0 } }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    replaceAllMatches: async () => {
      const id = get().activeId;
      const { find, selectionRect } = get();
      if (id == null || find.query === "") return;
      const options: FindOptions = {
        query: find.query,
        regex: find.regex,
        caseSensitive: find.caseSensitive,
        wholeCell: find.wholeCell,
        selection: find.inSelection && selectionRect ? selectionRect : undefined,
      };
      try {
        const result = await api.replaceAll(id, options, find.replacement);
        reloadDoc(result.meta);
        set((s) => ({ find: { ...s.find, matches: [], index: 0 } }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    // ----- filter ---------------------------------------------------------

    setFilterOpen: (open) => set((s) => ({ filter: { ...s.filter, open } })),

    updateFilterSpec: (spec) => set((s) => ({ filter: { ...s.filter, spec } })),

    // Applying/clearing a filter changes the visible row set, so both go through
    // reloadDoc (bumping dataVersion) to refetch the grid against the new view.
    applyFilter: async (spec) => {
      set((s) => ({ filter: { ...s.filter, spec } }));
      await mutate((id) => api.setFilter(id, spec));
    },

    clearFilter: () => mutate((id) => api.clearFilter(id)),

    // ----- diagnostics ------------------------------------------------------

    setDiagnosticsOpen: (open) => set({ diagnosticsOpen: open }),

    runDiagnosticsScan: async () => {
      const meta = activeMeta();
      if (!meta) return;
      const existing = get().diagnostics[meta.id];
      if (existing?.jobId != null) return; // a scan is already running
      try {
        const jobId = await api.startDiagnosticsScan(meta.id, meta.revision);
        patchDiagnostics(meta.id, { jobId, processed: 0, total: null, scanError: null });
      } catch (e) {
        patchDiagnostics(meta.id, { scanError: String(e) });
      }
    },

    cancelDiagnosticsScan: async () => {
      const id = get().activeId;
      if (id == null) return;
      const jobId = get().diagnostics[id]?.jobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    applyIssueFilter: async (issueId) => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        const updated = await api.applyDiagnosticFilter(meta.id, issueId, meta.revision);
        reloadDoc(updated);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    jumpToCell: async (row, col) => {
      const meta = activeMeta();
      if (!meta) return;
      // Diagnostic samples use absolute row indices; a jump while filtered
      // would land on the wrong (display) row and the target may be hidden
      // anyway, so drop the filter first.
      if (meta.filtered) {
        try {
          const updated = await api.clearFilter(meta.id);
          reloadDoc(updated);
        } catch (e) {
          set({ error: String(e) });
          return;
        }
      }
      jumpNonce += 1;
      set({ jumpTarget: { row, col, nonce: jumpNonce } });
    },

    // ----- background-job events -------------------------------------------

    handleJobProgress: (progress) => {
      if (progress.kind !== "diagnostics" || progress.docId == null) return;
      const docId = progress.docId;
      // Only track the scan we started (guards against reused ids after e.g.
      // an app-side restart of the job system).
      if (get().diagnostics[docId]?.jobId !== progress.jobId) return;
      patchDiagnostics(docId, { processed: progress.processed, total: progress.total });
    },

    handleJobFinished: async (finished) => {
      if (finished.kind !== "diagnostics" || finished.docId == null) return;
      const docId = finished.docId;
      if (get().diagnostics[docId]?.jobId !== finished.jobId) return;
      if (finished.status === "done") {
        const report = await api.getDiagnostics(docId).catch(() => null);
        patchDiagnostics(docId, { jobId: null, report, scanError: null });
      } else {
        patchDiagnostics(docId, {
          jobId: null,
          scanError: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
        });
      }
    },
  };
});

/** Convenience selector for the active document's metadata. */
export function useActiveMeta(): DocumentMeta | null {
  return useStore((s) => s.tabs.find((t) => t.id === s.activeId) ?? null);
}
