// Pure, framework-free helpers for the Excel `.xlsx` interop UI (F34): import
// option assembly, source selection, the Excel sheet-name and row/column limit
// checks (mirroring the Rust `excel` module EXACTLY so the dialog surfaces the
// same rejection the backend would, immediately and offline), and grid-width
// derivation for the export. Everything here is synchronous and side-effect
// free so it can be unit-tested without a backend.

import type {
  ExcelColumnWidths,
  ExcelExportOptions,
  ExcelHeaderMode,
  ExcelImportOptions,
  ExcelWorkbookInfo,
  ExportScope,
} from "../types";

/** Excel's hard row limit (a worksheet has at most this many rows). */
export const EXCEL_MAX_ROWS = 1_048_576;
/** Excel's hard column limit. */
export const EXCEL_MAX_COLS = 16_384;
/** Longest sheet name Excel accepts. */
export const MAX_SHEET_NAME_LEN = 31;
/** Characters Excel forbids in a sheet name (mirrors `INVALID_SHEET_CHARS`). */
export const INVALID_SHEET_CHARS = ["[", "]", ":", "*", "?", "/", "\\"] as const;

// ---------------------------------------------------------------------------
// Import options
// ---------------------------------------------------------------------------

/** Which source an import reads from. */
export type ExcelSourceKind = "sheet" | "table" | "namedRange";

/** A chosen import source in the open chooser. */
export interface ExcelSource {
  kind: ExcelSourceKind;
  name: string;
}

/** The per-source import UI state the dialog edits (before it becomes options). */
export interface ExcelImportUi {
  header: ExcelHeaderMode;
  merged: ExcelImportOptions["merged"];
  formula: ExcelImportOptions["formula"];
  /** `A1` range text — sheet source only; empty means the whole used range. */
  range: string;
  trimBlankRows: boolean;
  trimBlankColumns: boolean;
  forceIndexed: boolean;
}

/** Sensible defaults for the per-source import controls (match the Rust defaults). */
export function defaultImportUi(): ExcelImportUi {
  return {
    header: { type: "firstRow" },
    merged: "topLeftOnly",
    formula: "cachedResult",
    range: "",
    trimBlankRows: false,
    trimBlankColumns: false,
    forceIndexed: false,
  };
}

/**
 * Assemble the wire `ExcelImportOptions` for a chosen source and UI state. A
 * sheet source carries its optional `A1` range; a table's header is intrinsic
 * (the backend ignores the header mode); a named range is a fixed region.
 */
export function buildImportOptions(source: ExcelSource, ui: ExcelImportUi): ExcelImportOptions {
  const base: ExcelImportOptions = {
    header: ui.header,
    merged: ui.merged,
    formula: ui.formula,
    trimBlankRows: ui.trimBlankRows,
    trimBlankColumns: ui.trimBlankColumns,
    forceIndexed: ui.forceIndexed,
  };
  if (source.kind === "table") {
    // A table's own column names are the header; the header mode does not apply.
    return { ...base, table: source.name, header: { type: "firstRow" } };
  }
  if (source.kind === "namedRange") {
    return { ...base, namedRange: source.name };
  }
  const range = ui.range.trim();
  return { ...base, sheet: source.name, range: range === "" ? undefined : range };
}

/**
 * The detected header-row candidate offsets to surface as one-click chips.
 *
 * The backend computes these offsets relative to the sheet's whole used-range
 * start (they index the rows the import reads when the WHOLE used range is
 * scanned). As soon as a custom A1 sub-range is entered, the import reads from
 * that sub-range's origin instead, so a sheet-relative candidate offset no
 * longer lines up — applying it would mis-select the header row (or error).
 * We therefore surface candidates only for a sheet source with no custom range
 * (and never for a table/named-range source, whose header is intrinsic/fixed).
 * The manual "Row N" control, whose index is region-relative by construction,
 * still works against any chosen range.
 */
