// Pure helpers for explicit schemas and typed columns (F31): display labels,
// the display-format catalogue, the strict/advisory edit-gating decision, and
// a display-only value formatter that MIRRORS the Rust `schema::format_value`
// catalogue (module docs in `src-tauri/src/schema.rs`).
//
// The formatter is deliberately conservative: it only ever changes the RENDER
// of a cell, never its stored text, and whenever it cannot confidently format
// a value it returns the raw text unchanged — exactly like the Rust side's
// `unwrap_or_else(raw)` fallback. So a divergence from the backend can only
// ever show raw text where the backend would show a formatted string; it can
// never fabricate or corrupt data. The backend `format_value` remains the
// source of truth; date formatting here covers ISO-shaped values only.

import type { CellEditValidation, ColumnSchema, LogicalType } from "../types";

// ---------------------------------------------------------------------------
// Labels and option catalogues
// ---------------------------------------------------------------------------

/** Display labels for the nine logical types. */
export const LOGICAL_TYPE_LABELS: Record<LogicalType, string> = {
  text: "Text",
  integer: "Integer",
  decimal: "Decimal",
  float: "Float",
  boolean: "Boolean",
  date: "Date",
  datetime: "Datetime",
  uuid: "UUID",
  json: "JSON",
};

/** The nine logical types, in the order the editor lists them. */
export const LOGICAL_TYPES: LogicalType[] = [
  "text",
  "integer",
  "decimal",
  "float",
  "boolean",
  "date",
  "datetime",
  "uuid",
  "json",
];

const NUMERIC_TYPES: ReadonlySet<LogicalType> = new Set<LogicalType>([
  "integer",
  "decimal",
  "float",
]);
const TEMPORAL_TYPES: ReadonlySet<LogicalType> = new Set<LogicalType>(["date", "datetime"]);

export function isNumericType(lt: LogicalType): boolean {
  return NUMERIC_TYPES.has(lt);
}
export function isTemporalType(lt: LogicalType): boolean {
  return TEMPORAL_TYPES.has(lt);
}

/** One selectable display-format option. */
export interface DisplayFormatOption {
  value: string;
  label: string;
}

const NUMBER_FORMATS: DisplayFormatOption[] = [
  { value: "thousands", label: "Thousands grouping (1,234.5)" },
  { value: "fixed:0", label: "Fixed 0 decimals" },
  { value: "fixed:2", label: "Fixed 2 decimals" },
  { value: "fixed:4", label: "Fixed 4 decimals" },
  { value: "percent", label: "Percent (× 100 %)" },
];

const DATE_FORMATS: DisplayFormatOption[] = [
  { value: "iso", label: "ISO (2024-01-31)" },
  { value: "eu", label: "European (31.01.2024)" },
  { value: "us", label: "US (01/31/2024)" },
  { value: "long", label: "Long (Jan 31, 2024)" },
];

/**
 * The display-format choices offered for a logical type (empty for types with
 * no display catalogue: text/boolean/uuid/json). The leading "" option means
 * "raw / no formatting".
 */
export function displayFormatOptions(lt: LogicalType): DisplayFormatOption[] {
  if (isNumericType(lt)) return NUMBER_FORMATS;
  if (isTemporalType(lt)) return DATE_FORMATS;
  return [];
}

// ---------------------------------------------------------------------------
// Strict / advisory edit gating (pure decision, shared by every editor UI)
// ---------------------------------------------------------------------------

export interface EditGate {
  /** Strict + invalid: the edit must not be committed. */
  block: boolean;
  /** Advisory + invalid: commit is allowed but a warning is shown. */
  warn: boolean;
  /** Human-readable reason, when the value is invalid. */
  message: string | null;
}

/**
 * Decide how an editor UI should treat a proposed value, given the backend's
 * verdict. A valid value (or a column with no declared schema) passes freely;
 * a strict violation blocks the commit; an advisory violation warns but
 * allows it (the backend records the issue when the edit lands).
 */
export function gateCellEdit(v: CellEditValidation | null): EditGate {
  if (!v || v.valid) return { block: false, warn: false, message: null };
  const message = v.reason ?? "Value does not match the declared type";
  if (v.mode === "strict") return { block: true, warn: false, message };
  return { block: false, warn: true, message };
}

// ---------------------------------------------------------------------------
// Locale-aware number separators (mirrors `schema::separators`)
// ---------------------------------------------------------------------------

interface Separators {
  decimal: string;
  groupOut: string;
  groupAccept: string[];
}

const NBSP = " ";
const NARROW_NBSP = " ";

