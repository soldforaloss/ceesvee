import { create } from "zustand";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";

import * as api from "../lib/tauri";
import { applyReplace } from "../lib/replace";
import type {
  CellRect,
  DocumentMeta,
  ExportOptions,
  FindMatch,
  FindOptions,
  OpenOptions,
  SortKey,
} from "../types";

export type ThemePref = "light" | "dark" | "system";

const RECENT_KEY = "ceesvee-recent";
const THEME_KEY = "ceesvee-theme";
const MAX_RECENT = 10;

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

  // lifecycle / chrome
  init: () => void;
  setTheme: (theme: ThemePref) => void;
  setError: (error: string | null) => void;
  setActive: (id: number) => void;
  setSelection: (
    info: SelectionInfo | null,
    rect: CellRect | null,
    rows: number[],
    cols: number[],
  ) => void;

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
      }),

    setSelection: (info, rect, rows, cols) =>
      set({ selection: info, selectionRect: rect, selectedRows: rows, selectedCols: cols }),

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
        const activeId =
          s.activeId === id ? (tabs.length ? tabs[tabs.length - 1].id : null) : s.activeId;
        return { tabs, activeId, dataVersion: s.dataVersion + 1 };
      });
    },

    reparse: async (options) => {
      const id = get().activeId;
      if (id == null) return;
      set({ busy: true });
      try {
        const meta = await api.reparse(id, options);
        reloadDoc(meta);
        set({ busy: false });
      } catch (e) {
        set({ error: String(e), busy: false });
      }
    },

    setHeaderMode: (hasHeader) => mutate((id) => api.setHeaderMode(id, hasHeader)),

    setCell: (row, col, value) => mutate((id) => api.setCell(id, row, col, value), false),
    pasteBlock: (row, col, block) => mutate((id) => api.paste(id, row, col, block)),
    insertRows: (at, count) => mutate((id) => api.insertRows(id, at, count)),
    deleteRows: (indices) => mutate((id) => api.deleteRows(id, indices)),
    moveRow: (from, to) => mutate((id) => api.moveRow(id, from, to)),
    insertColumn: (at, name) => mutate((id) => api.insertColumn(id, at, name)),
    deleteColumns: (indices) => mutate((id) => api.deleteColumns(id, indices)),
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
      const { find } = get();
      const match = find.matches[find.index];
      if (id == null || !match) return;
      try {
        const window = await api.getRows(id, match.row, 1);
        const current = window.rows[0]?.[match.col] ?? "";
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
        await get().runFind();
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
  };
});

/** Convenience selector for the active document's metadata. */
export function useActiveMeta(): DocumentMeta | null {
  return useStore((s) => s.tabs.find((t) => t.id === s.activeId) ?? null);
}
