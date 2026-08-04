import { describe, expect, it } from "vitest";

import {
  buildImportOptions,
  checkExportLimits,
  dedupeSheetNames,
  defaultExportOptions,
  defaultImportUi,
  defaultSource,
  EXCEL_MAX_COLS,
  EXCEL_MAX_ROWS,
  gridWidthsPx,
  headerCandidateChips,
  isValidA1Range,
  outputColumnsForScope,
  sanitizeSheetName,
  sizingForScope,
  suggestExcelFileName,
  validateSheetName,
  validateSheetNames,
  type ExcelSource,
} from "./excel";
import type { ExcelWorkbookInfo } from "../types";

// ----- import option assembly ------------------------------------------------

describe("buildImportOptions", () => {
  const ui = { ...defaultImportUi(), merged: "repeat" as const, formula: "formulaText" as const };

  it("carries a sheet source's trimmed A1 range, dropping an empty one", () => {
    const withRange = buildImportOptions(
      { kind: "sheet", name: "Data" },
      { ...ui, range: " B2:F9 " },
    );
    expect(withRange.sheet).toBe("Data");
    expect(withRange.range).toBe("B2:F9");
    expect(withRange.table).toBeUndefined();
    expect(withRange.namedRange).toBeUndefined();

    const noRange = buildImportOptions({ kind: "sheet", name: "Data" }, { ...ui, range: "   " });
    expect(noRange.range).toBeUndefined();
    // The merged/formula policies still ride along.
    expect(noRange.merged).toBe("repeat");
    expect(noRange.formula).toBe("formulaText");
  });

  it("forces the intrinsic header for a table source and never sends a range", () => {
    const opts = buildImportOptions(
      { kind: "table", name: "Sales" },
      { ...ui, range: "B2:F9", header: { type: "none" } },
    );
    expect(opts.table).toBe("Sales");
    expect(opts.header).toEqual({ type: "firstRow" });
    expect(opts.range).toBeUndefined();
    expect(opts.sheet).toBeUndefined();
  });

  it("passes a named-range source through with its chosen header mode", () => {
    const opts = buildImportOptions(
      { kind: "namedRange", name: "Region" },
      { ...ui, header: { type: "row", index: 2 } },
    );
    expect(opts.namedRange).toBe("Region");
    expect(opts.header).toEqual({ type: "row", index: 2 });
    expect(opts.sheet).toBeUndefined();
  });
});

// ----- default source selection ----------------------------------------------

function workbook(partial: Partial<ExcelWorkbookInfo>): ExcelWorkbookInfo {
  return {
    has1904Epoch: false,
    sheets: [],
    tables: [],
    namedRanges: [],
    warnings: [],
    ...partial,
  };
}

function sheet(name: string, visibility: string, hasData: boolean, kind = "worksheet") {
  return {
    name,
    visibility,
    kind,
    hasData,
    startRow: 0,
    startCol: 0,
    usedRows: hasData ? 10 : 0,
    usedCols: hasData ? 3 : 0,
    formulaCount: 0,
    mergedCount: 0,
    formulasWithoutCachedResults: 0,
    headerCandidates: [],
    previewRows: [],
  };
}

describe("defaultSource", () => {
  it("prefers the first visible worksheet with data", () => {
    const info = workbook({
      sheets: [
        sheet("Hidden", "hidden", true),
        sheet("Empty", "visible", false),
        sheet("Data", "visible", true),
      ],
    });
    expect(defaultSource(info)).toEqual({ kind: "sheet", name: "Data" });
  });

  it("falls back to any worksheet with data, then a data table, then any worksheet", () => {
    expect(defaultSource(workbook({ sheets: [sheet("H", "hidden", true)] }))).toEqual({
      kind: "sheet",
      name: "H",
    });
    // A data-bearing table beats an empty worksheet.
    expect(
      defaultSource(
        workbook({
          sheets: [sheet("Blank", "visible", false)],
          tables: [{ name: "T", sheet: "Blank", columns: ["a"], rows: 3, range: "A2:A4" }],
        }),
      ),
    ).toEqual({ kind: "table", name: "T" });
    // With no data anywhere, an (empty) worksheet is still selectable last.
    expect(defaultSource(workbook({ sheets: [sheet("Blank", "visible", false)] }))).toEqual({
      kind: "sheet",
      name: "Blank",
    });
    expect(
      defaultSource(
        workbook({ tables: [{ name: "T", sheet: "S", columns: ["a"], rows: 3, range: "A2:A4" }] }),
      ),
    ).toEqual({ kind: "table", name: "T" });
  });

  it("returns null for an empty workbook", () => {
    expect(defaultSource(workbook({}))).toBeNull();
  });
});

// ----- detected header-row chips ---------------------------------------------