export function headerCandidateChips(
  source: ExcelSource | null,
  sheetCandidates: number[] | undefined,
  rangeText: string,
): number[] {
  if (!source || source.kind !== "sheet") return [];
  if (rangeText.trim() !== "") return [];
  return sheetCandidates ?? [];
}

/**
 * Pick the source the chooser should select by default: the first visible
 * worksheet that has data, else the first worksheet with data, else the first
 * table (data beats an empty sheet), else the first worksheet, else null (an
 * empty workbook).
 */
export function defaultSource(info: ExcelWorkbookInfo): ExcelSource | null {
  const worksheets = info.sheets.filter((s) => s.kind === "worksheet");
  const visibleWithData = worksheets.find((s) => s.visibility === "visible" && s.hasData);
  if (visibleWithData) return { kind: "sheet", name: visibleWithData.name };
  const withData = worksheets.find((s) => s.hasData);
  if (withData) return { kind: "sheet", name: withData.name };
  if (info.tables.length > 0) return { kind: "table", name: info.tables[0].name };
  if (worksheets.length > 0) return { kind: "sheet", name: worksheets[0].name };
  return null;
}

/** Human label for a sheet's visibility. */
export function visibilityLabel(visibility: string): string {
  switch (visibility) {
    case "visible":
      return "Visible";
    case "hidden":
      return "Hidden";
    case "veryHidden":
      return "Very hidden";
    default:
      return visibility;
  }
}

/** Human label for a sheet's kind. */
export function sheetKindLabel(kind: string): string {
  switch (kind) {
    case "worksheet":
      return "Worksheet";
    case "dialog":
      return "Dialog sheet";
    case "macro":
      return "Macro sheet";
    case "chart":
      return "Chart sheet";
    case "vba":
      return "VBA";
    default:
      return kind;
  }
}

/**
 * Whether an `A1` range string is well-formed enough to send. An empty string
 * is valid (it means "the whole used range"). Accepts a single cell (`B2`) or a
 * `start:end` pair, each with optional `$` anchors — mirroring the shapes the
 * backend's `parse_a1_range` accepts, so a doomed range disables the import
 * before the invoke.
 */
export function isValidA1Range(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed === "") return true;
  const parts = trimmed.split(":");
  if (parts.length > 2) return false;
  return parts.every((p) => isA1Cell(p.trim()));
}

function isA1Cell(cell: string): boolean {
  return /^\$?[A-Za-z]{1,3}\$?[0-9]{1,7}$/.test(cell);
}

// ---------------------------------------------------------------------------
// Export: sheet names
// ---------------------------------------------------------------------------

/**
 * Validate one Excel sheet name (mirrors the backend `validate_sheet_name`):
 * non-blank, at most 31 characters, no forbidden characters, no leading/trailing
 * apostrophe. Returns an error message, or null when the name is acceptable.
 * Uniqueness is checked across the set by {@link validateSheetNames}.
 */
export function validateSheetName(name: string): string | null {
  if (name.trim() === "") return "A sheet name must not be blank.";
  if ([...name].length > MAX_SHEET_NAME_LEN) {
    return `"${name}" is longer than Excel's ${MAX_SHEET_NAME_LEN}-character limit.`;
  }
  const bad = [...name].find((c) => (INVALID_SHEET_CHARS as readonly string[]).includes(c));
  if (bad !== undefined) {
    return `"${name}" contains the character '${bad}', which Excel forbids in a sheet name.`;
  }
  if (name.startsWith("'") || name.endsWith("'")) {
    return `"${name}" must not start or end with an apostrophe.`;
  }
  return null;
}

/** One rejected sheet name in the export set. */
export interface SheetNameIssue {
  /** Index of the offending sheet in the input list. */
  index: number;
  name: string;
  message: string;
}

/**
 * Validate every export sheet name and reject case-insensitive duplicates
 * (Excel sheet names are unique regardless of case). Returns one issue per
 * offending sheet, in order.
 */
