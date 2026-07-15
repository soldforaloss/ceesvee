// Pure helpers for the save/export pipeline UI.

/**
 * Encodings that can silently lose characters. Unicode encodings represent
 * everything, so only these require the pre-save compatibility scan.
 */
export function isLegacyEncoding(encoding: string): boolean {
  const normalized = encoding.trim().toLowerCase();
  return !(
    normalized === "utf-8" ||
    normalized === "utf8" ||
    normalized.startsWith("utf-16") ||
    normalized.startsWith("utf16")
  );
}

/** "12.3 MB"-style byte formatting for progress lines. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = "B";
  for (const next of units) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  const digits = value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${unit}`;
}
