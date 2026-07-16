// Resolve what Copy As / Paste Special operate on from the grid selection
// state (F14). Pure and unit-tested; the dialogs stay thin.

import type { CellRect } from "../types";

export interface CopyTarget {
  /** Display row indices; `null` means every visible row (kept off the IPC
   * wire so a million-row copy doesn't ship a million-entry array). */
  rows: number[] | null;
  cols: number[];
}

/**
 * The rows/columns a copy should cover. "visible" copies the whole filtered
 * view; "selection" prefers the active rectangle, then row markers, then
 * column markers.
 */
export function resolveCopyTarget(
  scope: "selection" | "visible",
  selectionRect: CellRect | null,
  selectedRows: number[],
  selectedCols: number[],
  colCount: number,
): CopyTarget | null {
  const allCols = Array.from({ length: colCount }, (_, i) => i);
  if (scope === "visible") return { rows: null, cols: allCols };
  if (selectionRect) {
    return {
      rows: Array.from({ length: selectionRect.height }, (_, i) => selectionRect.y + i),
      cols: Array.from({ length: selectionRect.width }, (_, i) => selectionRect.x + i),
    };
  }
  if (selectedRows.length) return { rows: [...selectedRows], cols: allCols };
  if (selectedCols.length) return { rows: null, cols: [...selectedCols] };
  return null;
}

/** Clipboard payloads past this many characters ask for confirmation. */
export const CLIPBOARD_WARN_CHARS = 8_000_000;