export function separatorsFor(locale?: string | null): Separators {
  const tag = locale ?? "";
  if (tag === "de-CH" || tag === "fr-CH" || tag === "it-CH" || tag === "en-CH") {
    return { decimal: ".", groupOut: "'", groupAccept: ["'", "’"] };
  }
  const lang = tag.split(/[-_]/)[0] ?? "";
  // Comma decimal, dot grouping.
  if (
    ["de", "es", "it", "nl", "pt", "da", "el", "id", "tr", "vi", "hr", "sl", "ro", "ca"].includes(
      lang,
    )
  ) {
    return { decimal: ",", groupOut: ".", groupAccept: ["."] };
  }
  // Comma decimal, space grouping (regular, NBSP, narrow NBSP accepted).
  if (
    [
      "fr",
      "ru",
      "pl",
      "cs",
      "sk",
      "sv",
      "fi",
      "nb",
      "nn",
      "no",
      "uk",
      "lt",
      "lv",
      "et",
      "hu",
      "bg",
    ].includes(lang)
  ) {
    return { decimal: ",", groupOut: NBSP, groupAccept: [" ", NBSP, NARROW_NBSP] };
  }
  // Default (en, ja, zh, ko, …): dot decimal, comma grouping.
  return { decimal: ".", groupOut: ",", groupAccept: [","] };
}

/**
 * Normalise locale-formatted number text to plain ASCII (`.` decimal, no
 * grouping), mirroring `schema::normalize_number`. Returns null when the text
 * is not a well-formed number for the given options (grouping separators must
 * sit between digits and start a group of exactly three).
 */
function normalizeNumber(
  s: string,
  sep: Separators,
  allowDecimal: boolean,
  allowExponent: boolean,
): string | null {
  const chars = Array.from(s);
  let out = "";
  let seenDecimal = false;
  let seenExponent = false;
  let i = 0;
  const isDigit = (c: string) => c >= "0" && c <= "9";
  while (i < chars.length) {
    const c = chars[i];
    if (c === "+" || c === "-") {
      const afterExponent = out.endsWith("e");
      if (!(out.length === 0 || afterExponent)) return null;
      out += c;
    } else if (isDigit(c)) {
      out += c;
    } else if (!seenExponent && !seenDecimal && sep.groupAccept.includes(c)) {
      const prevDigit = i > 0 && isDigit(chars[i - 1]);
      let j = i + 1;
      while (j < chars.length && isDigit(chars[j])) j += 1;
      const run = j - (i + 1);
      const boundaryOk =
        j === chars.length ||
        chars[j] === sep.decimal ||
        sep.groupAccept.includes(chars[j]) ||
        (allowExponent && (chars[j] === "e" || chars[j] === "E"));
      if (!(prevDigit && run === 3 && boundaryOk)) return null;
      // Valid group separator: skip it.
    } else if (c === sep.decimal && allowDecimal && !seenExponent) {
      if (seenDecimal) return null;
      seenDecimal = true;
      out += ".";
    } else if ((c === "e" || c === "E") && allowExponent) {
      if (seenExponent) return null;
      seenExponent = true;
      out += "e";
    } else {
      return null;
    }
    i += 1;
  }
  if (![...out].some(isDigit)) return null;
  return out;
}

// ---------------------------------------------------------------------------
// Number formatting (string arithmetic: exact for integer/decimal)
// ---------------------------------------------------------------------------

type NumberPattern = { kind: "thousands" } | { kind: "fixed"; n: number } | { kind: "percent" };

function numberPattern(fmt: string): NumberPattern | null {
  if (fmt === "thousands") return { kind: "thousands" };
  if (fmt === "percent") return { kind: "percent" };
  const m = /^fixed:(\d+)$/.exec(fmt);
  if (m) {
    const n = Number(m[1]);
    if (n <= 12) return { kind: "fixed", n };
  }
  return null;
}

function stripLeadingZeros(digits: string): string {
  const trimmed = digits.replace(/^0+/, "");
  return trimmed === "" ? "0" : trimmed;
}

function groupDigits(digits: string, group: string): string {
  const n = digits.length;
  let out = "";
  for (let i = 0; i < n; i++) {
    if (i > 0 && (n - i) % 3 === 0) out += group;
    out += digits[i];
  }
  return out;
}

function assemble(
  negative: boolean,
  intDigits: string,
  fracDigits: string,
  sep: Separators,
  group: boolean,
): string {
  const intPart = group ? groupDigits(intDigits, sep.groupOut) : intDigits;
  let out = negative ? "-" : "";
  out += intPart;
  if (fracDigits.length > 0) out += sep.decimal + fracDigits;
  return out;
}

