// Text inspection helpers for the multiline/raw cell editor (F13).

/** UTF-8 byte length of a string (what the file on disk will store). */
export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** Line count as an editor shows it: terminators split, CRLF counts once. */
export function countLines(value: string): number {
  if (value === "") return 1;
  let lines = 1;
  for (let i = 0; i < value.length; i++) {
    const ch = value[i];
    if (ch === "\r") {
      if (value[i + 1] === "\n") i++;
      lines++;
    } else if (ch === "\n") {
      lines++;
    }
  }
  return lines;
}

/** NUL is the one character CEESVEE never allows inside a cell. */
export function containsNul(value: string): boolean {
  return value.includes("\u0000");
}

/** Zero-width characters worth surfacing explicitly. */
const ZERO_WIDTH = new Set([0x200b, 0x200c, 0x200d, 0x2060, 0xfeff]);

/**
 * Render a cell value with every invisible character made visible, for the
 * editor's Escaped mode. Purely presentational: the stored value is never
 * altered. Backslash is escaped too, so the output is unambiguous.
 *
 * Represented: \n, \r, \t, NUL, non-breaking spaces, zero-width characters,
 * C0/C1 control characters, and the Unicode replacement character.
 */
export function escapeCellText(value: string): string {
  let out = "";
  for (const ch of value) {
    const code = ch.codePointAt(0)!;
    if (ch === "\\") {
      out += "\\\\";
    } else if (ch === "\n") {
      out += "\\n\n"; // keep the visual line structure readable
    } else if (ch === "\r") {
      out += "\\r";
    } else if (ch === "\t") {
      out += "\\t";
    } else if (code === 0) {
      out += "\\0";
    } else if (code === 0x00a0 || code === 0x202f) {
      out += `\\u{${code.toString(16).padStart(4, "0")}}`;
    } else if (ZERO_WIDTH.has(code)) {
      out += `\\u{${code.toString(16).padStart(4, "0")}}`;
    } else if (code === 0xfffd) {
      out += "\\u{fffd}";
    } else if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) {
      out += `\\u{${code.toString(16).padStart(4, "0")}}`;
    } else {
      out += ch;
    }
  }
  return out;
}

/** Whether the value contains anything Escaped mode would surface. */
export function hasInvisibles(value: string): boolean {
  return escapeCellText(value) !== value;
}
