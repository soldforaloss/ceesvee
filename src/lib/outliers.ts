// Pure helpers for the outlier finder (F30).

import type { OutlierAction, OutlierMethod } from "../types";

/** Numeric detection methods (bounds/median make sense). */
export function isNumericMethod(type: OutlierMethod["type"]): boolean {
  return type === "iqr" || type === "mad" || type === "zScore" || type === "percentile";
}

/**
 * Whether a corrective action applies to a method: median replacement and
 * capping need numeric statistics; blanking and row removal always work.
 */
export function actionAvailable(method: OutlierMethod["type"], action: OutlierAction): boolean {
  if (action === "replaceMedian" || action === "capToBounds") return isNumericMethod(method);
  return true;
}

/** Parse the allowed-values textarea: split on commas/newlines, trim. */
export function parseAllowedValues(text: string): string[] {
  return text
    .split(/\r?\n|,/)
    .map((v) => v.trim())
    .filter((v) => v !== "");
}