export function validateSheetNames(names: string[]): SheetNameIssue[] {
  const issues: SheetNameIssue[] = [];
  const seen = new Map<string, number>();
  names.forEach((name, index) => {
    const nameError = validateSheetName(name);
    if (nameError) {
      issues.push({ index, name, message: nameError });
      return;
    }
    const key = name.toLowerCase();
    const first = seen.get(key);
    if (first !== undefined) {
      issues.push({
        index,
        name,
        message: `Two sheets are both named "${name}" — Excel sheet names must be unique.`,
      });
      return;
    }
    seen.set(key, index);
  });
  return issues;
}

/**
 * Turn an arbitrary string (usually a file name) into a valid, non-empty Excel
 * sheet name: strip the extension, drop forbidden characters, trim apostrophe
 * edges and whitespace, clamp to 31 characters, and fall back to `"Sheet"`.
 */
export function sanitizeSheetName(raw: string): string {
  const stem = raw.replace(/\.[^.\\/]+$/, "");
  let cleaned = [...stem]
    .filter((c) => !(INVALID_SHEET_CHARS as readonly string[]).includes(c))
    .join("");
  cleaned = cleaned.replace(/^'+|'+$/g, "").trim();
  if ([...cleaned].length > MAX_SHEET_NAME_LEN) {
    cleaned = [...cleaned].slice(0, MAX_SHEET_NAME_LEN).join("").trim();
  }
  return cleaned === "" ? "Sheet" : cleaned;
}

/**
 * De-duplicate a list of sheet names case-insensitively, appending ` (2)`,
 * ` (3)`, … to later collisions and re-clamping to Excel's length limit, so a
 * multi-tab export starts from a writable set.
 */
export function dedupeSheetNames(names: string[]): string[] {
  const seen = new Set<string>();
  return names.map((name) => {
    let candidate = name;
    let n = 2;
    while (seen.has(candidate.toLowerCase())) {
      const suffix = ` (${n})`;
      const room = MAX_SHEET_NAME_LEN - suffix.length;
      const base = [...name].slice(0, Math.max(0, room)).join("");
      candidate = `${base}${suffix}`;
      n++;
    }
    seen.add(candidate.toLowerCase());
    return candidate;
  });
}

// ---------------------------------------------------------------------------
// Export: row/column limits
// ---------------------------------------------------------------------------

/** The projected size of one export sheet, before any byte is written. */
export interface SheetSizing {
  name: string;
  /** Data rows the scope resolves to (the header is added separately). */
  dataRows: number;
  columns: number;
  hasHeader: boolean;
}

/** One sheet that would exceed an Excel dimension limit. */
export interface LimitViolation {
  name: string;
  kind: "rows" | "columns";
  actual: number;
  limit: number;
  message: string;
}

/**
 * Check every export sheet against Excel's hard limits (1,048,576 rows ×
 * 16,384 columns), counting the header row toward the row total exactly as the
 * backend's `plan_export` does. Returns one violation per offending sheet; an
 * empty array means the export is within limits. The backend re-checks and is
 * the authority, but this refuses an over-limit export up front.
 */
export function checkExportLimits(sheets: SheetSizing[]): LimitViolation[] {
  const violations: LimitViolation[] = [];
  for (const s of sheets) {
    const outRows = s.dataRows + (s.hasHeader ? 1 : 0);
    if (outRows > EXCEL_MAX_ROWS) {
      violations.push({
        name: s.name,
        kind: "rows",
        actual: outRows,
        limit: EXCEL_MAX_ROWS,
        message: `Sheet "${s.name}" would have ${outRows.toLocaleString()} rows, over Excel's limit of ${EXCEL_MAX_ROWS.toLocaleString()}. Export fewer rows or split the document.`,
      });
    }
    if (s.columns > EXCEL_MAX_COLS) {
      violations.push({
        name: s.name,
        kind: "columns",
        actual: s.columns,
        limit: EXCEL_MAX_COLS,
        message: `Sheet "${s.name}" would have ${s.columns.toLocaleString()} columns, over Excel's limit of ${EXCEL_MAX_COLS.toLocaleString()}.`,
      });
    }
  }
  return violations;
}

