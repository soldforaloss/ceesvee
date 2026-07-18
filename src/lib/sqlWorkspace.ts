// Pure client-side logic for the F36 SQL workspace: named-parameter detection
// and typed validation, the query-history ring reducer, and schema-driven
// autocomplete matching. Everything here is deterministic and dependency-free
// so it is unit-testable in isolation; the Rust engine remains the authority
// for statement validation, parameter binding and history persistence — this
// module only powers the editor UX (detect `:params`, pre-flag bad values,
// offer suggestions, keep an in-memory history mirror).

import type { SqlHistoryEntry, SqlParam, SqlParamType, SqlSchemaDto, SqlTableInfo } from "../types";

/** The parameter value types the typed editor offers (mirrors the Rust enum). */
export const SQL_PARAM_TYPES: SqlParamType[] = [
  "text",
  "integer",
  "decimal",
  "float",
  "boolean",
  "date",
  "datetime",
  "null",
];

// ---------------------------------------------------------------------------
// Named-parameter detection
// ---------------------------------------------------------------------------

const IDENT_START = /[A-Za-z_]/;
const IDENT_CHAR = /[A-Za-z0-9_]/;

/**
 * Extract the distinct `:name` parameters a statement uses, in first-appearance
 * order. String literals, quoted and bracketed identifiers, line comments and
 * block comments are all skipped, and a double-colon cast is never mistaken for
 * a parameter. Only the `:name` form is recognised — the engine rejects
 * positional `?` and `@`/`$` placeholders, so the editor does too.
 */
export function detectParams(sql: string): string[] {
  const names: string[] = [];
  const seen = new Set<string>();
  let i = 0;
  const n = sql.length;
  while (i < n) {
    const c = sql[i];
    // Line comment.
    if (c === "-" && sql[i + 1] === "-") {
      const nl = sql.indexOf("\n", i + 2);
      i = nl === -1 ? n : nl + 1;
      continue;
    }
    // Block comment.
    if (c === "/" && sql[i + 1] === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end === -1 ? n : end + 2;
      continue;
    }
    // Quoted string / identifier: skip to the matching close (doubling escapes).
    if (c === "'" || c === '"' || c === "`") {
      i = skipQuoted(sql, i, c);
      continue;
    }
    if (c === "[") {
      const end = sql.indexOf("]", i + 1);
      i = end === -1 ? n : end + 1;
      continue;
    }
    // `::` cast — advance past both colons so neither starts a parameter.
    if (c === ":" && sql[i + 1] === ":") {
      i += 2;
      continue;
    }
    // A named parameter: `:` followed by an identifier.
    if (c === ":" && i + 1 < n && IDENT_START.test(sql[i + 1])) {
      let j = i + 1;
      while (j < n && IDENT_CHAR.test(sql[j])) j++;
      const name = sql.slice(i + 1, j);
      if (!seen.has(name)) {
        seen.add(name);
        names.push(name);
      }
      i = j;
      continue;
    }
    i += 1;
  }
  return names;
}

/** Advance past a quoted run starting at `open` (the opening quote), honouring
 * SQL's doubled-quote escape (`''`, `""`). Returns the index just past the
 * closing quote (or end of input). */
function skipQuoted(sql: string, open: number, quote: string): number {
  let i = open + 1;
  const n = sql.length;
  while (i < n) {
    if (sql[i] === quote) {
      if (sql[i + 1] === quote) {
        i += 2; // escaped quote
        continue;
      }
      return i + 1;
    }
    i += 1;
  }
  return n;
}

/**
 * Reconcile a typed-parameter list with the `:names` a (possibly edited) query
 * now uses: parameters still referenced keep their type and value, newly
 * introduced ones default to `text` with an empty value, and removed ones are
 * dropped. The result is ordered by first appearance in the SQL, so the editor
 * table tracks the query.
 */
export function mergeDetectedParams(existing: SqlParam[], sql: string): SqlParam[] {
  const byName = new Map(existing.map((p) => [p.name, p]));
  return detectParams(sql).map((name) => byName.get(name) ?? { name, type: "text", value: "" });
}

