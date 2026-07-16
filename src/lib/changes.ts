// Pure helpers for the change inspector (F15).

import type { ChangeSummary } from "../types";

export const CHANGE_KIND_LABELS: Record<string, string> = {
  cellEdits: "Cell edits",
  insertRows: "Insert rows",
  deleteRows: "Delete rows",
  moveRow: "Move row",
  insertColumn: "Insert column",
  deleteColumns: "Delete columns",
  renameColumn: "Rename column",
  moveColumn: "Move column",
  sortRows: "Sort rows",
  composite: "Combined operation",
  revert: "Revert",
};

export function changeKindLabel(kind: string): string {
  return CHANGE_KIND_LABELS[kind] ?? kind;
}

/** "12:34:56"-style local time for a change's epoch seconds. */
export function changeTime(epochSecs: number): string {
  if (epochSecs === 0) return "";
  return new Date(epochSecs * 1000).toLocaleTimeString();
}

/** The JSON change report (F15): exactly the reported operations. */
export function changeReportJson(fileName: string, changes: ChangeSummary[]): string {
  return JSON.stringify(
    {
      document: fileName,
      exportedAtEpochSecs: Math.floor(Date.now() / 1000),
      changes,
    },
    null,
    2,
  );
}
