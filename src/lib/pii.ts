// Pure helpers for PII detection (F28).

import type { PiiFinding } from "../types";

/**
 * Column indices safe to export: those with NO findings. Returns null when
 * every column has findings (nothing safe).
 */
export function nonPiiColumns(findings: PiiFinding[], columnCount: number): number[] | null {
  const flagged = new Set(findings.map((f) => f.column));
  const safe = Array.from({ length: columnCount }, (_, i) => i).filter((i) => !flagged.has(i));
  return safe.length > 0 ? safe : null;
}

/** Whether a redaction action needs its secret filled in before previewing. */
export function redactionNeedsSecret(kind: string, secret: string): boolean {
  return kind === "pseudonymize" && secret.trim() === "";
}
