// Pure helpers for the diagnostics panel: staleness, ordering and progress.

import type {
  DiagnosticIssue,
  DiagnosticSeverity,
  DiagnosticsReport,
  DocumentMeta,
} from "../types";

const SEVERITY_RANK: Record<DiagnosticSeverity, number> = {
  error: 0,
  warning: 1,
  info: 2,
};

/** Sort issues most-severe first, keeping the scan's order within a severity. */
export function sortIssues(issues: DiagnosticIssue[]): DiagnosticIssue[] {
  return [...issues].sort((a, b) => SEVERITY_RANK[a.severity] - SEVERITY_RANK[b.severity]);
}

/**
 * Whether a report no longer describes the document (any mutation bumps the
 * revision, so a stale report must be discarded and the panel offers a rescan).
 */
export function isReportStale(
  meta: Pick<DocumentMeta, "id" | "revision"> | null,
  report: DiagnosticsReport | null,
): boolean {
  if (!meta || !report) return false;
  return report.docId !== meta.id || report.revision !== meta.revision;
}

/** Integer 0-100 progress, or null while the total is unknown. */
export function progressPercent(processed: number, total: number | null): number | null {
  if (total === null || total <= 0) return null;
  return Math.min(100, Math.round((processed / total) * 100));
}

/** Total issue count across both report sections. */
export function issueCount(report: DiagnosticsReport | null): number {
  if (!report) return 0;
  return report.source.length + report.current.length;
}
