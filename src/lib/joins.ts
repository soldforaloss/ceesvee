// Pure helpers for relational joins (F21).

import type { JoinPreview } from "../types";

/** Default expansion threshold before a run needs explicit confirmation. */
export const JOIN_CONFIRM_THRESHOLD = 1_000_000;

/** Whether this preview's projected output needs an explicit confirmation. */
export function joinNeedsConfirmation(
  preview: JoinPreview | null,
  threshold = JOIN_CONFIRM_THRESHOLD,
): boolean {
  return preview !== null && preview.projectedRows > threshold;
}

/** The run button's label for the current preview/confirmation state. */
export function joinRunLabel(
  preview: JoinPreview | null,
  confirmed: boolean,
  running: boolean,
  threshold = JOIN_CONFIRM_THRESHOLD,
): string {
  if (running) return "Joining…";
  if (joinNeedsConfirmation(preview, threshold) && !confirmed && preview) {
    return `Large output (${preview.projectedRows.toLocaleString()} rows) — confirm`;
  }
  return "Join into a new document";
}
