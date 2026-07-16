// Cell-reference parsing for the palette's go-to commands (F11).

/**
 * Parse "42" (row), "C7" (A1-style cell), or "3,5" / "3:5" (row,column) into
 * zero-based display coordinates. Returns null for anything else.
 */
export function parseCellRef(arg: string): { row: number; col: number } | null {
  const trimmed = arg.trim().toUpperCase();
  if (trimmed === "") return null;
  const a1 = /^([A-Z]+)(\d+)$/.exec(trimmed);
  if (a1) {
    const row = Number(a1[2]);
    if (row < 1) return null; // references are 1-based; "A0" is invalid
    let col = 0;
    for (const ch of a1[1]) col = col * 26 + (ch.charCodeAt(0) - 64);
    return { row: row - 1, col: col - 1 };
  }
  const pair = /^(\d+)\s*[,:]\s*(\d+)$/.exec(trimmed);
  if (pair) {
    const row = Number(pair[1]);
    const col = Number(pair[2]);
    if (row < 1 || col < 1) return null;
    return { row: row - 1, col: col - 1 };
  }
  const rowOnly = /^(\d+)$/.exec(trimmed);
  if (rowOnly) {
    const row = Number(rowOnly[1]);
    if (row < 1) return null;
    return { row: row - 1, col: 0 };
  }
  return null;
}
