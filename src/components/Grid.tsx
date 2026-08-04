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
import { formatCellValue, isNumericType } from "../lib/schema";
import * as api from "../lib/tauri";
import { physicalToDisplay, projectColumns } from "../lib/viewProjection";
import { useStore } from "../store/useStore";
import type { ColumnKind, ColumnSchema, DocumentMeta, LogicalType } from "../types";
import { ColumnMenu, type ColumnMenuState } from "./ColumnMenu";

const PAGE = 200;
const DEFAULT_COL_WIDTH = 160;
/** Grid row heights: default, and taller when wrap text (F12) is on. */
const ROW_HEIGHT = 34;
const WRAP_ROW_HEIGHT = 68;
/** Auto-fit (F12) measures the cached sample and clamps to these bounds. */
const AUTOFIT_MIN = 56;
const AUTOFIT_MAX = 640;
const AUTOFIT_PADDING = 26;

// Header type-badge sprites (Feather-style), rendered by glide-data-grid next
// to a column's title. The `ceesvee*` sprites badge a HEURISTICALLY DETECTED
// type (theme-coloured); the `ceesveeSchema*` sprites badge an EXPLICITLY
// DECLARED F31 logical type and are drawn in a fixed violet so a declaration
// reads distinctly from — and visually wins over — detection.
const DECLARED = "#8b5cf6"; // violet-500: "this type was declared, not guessed"

const glyph = (body: (stroke: string) => string) => (p: { fgColor: string }) =>
  `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body(
    p.fgColor,
  )}</svg>`;

const NUMBER_BODY = (s: string) =>
  `<g stroke="${s}"><line x1="4" y1="9" x2="20" y2="9"/><line x1="4" y1="15" x2="20" y2="15"/><line x1="10" y1="3" x2="8" y2="21"/><line x1="16" y1="3" x2="14" y2="21"/></g>`;
const DATE_BODY = (s: string) =>
  `<g stroke="${s}"><rect x="3" y="4" width="18" height="18" rx="2"/><line x1="16" y1="2" x2="16" y2="6"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="3" y1="10" x2="21" y2="10"/></g>`;
const BOOL_BODY = (s: string) => `<polyline points="20 6 9 17 4 12" stroke="${s}"/>`;
const TEXT_BODY = (s: string) =>
  `<g stroke="${s}"><path d="M4 7V4h16v3"/><line x1="12" y1="4" x2="12" y2="20"/><line x1="8" y1="20" x2="16" y2="20"/></g>`;
const UUID_BODY = (s: string) =>
  `<g stroke="${s}"><circle cx="8" cy="15" r="4"/><line x1="10.85" y1="12.15" x2="19" y2="4"/><line x1="18" y1="5" x2="20" y2="7"/><line x1="15" y1="8" x2="17" y2="10"/></g>`;
const JSON_BODY = (s: string) =>
  `<g stroke="${s}"><path d="M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5a2 2 0 0 0 2 2h1"/><path d="M16 3h1a2 2 0 0 1 2 2v5a2 2 0 0 1 2 2 2 2 0 0 1-2 2v5a2 2 0 0 1-2 2h-1"/></g>`;