interface DecimalParts {
  negative: boolean;
  int: string;
  frac: string;
}

/** Split normalised ASCII text into decimal parts; null when it has an exponent. */
function decimalParts(norm: string): DecimalParts | null {
  if (/[eE]/.test(norm)) return null;
  let negative = false;
  let rest = norm;
  if (rest.startsWith("-")) {
    negative = true;
    rest = rest.slice(1);
  } else if (rest.startsWith("+")) {
    rest = rest.slice(1);
  }
  const dot = rest.indexOf(".");
  const intRaw = dot >= 0 ? rest.slice(0, dot) : rest;
  const frac = dot >= 0 ? rest.slice(dot + 1) : "";
  return { negative, int: stripLeadingZeros(intRaw), frac };
}

/** Add one to a non-negative integer digit string. */
function incrementDigits(s: string): string {
  const arr = s.split("");
  let i = arr.length - 1;
  for (; i >= 0; i--) {
    if (arr[i] === "9") {
      arr[i] = "0";
    } else {
      arr[i] = String(Number(arr[i]) + 1);
      return arr.join("");
    }
  }
  return "1" + arr.join("");
}

/** Rescale to exactly `n` fractional digits, half-away-from-zero rounding. */
function roundHalfAway(int: string, frac: string, n: number): { int: string; frac: string } {
  if (frac.length <= n) return { int, frac: frac.padEnd(n, "0") };
  const keep = frac.slice(0, n);
  const nextDigit = frac.charCodeAt(n) - 48;
  let digits = int + keep;
  if (nextDigit >= 5) digits = incrementDigits(digits);
  if (n === 0) return { int: stripLeadingZeros(digits), frac: "" };
  while (digits.length <= n) digits = "0" + digits;
  const newInt = stripLeadingZeros(digits.slice(0, digits.length - n));
  const newFrac = digits.slice(digits.length - n);
  return { int: newInt, frac: newFrac };
}

/** Multiply by 100 (shift the decimal point right two places). */
function times100(parts: DecimalParts): DecimalParts {
  const digits = parts.int + parts.frac;
  const newPointFromRight = parts.frac.length - 2;
  let int: string;
  let frac: string;
  if (newPointFromRight <= 0) {
    int = stripLeadingZeros(digits + "0".repeat(-newPointFromRight));
    frac = "";
  } else {
    int = stripLeadingZeros(digits.slice(0, digits.length - newPointFromRight));
    frac = digits.slice(digits.length - newPointFromRight);
  }
  return { negative: parts.negative, int, frac };
}

function formatDecimalParts(parts: DecimalParts, pattern: NumberPattern, sep: Separators): string {
  switch (pattern.kind) {
    case "thousands":
      return assemble(parts.negative, parts.int, parts.frac, sep, true);
    case "fixed": {
      const r = roundHalfAway(parts.int, parts.frac, pattern.n);
      return assemble(parts.negative, r.int, r.frac, sep, false);
    }
    case "percent": {
      const p = times100(parts);
      return assemble(p.negative, p.int, p.frac, sep, false) + "%";
    }
  }
}

/** Float path: JS-number based (the stored value already parsed as a float). */
function formatFloat(norm: string, pattern: NumberPattern, sep: Separators): string | null {
  const num = Number(norm);
  if (!Number.isFinite(num)) return null;
  if (pattern.kind === "fixed") {
    const parts = decimalParts(num.toFixed(pattern.n));
    return parts ? assemble(parts.negative, parts.int, parts.frac, sep, false) : null;
  }
  const scaled = pattern.kind === "percent" ? num * 100 : num;
  const parts = decimalParts(scaled.toString());
  if (!parts) return null; // exponent notation: fall back to raw (as Rust does)
  const out = assemble(parts.negative, parts.int, parts.frac, sep, pattern.kind === "thousands");
  return pattern.kind === "percent" ? out + "%" : out;
}

function formatNumberValue(
  lt: LogicalType,
  raw: string,
  fmt: string,
  sep: Separators,
): string | null {
  const pattern = numberPattern(fmt);
  if (!pattern) return null;
  if (lt === "float") {
    const norm = normalizeNumber(raw.trim(), sep, true, true);
    return norm ? formatFloat(norm, pattern, sep) : null;
  }
  const norm = normalizeNumber(raw.trim(), sep, lt !== "integer", false);
  if (norm === null) return null;
  const parts = decimalParts(norm);
  return parts ? formatDecimalParts(parts, pattern, sep) : null;
}