describe("headerCandidateChips", () => {
  it("surfaces the sheet's candidates for a whole-used-range sheet source", () => {
    expect(headerCandidateChips({ kind: "sheet", name: "Data" }, [0, 2], "")).toEqual([0, 2]);
    // Whitespace-only range still counts as "the whole used range".
    expect(headerCandidateChips({ kind: "sheet", name: "Data" }, [1], "   ")).toEqual([1]);
  });

  it("hides candidates once a custom A1 range is entered (offsets no longer align)", () => {
    // The candidate offsets are sheet-used-range-relative; a sub-range shifts
    // the import origin, so applying them would mis-select the header row.
    expect(headerCandidateChips({ kind: "sheet", name: "Data" }, [0, 2], "B5:F99")).toEqual([]);
  });

  it("never surfaces candidates for a table or named-range source", () => {
    expect(headerCandidateChips({ kind: "table", name: "T" }, [0, 2], "")).toEqual([]);
    expect(headerCandidateChips({ kind: "namedRange", name: "R" }, [0, 2], "")).toEqual([]);
    expect(headerCandidateChips(null, [0, 2], "")).toEqual([]);
  });

  it("tolerates an undefined candidate list", () => {
    expect(headerCandidateChips({ kind: "sheet", name: "Data" }, undefined, "")).toEqual([]);
  });
});

// ----- A1 range validation ---------------------------------------------------

describe("isValidA1Range", () => {
  it("accepts an empty range (the whole used range)", () => {
    expect(isValidA1Range("")).toBe(true);
    expect(isValidA1Range("   ")).toBe(true);
  });

  it("accepts single cells and ranges with optional anchors", () => {
    expect(isValidA1Range("A1")).toBe(true);
    expect(isValidA1Range("B2:F100")).toBe(true);
    expect(isValidA1Range("$B$2:$F$100")).toBe(true);
    expect(isValidA1Range(" b2 : f9 ")).toBe(true);
  });

  it("rejects malformed ranges", () => {
    expect(isValidA1Range("A")).toBe(false);
    expect(isValidA1Range("1")).toBe(false);
    expect(isValidA1Range("A1:B2:C3")).toBe(false);
    expect(isValidA1Range("Sheet1!A1")).toBe(false);
  });
});

// ----- sheet-name validation -------------------------------------------------

describe("validateSheetName", () => {
  it("accepts an ordinary name", () => {
    expect(validateSheetName("Sales 2024")).toBeNull();
  });

  it("rejects blank, over-long, forbidden-char and apostrophe-edge names", () => {
    expect(validateSheetName("  ")).toMatch(/blank/);
    expect(validateSheetName("x".repeat(32))).toMatch(/31-character/);
    expect(validateSheetName("Q1/Q2")).toMatch(/forbids/);
    expect(validateSheetName("Jan:Feb")).toMatch(/forbids/);
    expect(validateSheetName("'quoted")).toMatch(/apostrophe/);
    expect(validateSheetName("quoted'")).toMatch(/apostrophe/);
  });

  it("counts characters, not UTF-16 units, for the length limit", () => {
    // 31 astral-plane emoji: 31 characters (62 UTF-16 units) — still valid.
    expect(validateSheetName("😀".repeat(31))).toBeNull();
    expect(validateSheetName("😀".repeat(32))).toMatch(/31-character/);
  });
});

describe("validateSheetNames", () => {
  it("flags case-insensitive duplicates", () => {
    const issues = validateSheetNames(["Data", "Notes", "data"]);
    expect(issues).toHaveLength(1);
    expect(issues[0].index).toBe(2);
    expect(issues[0].message).toMatch(/unique/);
  });

  it("returns no issues for a clean, distinct set", () => {
    expect(validateSheetNames(["A", "B", "C"])).toEqual([]);
  });
});

describe("sanitizeSheetName / dedupeSheetNames", () => {
  it("strips the extension, forbidden characters and apostrophe edges", () => {
    expect(sanitizeSheetName("report/final.csv")).toBe("reportfinal");
    expect(sanitizeSheetName("'quoted'.xlsx")).toBe("quoted");
  });

  it("clamps to 31 characters and never yields an empty name", () => {
    expect([...sanitizeSheetName("a".repeat(50))].length).toBe(31);
    expect(sanitizeSheetName("[]:*?/\\")).toBe("Sheet");
    expect(sanitizeSheetName("data.csv.xlsx")).toBe("data.csv");
  });

  it("appends numeric suffixes to case-insensitive collisions", () => {
    expect(dedupeSheetNames(["Data", "Data", "data"])).toEqual(["Data", "Data (2)", "data (3)"]);
  });
});

// ----- export limit checks (mirror plan_export) ------------------------------