const HEADER_ICONS: Record<string, (p: { fgColor: string }) => string> = {
  ceesveeNumber: glyph(NUMBER_BODY),
  ceesveeDate: glyph(DATE_BODY),
  ceesveeBool: glyph(BOOL_BODY),
  // Declared (F31): fixed violet, one per logical-type family.
  ceesveeSchemaNumber: glyph(() => NUMBER_BODY(DECLARED)),
  ceesveeSchemaDate: glyph(() => DATE_BODY(DECLARED)),
  ceesveeSchemaBool: glyph(() => BOOL_BODY(DECLARED)),
  ceesveeSchemaText: glyph(() => TEXT_BODY(DECLARED)),
  ceesveeSchemaUuid: glyph(() => UUID_BODY(DECLARED)),
  ceesveeSchemaJson: glyph(() => JSON_BODY(DECLARED)),
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

/** The declared-schema badge sprite for a logical type (violet, F31). */
function iconForLogicalType(lt: LogicalType): string {
  switch (lt) {
    case "integer":
    case "decimal":
    case "float":
      return "ceesveeSchemaNumber";
    case "date":
    case "datetime":
      return "ceesveeSchemaDate";
    case "boolean":
      return "ceesveeSchemaBool";
    case "uuid":
      return "ceesveeSchemaUuid";
    case "json":
      return "ceesveeSchemaJson";
    default:
      return "ceesveeSchemaText";
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
  // F12: column layout (hidden/pinned/order by stable ID), wrap, auto-fit.
  const columnLayout = useStore((s) => s.columnLayout);
  const wrapText = useStore((s) => s.wrapText);
  const autoFitRequest = useStore((s) => s.autoFitRequest);

  // Detected type per column (defaults to text until summaries load).
  const columnKinds = useMemo<ColumnKind[]>(() => {
    const kinds: ColumnKind[] = new Array(colCount).fill("text");
    if (summaries) {
      for (const cs of summaries) if (cs.column < colCount) kinds[cs.column] = cs.kind;
    }
    return kinds;
  }, [summaries, colCount]);

  // F31: the active document's declared schema, by physical column. A declared
  // logical type wins the header badge over detection and drives display-only
  // formatting + numeric alignment. Keyed by stable column ID so it stays put
  // across reorders.
  const schemaColumns = useStore((s) => s.schemaInfo?.schema.columns ?? null);
  const columnSchemas = useMemo<(ColumnSchema | undefined)[]>(() => {
    const arr: (ColumnSchema | undefined)[] = new Array(colCount).fill(undefined);
    if (schemaColumns) {
      for (let phys = 0; phys < colCount; phys++) {
        const id = meta.columnIds[phys];
        if (id) arr[phys] = schemaColumns[id];
      }
    }
    return arr;
  }, [schemaColumns, meta.columnIds, colCount]);

  // ----- columns (through the F12 view projection) -------------------------

  // DISPLAY position -> PHYSICAL column index. Everything the grid renders or
  // reports is translated at this boundary; the store and backend always see
  // physical columns (rows stay in display space — the backend maps those).
  const projection = useMemo(
    () => projectColumns(meta.columnIds, columnLayout),
    [meta.columnIds, columnLayout],
  );
  const projectionRef = useRef(projection);
  projectionRef.current = projection;

  const columns = useMemo<GridColumn[]>(
    () =>
      projection.physical.map((phys) => {
        const schema = columnSchemas[phys];
        return {
          title: meta.headers[phys] || `Column ${phys + 1}`,
          id: meta.columnIds[phys] ?? String(phys),
          width: colWidths[phys] ?? DEFAULT_COL_WIDTH,
          hasMenu: true,
          // Declared logical type wins over the detected-kind badge (F31).
          icon: schema
            ? iconForLogicalType(schema.logicalType)
            : iconForKind(columnKinds[phys] ?? "text"),
        };
      }),
    [meta.headers, meta.columnIds, colWidths, columnKinds, columnSchemas, projection],
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
      // Repaints happen in DISPLAY coordinates (the projection's width).
      const cols = projectionRef.current.physical.length;
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
  // numeric alignment, and the summaries panel. The F31 schema is refetched
  // alongside so declared badges + display formatting track structural edits.
  useEffect(() => {
    useStore.getState().loadSummaries();
    void useStore.getState().loadSchema();
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
    // Stored column selections are PHYSICAL; the grid needs display indices.
    const displayCols = s.selectedCols
      .map((c) => physicalToDisplay(projectionRef.current, c))
      .filter((c): c is number => c !== null)
      .sort((a, b) => a - b);
    let cols = CompactSelection.empty();
    for (const [start, end] of indicesToRanges(displayCols)) cols = cols.add([start, end]);
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
  // Columns pass through the projection, so copies respect the view order.
  const getCellsForSelection = useCallback((sel: Rectangle) => {
    const id = docIdRef.current;
    return async (): Promise<readonly (readonly GridCell[])[]> => {
      const resp = await api.getRows(id, sel.y, sel.height);
      const physical = projectionRef.current.physical;
      const out: GridCell[][] = [];
      for (let r = 0; r < sel.height; r++) {
        const rowData = resp.rows[r];
        const cells: GridCell[] = [];
        for (let c = sel.x; c < sel.x + sel.width; c++) {
          const value = rowData?.[physical[c] ?? c] ?? "";
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
      const phys = projection.physical[col] ?? col;
      const value = rowData[phys] ?? "";
      const isDirty = dirtyCache.current.get(row)?.[phys] ?? false;
      // F31: a declared display format changes only what is SHOWN. `data`
      // stays the raw stored text, so the overlay editor and copy/fill always
      // see and write the unformatted value; numeric alignment prefers the
      // declared logical type over heuristic detection.
      const schema = columnSchemas[phys];
      const displayData = schema ? formatCellValue(schema, value) : value;
      const numeric = schema ? isNumericType(schema.logicalType) : columnKinds[phys] === "number";
      return {
        kind: GridCellKind.Text,
        data: value,
        displayData,
        allowOverlay: !readOnly,
        allowWrapping: wrapText,
        contentAlign: numeric ? "right" : undefined,
        themeOverride: isDirty ? dirtyCellOverride : undefined,
      };
    },
    // Cell data is read from refs; recreated when column types, schema, wrap
    // or the projection change so cells re-render correctly. Structural
    // refreshes still go through `updateCells`.
    [columnKinds, columnSchemas, readOnly, wrapText, projection],
  );

  const onCellEdited = useCallback(
    ([col, row]: Item, newValue: EditableGridCell) => {
      if (readOnly) return;
      if (newValue.kind !== GridCellKind.Text) return;
      const value = newValue.data;
      // The backend expects the PHYSICAL column (rows are display-space and
      // translate through the backend's row view) — so an edit made in a
      // sorted/filtered, reordered view lands on the correct source cell.
      const phys = projectionRef.current.physical[col] ?? col;
      const rowData = rowCache.current.get(row);
      if (rowData) rowData[phys] = value;
      const dirtyRow = dirtyCache.current.get(row);
      if (dirtyRow) dirtyRow[phys] = true;
      gridRef.current?.updateCells([{ cell: [col, row] }]);
      void useStore.getState().setCell(row, phys, value);
    },
    [readOnly],
  );

  const onPaste = useCallback(
    (target: Item, values: readonly (readonly string[])[]) => {
      if (readOnly) return false;
      const [col, row] = target;
      const phys = projectionRef.current.physical[col] ?? col;
      const block = values.map((line) => Array.from(line));
      void useStore.getState().pasteBlock(row, phys, block);
      return false; // applied via the backend, which triggers a reload
    },
    [readOnly],
  );

  const onColumnResize = useCallback(
    (_col: GridColumn, newSize: number, colIndex: number) => {
      // Widths are stored by PHYSICAL column so they survive reorders.
      setColumnWidth(projectionRef.current.physical[colIndex] ?? colIndex, newSize);
    },
    [setColumnWidth],
  );

  // F12: drag-reorder updates the layout (display-space move; the store
  // translates it into the ID-based column order).
  const onColumnMoved = useCallback((from: number, to: number) => {
    useStore.getState().reorderColumns(from, to);
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
    const physical = projectionRef.current.physical;
    const rows = [...rowsSel].sort((a, b) => a - b);
    // The store (and every consumer: transforms, exports, Copy As) sees
    // PHYSICAL columns; the rect stays display-space for grid restore and is
    // translated per consumer.
    const cols = [...colsSel].map((c) => physical[c] ?? c).sort((a, b) => a - b);

    // Stats are computed in Rust over the full range (see the store).
    useStore.getState().setSelection(range ? rectOf(range) : null, rows, cols);
  }, []);

  // ----- header menu (column operations) ---------------------------------

  const onHeaderMenuClick = useCallback((col: number, bounds: Rectangle) => {
    // The menu operates on the PHYSICAL column.
    const phys = projectionRef.current.physical[col] ?? col;
    setMenu({ col: phys, x: bounds.x, y: bounds.y + bounds.height });
  }, []);

  const onCellContextMenu = useCallback((cell: Item, event: CellClickedEventArgs) => {
    const [col, row] = cell;
    if (row < 0 || col < 0) return;
    event.preventDefault();
    setCellMenu({
      row,
      col: projectionRef.current.physical[col] ?? col,
      x: event.bounds.x + event.localEventX,
      y: event.bounds.y + event.localEventY,
    });
  }, []);

  // ----- scroll to the active find match ---------------------------------

  useEffect(() => {
    if (findMatches.length === 0) return;
    const match = findMatches[findIndex];
    if (!match) return;
    // Matches carry PHYSICAL columns; scroll in display space. A match in a
    // hidden column still scrolls its row into view (column 0).
    const display = physicalToDisplay(projectionRef.current, match.col) ?? 0;
    gridRef.current?.scrollTo(display, match.row, "both", 0, 0, {
      vAlign: "center",
      hAlign: "center",
    });
    setSelectionState({
      columns: CompactSelection.empty(),
      rows: CompactSelection.empty(),
      current: {
        cell: [display, match.row],
        range: { x: display, y: match.row, width: 1, height: 1 },
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
    const physCol = Math.min(jumpTarget.col, Math.max(0, colCount - 1));
    // Jump targets carry PHYSICAL columns; translate for the display.
    const col = physicalToDisplay(projectionRef.current, physCol) ?? 0;
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

  // ----- auto-fit column widths (F12) -------------------------------------

  useEffect(() => {
    if (!autoFitRequest) return;
    const state = useStore.getState();
    const targets =
      autoFitRequest.cols === "all" ? projectionRef.current.physical : autoFitRequest.cols;
    if (targets.length === 0) {
      state.clearAutoFitRequest();
      return;
    }
    // Measure the header plus the CACHED sample (the rows around the visible
    // window) — never the whole document. Consistent with the grid font.
    const canvas = document.createElement("canvas");
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      state.clearAutoFitRequest();
      return;
    }
    ctx.font = "13px Inter, ui-sans-serif, system-ui, sans-serif";
    const widths: Record<number, number> = {};
    for (const phys of targets) {
      let max = ctx.measureText(meta.headers[phys] ?? "").width + 28; // menu chevron
      let sampled = 0;
      for (const rowData of rowCache.current.values()) {
        const cell = rowData[phys];
        if (cell) {
          // Wrapped cells shouldn't force full single-line width.
          const line = wrapText ? cell.slice(0, 80) : cell;
          const w = ctx.measureText(line).width;
          if (w > max) max = w;
        }
        if (++sampled >= 500) break;
      }
      widths[phys] = Math.min(AUTOFIT_MAX, Math.max(AUTOFIT_MIN, Math.ceil(max) + AUTOFIT_PADDING));
    }
    state.setColumnWidthsBulk(widths);
    state.clearAutoFitRequest();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoFitRequest?.nonce]);

  // Effective leading freeze: pinned view columns when any, else the manual
  // "freeze up to here" count (both clamped below the display width).
  const displayCount = projection.physical.length;
  const effectiveFreeze = Math.min(
    projection.frozen > 0 ? projection.frozen : frozenCols,
    Math.max(0, displayCount - 1),
  );

  return (
    <div className="gdg-wrapper">
      <DataEditor
        ref={gridRef}
        theme={dark ? darkGridTheme : lightGridTheme}
        columns={columns}
        headerIcons={HEADER_ICONS}
        rows={meta.rowCount}
        rowHeight={wrapText ? WRAP_ROW_HEIGHT : ROW_HEIGHT}
        freezeColumns={effectiveFreeze}
        getCellContent={getCellContent}
        onCellEdited={onCellEdited}
        onPaste={onPaste}
        onColumnResize={onColumnResize}
        onColumnMoved={onColumnMoved}
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
          columnIds={meta.columnIds}
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
