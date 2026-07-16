// ViewProjection (F12): the pure column-side coordinate layer between the
// grid (DISPLAY positions) and the document (PHYSICAL column indices).
// Row-side projection (filter + non-destructive sort) lives in Rust; the
// grid already speaks display rows to the backend. This module composes
// hidden columns, arbitrary pinned columns, and reordering — all referenced
// by stable logical column IDs so views survive renames and structural
// edits — and reports missing IDs instead of corrupting a view.

import type { FilterGroup, FilterNode, NamedView, SortKey, ViewSortKey } from "../types";

/** The column-layout ingredients of a view, by stable column ID. */
export interface ColumnLayout {
  hiddenColumnIds: string[];
  /** Pin order; pinned columns display first and freeze. */
  pinnedColumnIds: string[];
  /** Display order for unpinned columns; unlisted IDs keep file order. */
  columnOrder: string[];
}

export const emptyLayout = (): ColumnLayout => ({
  hiddenColumnIds: [],
  pinnedColumnIds: [],
  columnOrder: [],
});

/** True when the layout changes nothing about the natural column view. */
export function layoutIsTrivial(layout: ColumnLayout | null): boolean {
  return (
    !layout ||
    (layout.hiddenColumnIds.length === 0 &&
      layout.pinnedColumnIds.length === 0 &&
      layout.columnOrder.length === 0)
  );
}

/** The resolved display→physical column mapping. */
export interface ColumnProjection {
  /** physical[displayIndex] = physical column index. */
  physical: number[];
  /** Leading frozen (pinned) column count in display space. */
  frozen: number;
  /** Layout-referenced IDs that no longer exist in the document. */
  missing: string[];
  /** True when the projection is the identity (no translation needed). */
  identity: boolean;
}

/**
 * Resolve a layout against the document's current column IDs. Pinned columns
 * come first (in pin order), then the remaining visible columns in
 * `columnOrder` order with unlisted columns keeping file order. Hidden
 * columns are excluded; IDs that no longer resolve are reported in `missing`
 * (a recoverable warning — never an error).
 */
export function projectColumns(columnIds: string[], layout: ColumnLayout | null): ColumnProjection {
  if (layoutIsTrivial(layout)) {
    return {
      physical: columnIds.map((_, i) => i),
      frozen: 0,
      missing: [],
      identity: true,
    };
  }
  const l = layout as ColumnLayout;
  const indexOf = new Map<string, number>();
  columnIds.forEach((id, i) => indexOf.set(id, i));

  const missing: string[] = [];
  const seenMissing = new Set<string>();
  const note = (id: string) => {
    if (indexOf.has(id) || seenMissing.has(id)) return;
    seenMissing.add(id);
    missing.push(id);
  };
  l.hiddenColumnIds.forEach(note);
  l.pinnedColumnIds.forEach(note);
  l.columnOrder.forEach(note);

  const hidden = new Set(l.hiddenColumnIds);
  const pinned = l.pinnedColumnIds.filter((id) => indexOf.has(id) && !hidden.has(id));
  const pinnedSet = new Set(pinned);

  const ordered: string[] = [];
  const placed = new Set<string>();
  for (const id of l.columnOrder) {
    if (!indexOf.has(id) || hidden.has(id) || pinnedSet.has(id) || placed.has(id)) continue;
    ordered.push(id);
    placed.add(id);
  }
  for (const id of columnIds) {
    if (hidden.has(id) || pinnedSet.has(id) || placed.has(id)) continue;
    ordered.push(id);
    placed.add(id);
  }

  const displayIds = [...pinned, ...ordered];
  const physical = displayIds.map((id) => indexOf.get(id) as number);
  const identity = physical.length === columnIds.length && physical.every((p, i) => p === i);
  return { physical, frozen: pinned.length, missing, identity: identity && pinned.length === 0 };
}

/** Display index of a physical column, or null when it is hidden. */
export function physicalToDisplay(projection: ColumnProjection, physical: number): number | null {
  if (projection.identity) return physical < projection.physical.length ? physical : null;
  const d = projection.physical.indexOf(physical);
  return d === -1 ? null : d;
}

