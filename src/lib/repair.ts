// Pure helpers for missing-value repair (F29).

import type { RepairPreview } from "../types";

/** Parse the null-token input: comma-separated, trimmed, blanks dropped. */
export function parseNullTokens(text: string): string[] {
  return text
    .split(",")
    .map((t) => t.trim())
    .filter((t) => t !== "");
}

/** The apply button's label: explicit about removals, exact about counts. */
export function repairApplyLabel(preview: RepairPreview | null): string {
  if (!preview) return "Apply";
  if (preview.rowsRemoved > 0 || preview.columnsRemoved > 0) {
    const parts: string[] = [];
    if (preview.rowsRemoved > 0)
      parts.push(
        `${preview.rowsRemoved.toLocaleString()} row${preview.rowsRemoved === 1 ? "" : "s"}`,
      );
    if (preview.columnsRemoved > 0)
      parts.push(
        `${preview.columnsRemoved.toLocaleString()} column${preview.columnsRemoved === 1 ? "" : "s"}`,
      );
    return `Remove ${parts.join(" and ")}`;
  }
  return `Apply to ${preview.cellsAffected.toLocaleString()} cell${preview.cellsAffected === 1 ? "" : "s"}`;
}

/** Whether a preview found nothing at all to do. */
export function repairIsNoop(preview: RepairPreview | null): boolean {
  return (
    preview !== null &&
    preview.cellsAffected === 0 &&
    preview.rowsRemoved === 0 &&
    preview.columnsRemoved === 0
  );
}