describe("checkExportLimits", () => {
  it("passes sheets within the limits", () => {
    expect(
      checkExportLimits([{ name: "S", dataRows: 1000, columns: 20, hasHeader: true }]),
    ).toEqual([]);
  });

  it("counts the header toward the row limit", () => {
    // Exactly at the row limit WITH a header: data rows = MAX - 1 is fine…
    expect(
      checkExportLimits([{ name: "S", dataRows: EXCEL_MAX_ROWS - 1, columns: 1, hasHeader: true }]),
    ).toEqual([]);
    // …but MAX data rows plus a header overflows by one.
    const over = checkExportLimits([
      { name: "S", dataRows: EXCEL_MAX_ROWS, columns: 1, hasHeader: true },
    ]);
    expect(over).toHaveLength(1);
    expect(over[0].kind).toBe("rows");
    expect(over[0].actual).toBe(EXCEL_MAX_ROWS + 1);
  });

  it("flags over-wide sheets", () => {
    const over = checkExportLimits([
      { name: "Wide", dataRows: 1, columns: EXCEL_MAX_COLS + 1, hasHeader: false },
    ]);
    expect(over).toHaveLength(1);
    expect(over[0].kind).toBe("columns");
    expect(over[0].message).toMatch(/16,384/);
  });

  it("reports each offending sheet independently", () => {
    const over = checkExportLimits([
      { name: "OK", dataRows: 5, columns: 5, hasHeader: true },
      { name: "TallWide", dataRows: EXCEL_MAX_ROWS, columns: EXCEL_MAX_COLS + 1, hasHeader: true },
    ]);
    expect(over).toHaveLength(2);
    expect(over.every((v) => v.name === "TallWide")).toBe(true);
  });
});

// ----- scope → sizing / output columns / grid widths -------------------------

const dims = { totalRows: 100, visibleRows: 30, columns: 4, hasHeader: true };

describe("sizingForScope", () => {
  it("maps each scope to its data-row and column extents", () => {
    expect(sizingForScope("S", { type: "all" }, dims)).toMatchObject({ dataRows: 100, columns: 4 });
    expect(sizingForScope("S", { type: "visibleRows" }, dims)).toMatchObject({ dataRows: 30 });
    expect(sizingForScope("S", { type: "selectedRows", rows: [1, 2, 3] }, dims)).toMatchObject({
      dataRows: 3,
      columns: 4,
    });
    expect(sizingForScope("S", { type: "selectedColumns", columns: [0, 2] }, dims)).toMatchObject({
      dataRows: 30,
      columns: 2,
    });
    expect(
      sizingForScope(
        "S",
        { type: "selectedRange", rect: { x: 1, y: 5, width: 2, height: 9 } },
        dims,
      ),
    ).toMatchObject({ dataRows: 9, columns: 2 });
  });

  it("counts only the visible rows for selected columns under a filter", () => {
    // A million-plus-row document filtered down to a subset within Excel's
    // limit: exporting selected columns must size on the visible rows (what
    // the backend writes), so the up-front limit check does not block it.
    const filtered = {
      totalRows: EXCEL_MAX_ROWS + 500,
      visibleRows: 10,
      columns: 4,
      hasHeader: true,
    };
    const sizing = sizingForScope("S", { type: "selectedColumns", columns: [0, 2] }, filtered);
    expect(sizing).toMatchObject({ dataRows: 10, columns: 2 });
    expect(checkExportLimits([sizing])).toEqual([]);
  });
});

describe("outputColumnsForScope", () => {
  it("returns every column for row scopes and the subset for column/range scopes", () => {
    expect(outputColumnsForScope({ type: "all" }, 3)).toEqual([0, 1, 2]);
    expect(outputColumnsForScope({ type: "selectedColumns", columns: [2, 0] }, 3)).toEqual([2, 0]);
    expect(
      outputColumnsForScope(
        { type: "selectedRange", rect: { x: 1, y: 0, width: 2, height: 4 } },
        5,
      ),
    ).toEqual([1, 2]);
  });
});

describe("gridWidthsPx", () => {
  it("aligns widths to output columns, using 0 for unknown or non-positive widths", () => {
    expect(gridWidthsPx({ 0: 80, 2: 120.6 }, [0, 1, 2])).toEqual([80, 0, 121]);
    expect(gridWidthsPx({ 2: 100, 0: -5 }, [2, 0])).toEqual([100, 0]);
  });
});

// ----- misc ------------------------------------------------------------------

describe("defaults & file name", () => {
  it("mirrors the Rust export defaults", () => {
    expect(defaultExportOptions()).toEqual({
      headerStyle: true,
      freezeHeader: true,
      autofilter: false,
      columnWidths: "default",
      typed: true,
      backup: "none",
    });
  });

  it("suggests an .xlsx name from a source name", () => {
    expect(suggestExcelFileName("customers.csv")).toBe("customers.xlsx");
    expect(suggestExcelFileName("book")).toBe("book.xlsx");
  });
});

// A tiny type-level anchor so an ExcelSource typo fails the build.
const _src: ExcelSource = { kind: "sheet", name: "x" };
void _src;