// ---------------------------------------------------------------------------
// Typed value validation
// ---------------------------------------------------------------------------

/** Strict decimal shape (mirrors the Rust `is_decimal`): optional sign, digits,
 * optional `.digits`; no exponent, no `inf`/`nan`, both sides non-empty. */
export function isStrictDecimal(raw: string): boolean {
  const s = raw.replace(/^[+-]/, "");
  const digits = (t: string) => t.length > 0 && /^[0-9]+$/.test(t);
  const dot = s.indexOf(".");
  if (dot === -1) return digits(s);
  return digits(s.slice(0, dot)) && digits(s.slice(dot + 1));
}

const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;

function isI64(raw: string): boolean {
  if (!/^[+-]?[0-9]+$/.test(raw)) return false;
  try {
    const v = BigInt(raw);
    return v >= I64_MIN && v <= I64_MAX;
  } catch {
    return false;
  }
}

function isCalendarDate(raw: string): boolean {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(raw);
  if (!m) return false;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (mo < 1 || mo > 12 || d < 1 || d > 31) return false;
  const dt = new Date(Date.UTC(y, mo - 1, d));
  return dt.getUTCFullYear() === y && dt.getUTCMonth() === mo - 1 && dt.getUTCDate() === d;
}

function isIsoDatetime(raw: string): boolean {
  // `YYYY-MM-DD`(T or space)`HH:MM:SS`(optional `.fff`)(optional `Z`/±HH:MM).
  const m =
    /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(\.\d+)?(Z|[+-]\d{2}:?\d{2})?$/.exec(raw);
  if (!m) return false;
  if (!isCalendarDate(`${m[1]}-${m[2]}-${m[3]}`)) return false;
  const [h, mi, s] = [Number(m[4]), Number(m[5]), Number(m[6])];
  return h <= 23 && mi <= 59 && s <= 59;
}

/**
 * Validate a typed parameter's value the way the engine will before it binds,
 * returning a human-readable error or `null` when the value is bindable. `null`
 * type needs no value; `text` accepts ANY string (including SQL-looking text —
 * it is bound as a value, never spliced into the statement), so it never
 * errors. All checks are advisory; the Rust binder re-validates authoritatively.
 */
export function validateParamValue(param: SqlParam): string | null {
  if (param.type === "null") return null;
  const raw = param.value ?? "";
  if (param.type === "text") return null; // any string is valid text
  const t = raw.trim();
  if (t === "") return "value required";
  switch (param.type) {
    case "integer":
      return isI64(t) ? null : "not a valid integer";
    case "decimal":
      return isStrictDecimal(t) ? null : "not a valid decimal";
    case "float": {
      const v = Number(t);
      return Number.isFinite(v) ? null : "not a valid number";
    }
    case "boolean":
      return /^(true|false|1|0)$/i.test(t) ? null : "use true or false";
    case "date":
      return isCalendarDate(t) ? null : "use YYYY-MM-DD";
    case "datetime":
      return isIsoDatetime(t) ? null : "use ISO 8601 date-time";
    default:
      return null;
  }
}

/** Map every parameter name to its validation error (only invalid ones). */
export function paramErrors(params: SqlParam[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const p of params) {
    const err = validateParamValue(p);
    if (err) out[p.name] = err;
  }
  return out;
}

/** Whether every parameter currently holds a bindable value. */
export function allParamsValid(params: SqlParam[]): boolean {
  return params.every((p) => validateParamValue(p) === null);
}

// ---------------------------------------------------------------------------
// Query-history ring reducer
// ---------------------------------------------------------------------------

/** Default history ring capacity (mirrors the engine's `SQL_HISTORY_CAP`). */
export const SQL_HISTORY_CAP = 100;

/**
 * Prepend `entry` to the history ring and cap it (most-recent first),
 * immutably. History is data only — this reducer never executes anything; it
 * keeps the in-memory mirror in step with an optimistic run before the
 * persisted list is re-fetched.
 */
export function pushHistoryEntry(
  list: SqlHistoryEntry[],
  entry: SqlHistoryEntry,
  cap: number = SQL_HISTORY_CAP,
): SqlHistoryEntry[] {
  return [entry, ...list].slice(0, Math.max(0, cap));
}

