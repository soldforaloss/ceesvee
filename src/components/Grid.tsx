import {
  CompactSelection,
  DataEditor,
  GridCellKind,
  type CellClickedEventArgs,
  type DataEditorRef,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Rectangle,
} from "@glideapps/glide-data-grid";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { indicesToRanges } from "../lib/gridSelection";
import { darkGridTheme, dirtyCellOverride, lightGridTheme } from "../lib/gridTheme";
import * as api from "../lib/tauri";
import { useStore } from "../store/useStore";
import type { ColumnKind, DocumentMeta } from "../types";
import { ColumnMenu, type ColumnMenuState } from "./ColumnMenu";

const PAGE = 200;
const DEFAULT_COL_WIDTH = 160;

// Header type-badge sprites (Feather-style), rendered by glide-data-grid next
// to a column's title once its type has been detected. Text columns get none.
const HEADER_ICONS: Record<string, (p: { fgColor: string }) => string> = {
  ceesveeNumber: (p) =>
    `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="${p.fgColor}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="4" y1="9" x2="20" y2="9"/><line x1="4" y1="15" x2="20" y2="15"/><line x1="10" y1="3" x2="8" y2="21"/><line x1="16" y1="3" x2="14" y2="21"/></svg>`,
  ceesveeDate: (p) =>
    `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="${p.fgColor}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="18" rx="2"/><line x1="16" y1="2" x2="16" y2="6"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="3" y1="10" x2="21" y2="10"/></svg>`,
  ceesveeBool: (p) =>
    `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="${p.fgColor}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`,
};

function iconForKind(kind: ColumnKind): string | undefined {
  switch (kind) {
    case "number":
      return "ceesveeNumber";
    case "date":
      return "ceesveeDate";
    case "bool":
      return "ceesveeBool";
    default:
      return undefined;
  }
}

interface GridProps {
  meta: DocumentMeta;
  dataVersion: number;
  dark: boolean;
}

