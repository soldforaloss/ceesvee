// Pure helpers for the reopen-with-settings and external-change flows.

import { delimiterLabel } from "./labels";
import type { DocumentMeta, FileFingerprint, OpenOptions, ReparseDiff } from "../types";

/** Stable string identity of a fingerprint, for the per-document ignore list. */
export function fingerprintKey(fp: FileFingerprint | null): string {
  return fp ? `${fp.size}:${fp.modifiedAtMs}` : "missing";
}

/**
 * The document's current parse settings as explicit overrides — what "Reload
 * from disk" passes to applyReparse so nothing is silently re-detected.
 */
export function currentOpenOptions(meta: DocumentMeta): OpenOptions {
  return {
    delimiter: meta.delimiter,
    encoding: meta.encoding,
    hasHeaderRow: meta.hasHeaderRow,
  };
}

const DIFF_LABELS: Record<string, string> = {
  delimiter: "Delimiter",
  encoding: "Encoding",
  bom: "BOM",
  lineEnding: "Line endings",
  headerMode: "First row is header",
  rowCount: "Rows",
  colCount: "Columns",
};

function prettyValue(field: string, value: string): string {
  if (field === "delimiter") return delimiterLabel(value);
  if (field === "bom" || field === "headerMode") return value === "true" ? "yes" : "no";
  if (field === "lineEnding") return value.toUpperCase();
  return value;
}

/** Human-readable "Delimiter: Comma → Semicolon" line for one difference. */
export function describeDiff(diff: ReparseDiff): string {
  const label = DIFF_LABELS[diff.field] ?? diff.field;
  return `${label}: ${prettyValue(diff.field, diff.current)} → ${prettyValue(diff.field, diff.proposed)}`;
}
