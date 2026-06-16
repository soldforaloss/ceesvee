import {
  CompactSelection,
  DataEditor,
  GridCellKind,
  type DataEditorRef,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Rectangle,
} from "@glideapps/glide-data-grid";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { darkGridTheme, dirtyCellOverride, lightGridTheme } from "../lib/gridTheme";
import * as api from "../lib/tauri";
import { useStore } from "../store/useStore";
import type { DocumentMeta } from "../types";
import { ColumnMenu, type ColumnMenuState } from "./ColumnMenu";

const PAGE = 200;
const DEFAULT_COL_WIDTH = 160;

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

  const [colWidths, setColWidths] = useState<Record<number, number>>({});
  const [selection, setSelectionState] = useState<GridSelection>({
    columns: CompactSelection.empty(),
    rows: CompactSelection.empty(),
  });
  const [menu, setMenu] = useState<ColumnMenuState | null>(null);

  const findMatches = useStore((s) => s.find.matches);
  const findIndex = useStore((s) => s.find.index);

  // ----- columns ----------------------------------------------------------

  const columns = useMemo<GridColumn[]>(
    () =>
      meta.headers.map((title, i) => ({
        title: title || `Column ${i + 1}`,
        id: String(i),
        width: colWidths[i] ?? DEFAULT_COL_WIDTH,
        hasMenu: true,
      })),
    [meta.headers, colWidths],
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

  const onVisibleRegionChanged = useCallback(
    (range: Rectangle) => {
      visibleRegion.current = range;
      loadRange(Math.max(0, range.y - PAGE), range.height + 2 * PAGE);
    },
    [loadRange],
  );

  // Column widths are keyed by position, so reset them when columns are
  // inserted/removed to avoid a width sticking to the wrong column.
  const prevColCount = useRef(colCount);
  useEffect(() => {
    if (prevColCount.current !== colCount) {
      setColWidths({});
      prevColCount.current = colCount;
    }
  }, [colCount]);

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
        allowOverlay: true,
        themeOverride: isDirty ? dirtyCellOverride : undefined,
      };
    },
    // Reads only refs; structural refreshes go through `updateCells`.
    [],
  );

  const onCellEdited = useCallback(([col, row]: Item, newValue: EditableGridCell) => {
    if (newValue.kind !== GridCellKind.Text) return;
    const value = newValue.data;
    const rowData = rowCache.current.get(row);
    if (rowData) rowData[col] = value;
    const dirtyRow = dirtyCache.current.get(row);
    if (dirtyRow) dirtyRow[col] = true;
    gridRef.current?.updateCells([{ cell: [col, row] }]);
    void useStore.getState().setCell(row, col, value);
  }, []);

  const onPaste = useCallback((target: Item, values: readonly (readonly string[])[]) => {
    const [col, row] = target;
    const block = values.map((line) => Array.from(line));
    void useStore.getState().pasteBlock(row, col, block);
    return false; // applied via the backend, which triggers a reload
  }, []);

  const onColumnResize = useCallback((_col: GridColumn, newSize: number, colIndex: number) => {
    setColWidths((prev) => ({ ...prev, [colIndex]: newSize }));
  }, []);

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
  }, [findMatches, findIndex]);

  return (
    <div className="gdg-wrapper">
      <DataEditor
        ref={gridRef}
        theme={dark ? darkGridTheme : lightGridTheme}
        columns={columns}
        rows={meta.rowCount}
        getCellContent={getCellContent}
        onCellEdited={onCellEdited}
        onPaste={onPaste}
        onColumnResize={onColumnResize}
        onVisibleRegionChanged={onVisibleRegionChanged}
        onHeaderMenuClick={onHeaderMenuClick}
        gridSelection={selection}
        onGridSelectionChange={onGridSelectionChange}
        getCellsForSelection={getCellsForSelection}
        rowMarkers="both"
        rangeSelect="multi-rect"
        columnSelect="multi"
        rowSelect="multi"
        smoothScrollX
        smoothScrollY
        fillHandle
        keybindings={{ search: false }}
        width="100%"
        height="100%"
      />
      {menu && <ColumnMenu state={menu} headers={meta.headers} onClose={() => setMenu(null)} />}
    </div>
  );
}

function rectOf(range: Rectangle) {
  return { x: range.x, y: range.y, width: range.width, height: range.height };
}