export function Grid({ meta, dataVersion, dark }: GridProps) {
  const gridRef = useRef<DataEditorRef>(null);
  const rowCache = useRef<Map<number, string[]>>(new Map());
  const dirtyCache = useRef<Map<number, boolean[]>>(new Map());
  const inFlight = useRef<Set<number>>(new Set());
  const visibleRegion = useRef<Rectangle | null>(null);
  // Bumped on every cache invalidation; an in-flight fetch from a previous
  // generation must not write its (now-stale) rows into the cleared cache.
  const generation = useRef(0);

  // Latest values for use inside stable callbacks.
  const docId = meta.id;
  const colCount = meta.colCount;
  const docIdRef = useRef(docId);
  const colCountRef = useRef(colCount);
  docIdRef.current = docId;
  colCountRef.current = colCount;

  const [selection, setSelectionState] = useState<GridSelection>({
    columns: CompactSelection.empty(),
    rows: CompactSelection.empty(),
  });
  const [menu, setMenu] = useState<ColumnMenuState | null>(null);
  // F13: right-click cell menu (edit in the multiline editor / copy full value).
  const [cellMenu, setCellMenu] = useState<{
    row: number;
    col: number;
    x: number;
    y: number;
  } | null>(null);

  const findMatches = useStore((s) => s.find.matches);
  const findIndex = useStore((s) => s.find.index);
  // Per-document UI state (F08), saved/restored by the store on tab switches.
  const colWidths = useStore((s) => s.columnWidths);
  const setColumnWidth = useStore((s) => s.setColumnWidth);
  const resetColumnWidths = useStore((s) => s.resetColumnWidths);
  const frozenCols = useStore((s) => s.frozenColumnCount);
  const setScrollPosition = useStore((s) => s.setScrollPosition);
  const summaries = useStore((s) => (s.summariesDocId === docId ? s.summaries : null));
  const jumpTarget = useStore((s) => s.jumpTarget);

  // Detected type per column (defaults to text until summaries load).
  const columnKinds = useMemo<ColumnKind[]>(() => {
    const kinds: ColumnKind[] = new Array(colCount).fill("text");
    if (summaries) {
      for (const cs of summaries) if (cs.column < colCount) kinds[cs.column] = cs.kind;
    }
    return kinds;
  }, [summaries, colCount]);

  // ----- columns ----------------------------------------------------------

  const columns = useMemo<GridColumn[]>(
    () =>
      meta.headers.map((title, i) => ({
        title: title || `Column ${i + 1}`,
        id: String(i),
        width: colWidths[i] ?? DEFAULT_COL_WIDTH,
        hasMenu: true,
        icon: iconForKind(columnKinds[i] ?? "text"),
      })),
    [meta.headers, colWidths, columnKinds],
  );

  // ----- windowed data fetching ------------------------------------------

  const loadPage = useCallback(async (page: number) => {
    const id = docIdRef.current;
    const gen = generation.current;
    const startRow = page * PAGE;
    if (inFlight.current.has(page) || rowCache.current.has(startRow)) return;
    inFlight.current.add(page);
    try {
      const resp = await api.getRows(id, startRow, PAGE);
      // The document was invalidated while this fetch was in flight — drop it.
      if (gen !== generation.current) return;
      for (let i = 0; i < resp.rows.length; i++) {
        rowCache.current.set(resp.start + i, resp.rows[i]);
        dirtyCache.current.set(resp.start + i, resp.dirty[i]);
      }
      const updates: { cell: Item }[] = [];
      const cols = colCountRef.current;
      for (let i = 0; i < resp.rows.length; i++) {
        const r = resp.start + i;
        for (let c = 0; c < cols; c++) updates.push({ cell: [c, r] });
      }
      gridRef.current?.updateCells(updates);
    } catch (e) {
      useStore.getState().setError(String(e));
    } finally {
      inFlight.current.delete(page);
    }
  }, []);

  const loadRange = useCallback(
    (startRow: number, rowCount: number) => {
      const firstPage = Math.max(0, Math.floor(startRow / PAGE));
      // -1 so an exact-multiple range doesn't pull in the page just past it.
      const lastPage = Math.floor((startRow + Math.max(1, rowCount) - 1) / PAGE);
      for (let p = firstPage; p <= lastPage; p++) void loadPage(p);
    },
    [loadPage],
  );

  // Invalidate the cache when the document or its data changes structurally,
  // then refetch the visible window (loadPage's `updateCells` repaints them).
  useEffect(() => {
    generation.current += 1;
    rowCache.current.clear();
    dirtyCache.current.clear();
    inFlight.current.clear();
    const region = visibleRegion.current;
    const start = region ? Math.max(0, region.y - PAGE) : 0;
    const count = region ? region.height + 2 * PAGE : PAGE;
    loadRange(start, count);
  }, [docId, dataVersion, loadRange]);

  // (Re)detect column types + summaries whenever the document changes
  // structurally. Debounced in the store; results drive header badges,
  // numeric alignment, and the summaries panel.
  useEffect(() => {
    useStore.getState().loadSummaries();
  }, [docId, dataVersion]);

  const onVisibleRegionChanged = useCallback(
    (range: Rectangle) => {
      visibleRegion.current = range;
      loadRange(Math.max(0, range.y - PAGE), range.height + 2 * PAGE);
      // Persist (debounced) so the position survives tab switches (F08).
      setScrollPosition(Math.max(0, range.y), Math.max(0, range.x));
    },
    [loadRange, setScrollPosition],
  );

  // Column widths are keyed by position, so reset them when THIS document's
  // column count changes (insert/remove); a tab switch restores the other
  // document's widths instead.
  const prevColCount = useRef(colCount);
  const prevDocId = useRef(docId);
  useEffect(() => {
    if (prevDocId.current === docId && prevColCount.current !== colCount) {
      resetColumnWidths();
    }
    prevDocId.current = docId;
    prevColCount.current = colCount;
  }, [docId, colCount, resetColumnWidths]);

  // Restore the saved scroll position and selection after a tab switch.
  useEffect(() => {
    const s = useStore.getState();
    const pos = s.scrollPosition;
    const rect = s.selectionRect;

    let rows = CompactSelection.empty();
    for (const [start, end] of indicesToRanges(s.selectedRows)) rows = rows.add([start, end]);
    let cols = CompactSelection.empty();
    for (const [start, end] of indicesToRanges(s.selectedCols)) cols = cols.add([start, end]);
    setSelectionState({
      columns: cols,
      rows,
      current: rect
        ? {
            cell: [rect.x, rect.y],
            range: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
            rangeStack: [],
          }
        : undefined,
    });

    if (pos.row > 0 || pos.column > 0) {
      // Next frame: the editor has swapped to the new document by then.
      requestAnimationFrame(() => {
        gridRef.current?.scrollTo(pos.column, pos.row, "both", 0, 0);
      });
    }
    // Reads go through getState() on purpose: this must run only when the
    // document switches, not on every scroll/selection change.
  }, [docId]);

  // Copy/fill must read the FULL selected range from the backend, not the
  // windowed cache (off-screen rows aren't cached and would copy as blanks).
  const getCellsForSelection = useCallback((sel: Rectangle) => {
    const id = docIdRef.current;
    return async (): Promise<readonly (readonly GridCell[])[]> => {
      const resp = await api.getRows(id, sel.y, sel.height);
      const out: GridCell[][] = [];
      for (let r = 0; r < sel.height; r++) {
        const rowData = resp.rows[r];
        const cells: GridCell[] = [];
        for (let c = sel.x; c < sel.x + sel.width; c++) {
          const value = rowData?.[c] ?? "";
          cells.push({
            kind: GridCellKind.Text,
            data: value,
            displayData: value,
            allowOverlay: true,
          });
        }
        out.push(cells);
      }
      return out;
    };
  }, []);

  // ----- cell rendering & editing ----------------------------------------

  // Indexed read-only documents (F10): cells render but never open an editor,
  // and paste/fill are inert (the backend refuses them anyway).
  const readOnly = meta.backing === "indexedReadOnly";

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const rowData = rowCache.current.get(row);
      if (!rowData) {
        return { kind: GridCellKind.Loading, allowOverlay: false };
      }
      const value = rowData[col] ?? "";
      const isDirty = dirtyCache.current.get(row)?.[col] ?? false;
      return {
        kind: GridCellKind.Text,
        data: value,
        displayData: value,
        allowOverlay: !readOnly,
        contentAlign: columnKinds[col] === "number" ? "right" : undefined,
        themeOverride: isDirty ? dirtyCellOverride : undefined,
      };
    },
    // Cell data is read from refs; recreated only when column types change so
    // numeric columns re-render right-aligned. Structural refreshes still go
    // through `updateCells`.
    [columnKinds, readOnly],
  );

  const onCellEdited = useCallback(
    ([col, row]: Item, newValue: EditableGridCell) => {
      if (readOnly) return;
      if (newValue.kind !== GridCellKind.Text) return;
      const value = newValue.data;
      const rowData = rowCache.current.get(row);
      if (rowData) rowData[col] = value;
      const dirtyRow = dirtyCache.current.get(row);
      if (dirtyRow) dirtyRow[col] = true;
      gridRef.current?.updateCells([{ cell: [col, row] }]);
      void useStore.getState().setCell(row, col, value);
    },
    [readOnly],
  );

  const onPaste = useCallback(
    (target: Item, values: readonly (readonly string[])[]) => {
      if (readOnly) return false;
      const [col, row] = target;
      const block = values.map((line) => Array.from(line));
      void useStore.getState().pasteBlock(row, col, block);
      return false; // applied via the backend, which triggers a reload
    },
    [readOnly],
  );

  const onColumnResize = useCallback(
    (_col: GridColumn, newSize: number, colIndex: number) => {
      setColumnWidth(colIndex, newSize);
    },
    [setColumnWidth],
  );

  const onGridSelectionChange = useCallback((next: GridSelection) => {
    setSelectionState(next);
    const range = next.current?.range;

    // Selected rows/columns combine marker selections and the active range.
    const rowsSel = new Set<number>(next.rows.toArray());
    const colsSel = new Set<number>(next.columns.toArray());
    if (range) {
      for (let r = range.y; r < range.y + range.height; r++) rowsSel.add(r);
      for (let c = range.x; c < range.x + range.width; c++) colsSel.add(c);
    }
    const rows = [...rowsSel].sort((a, b) => a - b);
    const cols = [...colsSel].sort((a, b) => a - b);

    // Stats are computed in Rust over the full range (see the store).
    useStore.getState().setSelection(range ? rectOf(range) : null, rows, cols);
  }, []);

  // ----- header menu (column operations) ---------------------------------

  const onHeaderMenuClick = useCallback((col: number, bounds: Rectangle) => {
    setMenu({ col, x: bounds.x, y: bounds.y + bounds.height });
  }, []);

  const onCellContextMenu = useCallback((cell: Item, event: CellClickedEventArgs) => {
    const [col, row] = cell;
    if (row < 0 || col < 0) return;
    event.preventDefault();
    setCellMenu({
      row,
      col,
      x: event.bounds.x + event.localEventX,
      y: event.bounds.y + event.localEventY,
    });
  }, []);

  // ----- scroll to the active find match ---------------------------------

  useEffect(() => {
    if (findMatches.length === 0) return;
    const match = findMatches[findIndex];
    if (!match) return;
    gridRef.current?.scrollTo(match.col, match.row, "both", 0, 0, {
      vAlign: "center",
      hAlign: "center",
    });
    setSelectionState({
      columns: CompactSelection.empty(),
      rows: CompactSelection.empty(),
      current: {
        cell: [match.col, match.row],
        range: { x: match.col, y: match.row, width: 1, height: 1 },
        rangeStack: [],
      },
    });
    // Mirror into the store so selection-driven commands (F13 cell editor)
    // target the cell that is actually highlighted.
    useStore.getState().setSelection({ x: match.col, y: match.row, width: 1, height: 1 }, [], []);
  }, [findMatches, findIndex]);

  // ----- jump requests (e.g. a diagnostics sample) ------------------------

  useEffect(() => {
    if (!jumpTarget) return;
    const row = Math.min(jumpTarget.row, Math.max(0, meta.rowCount - 1));
    const col = Math.min(jumpTarget.col, Math.max(0, colCount - 1));
    gridRef.current?.scrollTo(col, row, "both", 0, 0, {
      vAlign: "center",
      hAlign: "center",
    });
    setSelectionState({
      columns: CompactSelection.empty(),
      rows: CompactSelection.empty(),
      current: {
        cell: [col, row],
        range: { x: col, y: row, width: 1, height: 1 },
        rangeStack: [],
      },
    });
    // Mirror into the store so selection-driven commands (F13 cell editor)
    // target the cell that is actually highlighted.
    useStore.getState().setSelection({ x: col, y: row, width: 1, height: 1 }, [], []);
    // Depend on the nonce so repeated jumps to the same cell still scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jumpTarget?.nonce]);

  return (
    <div className="gdg-wrapper">
      <DataEditor
        ref={gridRef}
        theme={dark ? darkGridTheme : lightGridTheme}
        columns={columns}
        headerIcons={HEADER_ICONS}
        rows={meta.rowCount}
        freezeColumns={Math.min(frozenCols, Math.max(0, colCount - 1))}
        getCellContent={getCellContent}
        onCellEdited={onCellEdited}
        onPaste={onPaste}
        onColumnResize={onColumnResize}
        onVisibleRegionChanged={onVisibleRegionChanged}
        onHeaderMenuClick={onHeaderMenuClick}
        onCellContextMenu={onCellContextMenu}
        gridSelection={selection}
        onGridSelectionChange={onGridSelectionChange}
        getCellsForSelection={getCellsForSelection}
        rowMarkers="both"
        rangeSelect="multi-rect"
        columnSelect="multi"
        rowSelect="multi"
        smoothScrollX
        smoothScrollY
        fillHandle={!readOnly}
        keybindings={{ search: false }}
        width="100%"
        height="100%"
      />
      {menu && (
        <ColumnMenu
          state={menu}
          headers={meta.headers}
          readOnly={readOnly}
          onClose={() => setMenu(null)}
        />
      )}
      {cellMenu && (
        <CellContextMenu
          state={cellMenu}
          docId={meta.id}
          readOnly={readOnly}
          onClose={() => setCellMenu(null)}
        />
      )}
    </div>
  );
}

function rectOf(range: Rectangle) {
  return { x: range.x, y: range.y, width: range.width, height: range.height };
}

/** Right-click cell menu (F13): open the multiline editor or copy the FULL
 * value (fetched from Rust — the grid cache may hold only visible rows). */
function CellContextMenu({
  state,
  docId,
  readOnly,
  onClose,
}: {
  state: { row: number; col: number; x: number; y: number };
  docId: number;
  readOnly: boolean;
  onClose: () => void;
}) {
  const openCellEditor = useStore((s) => s.openCellEditor);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const copyFull = async () => {
    try {
      const value = await api.getCell(docId, state.row, state.col);
      await navigator.clipboard.writeText(value);
    } catch {
      // Clipboard/read failures surface elsewhere; the menu just closes.
    }
    onClose();
  };

  const item =
    "block w-full px-3 py-1.5 text-left text-sm text-zinc-700 hover:bg-zinc-100 dark:text-zinc-200 dark:hover:bg-zinc-700";

  return (
    <div
      ref={ref}
      className="fixed z-50 w-44 overflow-hidden rounded-lg border border-zinc-200 bg-white py-1 shadow-xl dark:border-zinc-700 dark:bg-zinc-800"
      style={{ left: state.x, top: state.y }}
    >
      <button
        className={item}
        onClick={() => {
          openCellEditor(state.row, state.col);
          onClose();
        }}
      >
        {readOnly ? "Inspect cell…" : "Edit cell…"}
      </button>
      <button className={item} onClick={() => void copyFull()}>
        Copy full value
      </button>
    </div>
  );
}