// ---------------------------------------------------------------------------
// Export: scope → sizing and grid widths
// ---------------------------------------------------------------------------

/** Dimensions of a document, as the export sizing needs them. */
export interface DocDimensions {
  /** Total data rows (unfiltered). */
  totalRows: number;
  /** Rows currently visible (after a filter / view). */
  visibleRows: number;
  columns: number;
  hasHeader: boolean;
}

/**
 * Project a scope onto a document to get the sheet's output size. Mirrors the
 * shape of `export_scope::resolve_scope`: `all` writes every row, `visibleRows`
 * and `selectedColumns` the filtered/view rows, and the row/range selections
 * scope their own extents.
 */
export function sizingForScope(name: string, scope: ExportScope, dims: DocDimensions): SheetSizing {
  switch (scope.type) {
    case "all":
      return { name, dataRows: dims.totalRows, columns: dims.columns, hasHeader: dims.hasHeader };
    case "visibleRows":
      return { name, dataRows: dims.visibleRows, columns: dims.columns, hasHeader: dims.hasHeader };
    case "selectedRows":
      return {
        name,
        dataRows: scope.rows.length,
        columns: dims.columns,
        hasHeader: dims.hasHeader,
      };
    case "selectedColumns":
      // The backend exports the VISIBLE rows of the selected columns, so a
      // filtered view whose subset fits Excel's limit must not be blocked on
      // the unfiltered total.
      return {
        name,
        dataRows: dims.visibleRows,
        columns: scope.columns.length,
        hasHeader: dims.hasHeader,
      };
    case "selectedRange":
      return {
        name,
        dataRows: scope.rect.height,
        columns: scope.rect.width,
        hasHeader: dims.hasHeader,
      };
  }
}

/**
 * The source column indices a scope writes, in output order. `all` /
 * `visibleRows` / `selectedRows` write every column; the column and range
 * scopes write their own subset. Used to align grid widths to output columns.
 */
export function outputColumnsForScope(scope: ExportScope, columnCount: number): number[] {
  switch (scope.type) {
    case "selectedColumns":
      return [...scope.columns];
    case "selectedRange": {
      const out: number[] = [];
      for (let c = scope.rect.x; c < scope.rect.x + scope.rect.width; c++) out.push(c);
      return out;
    }
    default: {
      const out: number[] = [];
      for (let c = 0; c < columnCount; c++) out.push(c);
      return out;
    }
  }
}

/**
 * Per-output-column pixel widths for the `grid` column-width option, aligned to
 * the sheet's output columns. A column with no recorded width contributes `0`,
 * which the backend treats as "leave the default" (it only sets positive
 * widths).
 */
export function gridWidthsPx(
  columnWidths: Record<number, number>,
  outputColumns: number[],
): number[] {
  return outputColumns.map((c) => {
    const w = columnWidths[c];
    return typeof w === "number" && w > 0 ? Math.round(w) : 0;
  });
}

// ---------------------------------------------------------------------------
// Export: options + labels
// ---------------------------------------------------------------------------

/** Sensible defaults for a fresh Excel export (match the Rust `Default`). */
export function defaultExportOptions(): ExcelExportOptions {
  return {
    headerStyle: true,
    freezeHeader: true,
    autofilter: false,
    columnWidths: "default",
    typed: true,
    backup: "none",
  };
}

const COLUMN_WIDTH_LABELS: Record<ExcelColumnWidths, string> = {
  default: "Default width",
  autofit: "Autofit to contents",
  grid: "Match the current grid",
};

export function columnWidthsLabel(width: ExcelColumnWidths): string {
  return COLUMN_WIDTH_LABELS[width];
}

/** Suggested `.xlsx` file name from a source name (extension replaced). */
export function suggestExcelFileName(base: string): string {
  const stem = base.replace(/\.[^.\\/]+$/, "");
  return `${stem}.xlsx`;
}