// ---------------------------------------------------------------------------
// Date / datetime formatting (ISO-shaped inputs; mirrors `date_pattern`)
// ---------------------------------------------------------------------------

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

interface Temporal {
  y: number;
  mo: number;
  d: number;
  h: number;
  mi: number;
  s: number;
}

function inRange(t: Temporal): boolean {
  return t.mo >= 1 && t.mo <= 12 && t.d >= 1 && t.d <= 31 && t.h <= 23 && t.mi <= 59 && t.s <= 59;
}

/** Parse an ISO-shaped date or datetime; null for zoned or exotic inputs. */
function parseIsoTemporal(raw: string, wantTime: boolean): Temporal | null {
  const t = raw.trim();
  const dOnly = /^(\d{4})-(\d{2})-(\d{2})$/.exec(t);
  if (!wantTime) {
    if (!dOnly) return null;
    const parsed: Temporal = {
      y: +dOnly[1],
      mo: +dOnly[2],
      d: +dOnly[3],
      h: 0,
      mi: 0,
      s: 0,
    };
    return inRange(parsed) ? parsed : null;
  }
  const dt = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})(?::(\d{2}))?$/.exec(t);
  if (dt) {
    const parsed: Temporal = {
      y: +dt[1],
      mo: +dt[2],
      d: +dt[3],
      h: +dt[4],
      mi: +dt[5],
      s: dt[6] ? +dt[6] : 0,
    };
    return inRange(parsed) ? parsed : null;
  }
  if (dOnly) {
    const parsed: Temporal = {
      y: +dOnly[1],
      mo: +dOnly[2],
      d: +dOnly[3],
      h: 0,
      mi: 0,
      s: 0,
    };
    return inRange(parsed) ? parsed : null;
  }
  return null;
}

function formatTemporal(t: Temporal, fmt: string, hasTime: boolean): string | null {
  const pad = (n: number) => String(n).padStart(2, "0");
  const y = String(t.y).padStart(4, "0");
  let date: string;
  let time: string;
  switch (fmt) {
    case "iso":
      date = `${y}-${pad(t.mo)}-${pad(t.d)}`;
      time = ` ${pad(t.h)}:${pad(t.mi)}:${pad(t.s)}`;
      break;
    case "eu":
      date = `${pad(t.d)}.${pad(t.mo)}.${y}`;
      time = ` ${pad(t.h)}:${pad(t.mi)}:${pad(t.s)}`;
      break;
    case "us":
      date = `${pad(t.mo)}/${pad(t.d)}/${y}`;
      time = ` ${pad(t.h)}:${pad(t.mi)}:${pad(t.s)}`;
      break;
    case "long":
      date = `${MONTHS[t.mo - 1]} ${t.d}, ${y}`;
      time = ` ${pad(t.h)}:${pad(t.mi)}`; // long datetime omits seconds
      break;
    default:
      return null;
  }
  return hasTime ? date + time : date;
}

// ---------------------------------------------------------------------------
// Public: display-only cell formatting
// ---------------------------------------------------------------------------

/**
 * Whether the (trimmed) cell matches one of the column's configured null
 * tokens. Mirrors the Rust `schema::is_null_token`: the cell is trimmed and
 * each token compared verbatim.
 */
function isNullToken(schema: ColumnSchema, raw: string): boolean {
  const trimmed = raw.trim();
  return schema.nullTokens.some((t) => t === trimmed);
}

/**
 * Render a cell for DISPLAY under the column schema's `displayFormat`. Never
 * changes stored text: no schema, no display format, an unknown pattern, or a
 * value that does not parse as the declared type all yield the raw text.
 * Text/boolean/uuid/json have no display patterns and always render raw. A cell
 * matching a configured null token is classified as blank — never a value — so
 * it renders raw even when it would otherwise parse (e.g. a "0" token under a
 * numeric fixed:2 format), mirroring the Rust `classify`/`format_value` path.
 */
export function formatCellValue(schema: ColumnSchema | undefined, raw: string): string {
  const fmt = schema?.displayFormat;
  if (!schema || !fmt) return raw;
  if (isNullToken(schema, raw)) return raw;
  const lt = schema.logicalType;
  if (isNumericType(lt)) {
    const sep = separatorsFor(schema.locale);
    return formatNumberValue(lt, raw, fmt, sep) ?? raw;
  }
  if (isTemporalType(lt)) {
    const t = parseIsoTemporal(raw, lt === "datetime");
    if (!t) return raw;
    return formatTemporal(t, fmt, lt === "datetime") ?? raw;
  }
  return raw;
}
