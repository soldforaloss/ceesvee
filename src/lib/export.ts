// Pure helpers for the scoped-export dialog (F04).

import type { CellRect, ExportScope, SplitOptions } from "../types";

/** The scope choices available given the current selection state. */
export interface ScopeChoice {
  scope: ExportScope;
  label: string;
}

/**
 * Build the scope menu for the export dialog from the current view/selection.
 * "All rows" is always available; the others appear when meaningful. A
 * non-destructive view sort (F12) also makes "visible rows" meaningful —
 * it is the only scope that writes rows in the current VIEW order.
 */
export function scopeChoices(
  filtered: boolean,
  selectionRect: CellRect | null,
  selectedRows: number[],
  selectedCols: number[],
  viewSorted = false,
): ScopeChoice[] {
  const choices: ScopeChoice[] = [{ scope: { type: "all" }, label: "All rows" }];
  if (filtered) {
    choices.push({ scope: { type: "visibleRows" }, label: "Visible (filtered) rows" });
  } else if (viewSorted) {
    choices.push({ scope: { type: "visibleRows" }, label: "Visible rows (view sort order)" });
  }
  if (selectedRows.length > 0) {
    choices.push({
      scope: { type: "selectedRows", rows: selectedRows },
      label: `Selected rows (${selectedRows.length.toLocaleString()})`,
    });
  }
  if (selectedCols.length > 0) {
    choices.push({
      scope: { type: "selectedColumns", columns: selectedCols },
      label: `Selected columns (${selectedCols.length.toLocaleString()})`,
    });
  }
  if (selectionRect && selectionRect.width * selectionRect.height > 1) {
    choices.push({
      scope: { type: "selectedRange", rect: selectionRect },
      label: `Selected range (${selectionRect.height.toLocaleString()} × ${selectionRect.width})`,
    });
  }
  return choices;
}

/** Stable identity for a scope choice, for <select> values. */
export function scopeKey(scope: ExportScope): string {
  return scope.type;
}

/** Validate/normalize split settings from the dialog inputs. */
export function buildSplit(
  kind: SplitOptions["type"],
  rowsPerFile: number,
  maxMegabytes: number,
  groupColumn: number,
): SplitOptions | { error: string } {
  switch (kind) {
    case "none":
      return { type: "none" };
    case "maxRows":
      if (!Number.isFinite(rowsPerFile) || rowsPerFile < 1) {
        return { error: "Rows per file must be at least 1" };
      }
      return { type: "maxRows", rowsPerFile: Math.floor(rowsPerFile) };
    case "approximateBytes":
      if (!Number.isFinite(maxMegabytes) || maxMegabytes <= 0) {
        return { error: "File size must be positive" };
      }
      return { type: "approximateBytes", maxBytes: Math.round(maxMegabytes * 1024 * 1024) };
    case "groupByColumn":
      if (groupColumn < 0) return { error: "Pick a column to group by" };
      return { type: "groupByColumn", column: groupColumn };
  }
}