/**
 * Split an ascending list of physical columns into contiguous runs. Under a
 * reordered/hidden layout a display-contiguous selection may map to several
 * physical runs; rectangle-shaped backend calls are made per run.
 */
export function contiguousRuns(cols: number[]): { start: number; len: number }[] {
  const runs: { start: number; len: number }[] = [];
  for (const c of cols) {
    const last = runs[runs.length - 1];
    if (last && c === last.start + last.len) {
      last.len += 1;
    } else {
      runs.push({ start: c, len: 1 });
    }
  }
  return runs;
}

/**
 * Remap a saved filter's column indices from the ID snapshot it was saved
 * against onto the document's current columns. ALL-OR-NOTHING: a filter with
 * any unmappable column is not applied (dropping single conditions could
 * silently widen an AND group); the missing IDs are reported so the UI can
 * warn recoverably. The view itself is never modified.
 */
export function remapFilterColumns(
  filter: FilterGroup,
  savedIds: string[],
  currentIds: string[],
): { filter: FilterGroup | null; missing: string[] } {
  const indexOf = new Map<string, number>();
  currentIds.forEach((id, i) => indexOf.set(id, i));
  const missing: string[] = [];

  const mapCol = (col: number): number | null => {
    const id = savedIds[col];
    // A filter saved before IDs existed (or one whose snapshot is too short)
    // can still apply when the index itself is in range.
    if (id === undefined) return col < currentIds.length ? col : null;
    const mapped = indexOf.get(id);
    if (mapped === undefined) {
      if (!missing.includes(id)) missing.push(id);
      return null;
    }
    return mapped;
  };

  const mapNode = (node: FilterNode): FilterNode | null => {
    if (node.type === "condition") {
      const col = mapCol(node.column);
      return col === null ? null : { ...node, column: col };
    }
    const nodes: FilterNode[] = [];
    for (const child of node.nodes) {
      const mapped = mapNode(child);
      if (mapped === null) return null;
      nodes.push(mapped);
    }
    return { ...node, nodes };
  };

  const mapped = mapNode(filter);
  return mapped === null || mapped.type !== "group"
    ? { filter: null, missing }
    : { filter: mapped, missing };
}

/**
 * Resolve a view's ID-based sort keys onto current physical columns. Keys
 * whose column no longer exists are SKIPPED (dropping a sort key is benign,
 * unlike dropping a filter condition) and reported.
 */
export function resolveSortKeys(
  keys: ViewSortKey[],
  currentIds: string[],
): { keys: SortKey[]; missing: string[] } {
  const indexOf = new Map<string, number>();
  currentIds.forEach((id, i) => indexOf.set(id, i));
  const resolved: SortKey[] = [];
  const missing: string[] = [];
  for (const key of keys) {
    const col = indexOf.get(key.columnId);
    if (col === undefined) {
      if (!missing.includes(key.columnId)) missing.push(key.columnId);
      continue;
    }
    resolved.push({ column: col, descending: key.descending ?? false });
  }
  return { keys: resolved, missing };
}

/** Index-keyed grid widths → ID-keyed widths for persistence. */
export function widthsToIds(
  widths: Record<number, number>,
  columnIds: string[],
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [col, width] of Object.entries(widths)) {
    const id = columnIds[Number(col)];
    if (id !== undefined) out[id] = width;
  }
  return out;
}

/** ID-keyed persisted widths → index-keyed widths for the grid. */
export function widthsFromIds(
  byId: Record<string, number>,
  columnIds: string[],
): Record<number, number> {
  const out: Record<number, number> = {};
  columnIds.forEach((id, i) => {
    const width = byId[id];
    if (width !== undefined) out[i] = width;
  });
  return out;
}

/** The layout ingredients of a named view. */
export function layoutOfView(view: NamedView): ColumnLayout {
  return {
    hiddenColumnIds: view.hiddenColumnIds,
    pinnedColumnIds: view.pinnedColumnIds,
    columnOrder: view.columnOrder,
  };
}