/** A one-line label for a history/saved entry: the SQL's first non-empty line,
 * trimmed and length-capped for the dropdown. */
export function historyLabel(sql: string, max = 80): string {
  const line = sql
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  const text = line ?? "(empty query)";
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

// ---------------------------------------------------------------------------
// Autocomplete
// ---------------------------------------------------------------------------

export interface SqlSuggestion {
  /** The text inserted into the editor. */
  text: string;
  kind: "table" | "column";
  /** Secondary label (table kind, or owning table + column type). */
  detail: string;
}

const SUGGESTION_CAP = 2000;

/**
 * Flatten a schema DTO into a de-duplicated suggestion list: one entry per
 * table alias, one per bare column name, and one per `alias.column` qualified
 * name. Bounded so a wide multi-source schema never produces an unbounded list.
 */
export function buildSuggestions(schema: SqlSchemaDto | null): SqlSuggestion[] {
  if (!schema) return [];
  const out: SqlSuggestion[] = [];
  const seen = new Set<string>();
  const push = (text: string, kind: SqlSuggestion["kind"], detail: string) => {
    const key = `${kind}:${text}`;
    if (seen.has(key) || out.length >= SUGGESTION_CAP) return;
    seen.add(key);
    out.push({ text, kind, detail });
  };
  const tables: SqlTableInfo[] = [...schema.documents, ...schema.files, ...schema.database];
  for (const t of tables) {
    push(t.alias, "table", `${t.kind}${t.label && t.label !== t.alias ? ` · ${t.label}` : ""}`);
    for (const col of t.columns) {
      push(col.name, "column", `${t.alias} · ${col.declType}`);
      push(`${t.alias}.${col.name}`, "column", col.declType);
    }
  }
  return out;
}

/**
 * The identifier-ish token immediately left of `caret` (letters, digits, `_`
 * and `.`), plus its start offset — the fragment autocomplete completes.
 */
export function currentToken(text: string, caret: number): { token: string; start: number } {
  let start = Math.max(0, Math.min(caret, text.length));
  while (start > 0 && /[A-Za-z0-9_.]/.test(text[start - 1])) start -= 1;
  return { token: text.slice(start, caret), start };
}

/**
 * Rank suggestions for a typed `prefix` (case-insensitive). Empty or
 * whitespace-only prefixes yield nothing (the list only appears while typing an
 * identifier). A dotted prefix (`alias.co`) matches qualified column names;
 * otherwise prefix-matches table and bare-column names. Exact case-prefix hits
 * rank first, then shorter names, then alphabetical.
 */
export function matchSuggestions(
  suggestions: SqlSuggestion[],
  prefix: string,
  limit = 8,
): SqlSuggestion[] {
  const p = prefix.trim();
  if (p === "") return [];
  const lower = p.toLowerCase();
  const dotted = p.includes(".");
  const hits = suggestions.filter((s) => {
    if (dotted) return s.kind === "column" && s.text.toLowerCase().startsWith(lower);
    // Bare prefix: match table names and unqualified column names only (skip
    // the `alias.column` forms — they surface via the dotted path).
    if (s.text.includes(".")) return false;
    return s.text.toLowerCase().startsWith(lower);
  });
  hits.sort((a, b) => {
    const ap = a.text.startsWith(p) ? 0 : 1;
    const bp = b.text.startsWith(p) ? 0 : 1;
    if (ap !== bp) return ap - bp;
    if (a.text.length !== b.text.length) return a.text.length - b.text.length;
    return a.text.localeCompare(b.text);
  });
  return hits.slice(0, limit);
}

/**
 * Apply a chosen suggestion to the editor text: replace the token ending at
 * `caret` with `suggestion.text`, returning the new text and the caret offset
 * to place after the inserted identifier.
 */
export function applySuggestion(
  text: string,
  caret: number,
  suggestion: string,
): { text: string; caret: number } {
  const { start } = currentToken(text, caret);
  const next = text.slice(0, start) + suggestion + text.slice(caret);
  return { text: next, caret: start + suggestion.length };
}
