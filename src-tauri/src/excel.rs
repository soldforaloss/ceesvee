//! F34: Excel `.xlsx` interoperability — reading workbooks into CEESVEE
//! documents and producing workbooks from open documents. This is NOT a
//! formula engine: formulas are read as their cached result, their text, or
//! blanked; nothing is evaluated, and there is no in-place `.xlsx` save (an
//! import always yields a fresh CEESVEE document — the UI states this).
//!
//! ## Reading (calamine)
//!
//! [`inspect`] is the OPEN CHOOSER: it lists every sheet (including hidden and
//! very-hidden ones), named tables, named ranges, per-sheet used ranges and
//! approximate dimensions, formula and merged-cell counts, detected header-row
//! candidates and a bounded preview. [`preview`] scans the SELECTED source
//! (sheet / named table / named range, optionally a cell sub-range) under the
//! chosen options and reports the resulting columns, sample rows, projected
//! dimensions and warnings. [`import`] runs the same scan and streams the cells
//! into a CEESVEE document through the shared [`DerivedDocumentBuilder`]
//! pipeline (an in-memory editable document for small results, an indexed
//! read-only one for large ones or when `forceIndexed` is set).
//!
//! Cell handling honours the workbook's declared date system
//! (`workbookPr@date1904`, surfaced by [`calamine::Xlsx::has_1904_epoch`]) — a
//! date cell carries the workbook epoch inside its [`calamine::ExcelDateTime`],
//! so 1904-epoch dates are NOT shifted and the Excel 1900 leap-year bug is
//! reproduced exactly (both are calamine's responsibility; we never re-derive
//! serial dates ourselves). Text cells stay text — a leading-zero string such
//! as a ZIP code is a string cell and is never numeric-coerced.
//!
//! Merged cells are handled per [`MergedPolicy`]: `topLeftOnly` mirrors Excel's
//! own storage (the value lives only in the top-left cell; the rest are blank),
//! `repeat` fills every cell of a merged region with the top-left value.
//! Formulas are handled per [`FormulaPolicy`]: `cachedResult` uses the cached
//! `<v>` value (blank when Excel stored none), `formulaText` emits the formula
//! source prefixed with `=`, `blank` drops it. A workbook that has formulas but
//! no cached results is flagged so the UI can warn (evaluating them is out of
//! scope).
//!
//! ## Writing (rust_xlsxwriter)
//!
//! [`export`] writes one sheet from one document, or several sheets (one per
//! open tab) into a single workbook. Values only — never formulas. Optional
//! bold+filled header styling, a frozen header row, an autofilter over the used
//! range and column widths (from the caller's grid widths or autofit). With
//! `typed`, columns carrying an F31 schema export real typed values
//! (integers/decimals/floats as numbers, booleans as booleans, dates/datetimes
//! as Excel dates); text stays text and any cell that is invalid under its
//! declared schema falls back to text. Excel's hard limits (1,048,576 rows ×
//! 16,384 columns) are validated BEFORE a byte is written, and the workbook is
//! committed through the F03 atomic-save pipeline, so a failure or cancellation
//! never touches an existing destination.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use calamine::{
    open_workbook, Data, DataType, Dimensions, Reader, Sheet, SheetType, SheetVisible, Xlsx,
    XlsxError,
};
use chrono::{Datelike, Timelike};
use rust_xlsxwriter::{Color, ExcelDateTime, Format, FormatPattern, Workbook};
use serde::{Deserialize, Serialize};

use crate::derived::DerivedDocumentBuilder;
use crate::document::Document;
use crate::dto::{ExcelColumnWidths, ExcelExportOptions, ExportScope};
use crate::error::{AppError, AppResult};
use crate::export_scope;
use crate::job::JobCtx;
use crate::save;
use crate::schema::{self, CellState, ColumnSchema, LogicalType, TypedValue};

/// Excel's hard row limit (a worksheet has at most this many rows).
pub const EXCEL_MAX_ROWS: u64 = 1_048_576;
/// Excel's hard column limit.
pub const EXCEL_MAX_COLS: u64 = 16_384;
/// Longest sheet name Excel accepts.
const MAX_SHEET_NAME_LEN: usize = 31;
/// Characters Excel forbids in a sheet name.
const INVALID_SHEET_CHARS: [char; 7] = ['[', ']', ':', '*', '?', '/', '\\'];

/// Rows shown in a chooser sheet preview.
const PREVIEW_ROWS: usize = 20;
/// Columns shown in a chooser sheet preview (wide sheets are clipped).
const PREVIEW_COLS: usize = 40;
/// Rows scanned for header-row candidates.
const HEADER_SCAN_ROWS: u32 = 25;
/// Most header-row candidates reported per sheet.
const MAX_HEADER_CANDIDATES: usize = 5;
/// Data rows retained by [`preview`].
pub const SAMPLE_ROWS: usize = 50;
/// Cooperative-cancellation cadence (cells scanned / rows written).
const CANCEL_EVERY_ROWS: usize = 1024;

// ---------------------------------------------------------------------------
// Import options (wire DTOs, camelCase)
// ---------------------------------------------------------------------------

/// Which row of the selected region is the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HeaderMode {
    /// The first row of the region is the header.
    #[default]
    FirstRow,
    /// The row at this 0-based offset within the region is the header; rows
    /// above it are dropped (they are title/notes rows), rows below are data.
    Row { index: u32 },
    /// No header row — columns get synthetic `Column N` names and every row is
    /// data.
    None,
}

/// How merged cells are imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MergedPolicy {
    /// Keep the value only in the top-left cell (Excel's own storage); the rest
    /// are blank.
    #[default]
    TopLeftOnly,
    /// Repeat the top-left value across every cell of the merged region.
    Repeat,
}

/// How formula cells are imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormulaPolicy {
    /// Use the cached result Excel stored (blank when it stored none).
    #[default]
    CachedResult,
    /// Emit the formula source text, prefixed with `=`.
    FormulaText,
    /// Emit a blank cell for every formula.
    Blank,
}

/// Everything an import (preview or apply) needs to know.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExcelImportOptions {
    /// Sheet to import (required unless `table` or `namedRange` is given).
    pub sheet: Option<String>,
    /// Named table to import (its parent sheet and range are resolved
    /// automatically; the table's own column names become the header).
    pub table: Option<String>,
    /// Named range to import (resolved to a sheet and a cell range).
    pub named_range: Option<String>,
    /// A1-style cell range within `sheet` (e.g. `"B2:F100"`); ignored for a
    /// table or named-range source.
    pub range: Option<String>,
    pub header: HeaderMode,
    pub merged: MergedPolicy,
    pub formula: FormulaPolicy,
    /// Drop rows that are entirely empty within the selection.
    pub trim_blank_rows: bool,
    /// Drop columns that are entirely empty within the selection.
    pub trim_blank_columns: bool,
    /// Spill straight to the indexed read-only backing instead of size-based
    /// auto-selection.
    pub force_indexed: bool,
}

// ---------------------------------------------------------------------------
// Inspection DTOs (chooser)
// ---------------------------------------------------------------------------

/// One sheet in the open chooser.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetInfo {
    pub name: String,
    /// `"visible"`, `"hidden"` or `"veryHidden"`.
    pub visibility: String,
    /// `"worksheet"`, `"dialog"`, `"macro"`, `"chart"` or `"vba"`.
    pub kind: String,
    /// Whether the sheet holds tabular data (only worksheets do).
    pub has_data: bool,
    /// 0-based row/column of the used range's top-left cell.
    pub start_row: u32,
    pub start_col: u32,
    /// Used-range height/width (approximate dimensions).
    pub used_rows: u32,
    pub used_cols: u32,
    pub formula_count: u64,
    pub merged_count: u64,
    /// Formula cells for which Excel stored no cached result.
    pub formulas_without_cached_results: u64,
    /// Detected header-row candidates, as 0-based offsets from the used-range
    /// start.
    pub header_candidates: Vec<u32>,
    /// Bounded preview of the cached cell values (top-left corner).
    pub preview_rows: Vec<Vec<String>>,
}

/// One named table in the open chooser.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub sheet: String,
    pub columns: Vec<String>,
    pub rows: u64,
    /// A1 range of the table body (headers excluded), when it has any rows.
    pub range: Option<String>,
}

/// One named range in the open chooser.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedRangeInfo {
    pub name: String,
    /// The raw defined-name formula (e.g. `Sheet1!$A$1:$C$9`).
    pub formula: String,
    /// The sheet the range resolves to, when it is a simple single-area range.
    pub sheet: Option<String>,
    pub range: Option<String>,
}

/// Everything the OPEN CHOOSER needs to render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookInfo {
    /// Whether the workbook uses the 1904 date epoch.
    pub has_1904_epoch: bool,
    pub sheets: Vec<SheetInfo>,
    pub tables: Vec<TableInfo>,
    pub named_ranges: Vec<NamedRangeInfo>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Import preview DTOs (chosen source + options)
// ---------------------------------------------------------------------------

/// One column of an import preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewColumn {
    pub name: String,
    pub inferred_type: LogicalType,
    /// Non-empty data cells in the column.
    pub non_empty: u64,
    /// Empty data cells in the column.
    pub empty: u64,
}

/// The preview of importing the selected source under the chosen options.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcelImportPreview {
    pub has_1904_epoch: bool,
    /// Human-readable description of what was scanned (sheet + range / table).
    pub source: String,
    pub has_header_row: bool,
    pub columns: Vec<PreviewColumn>,
    pub row_count: u64,
    pub column_count: usize,
    pub sample_rows: Vec<Vec<String>>,
    /// Formula cells with no cached result that landed in the selection.
    pub formulas_without_cached_results: u64,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Export source (options live in dto.rs alongside the other export options)
// ---------------------------------------------------------------------------

/// One sheet of an export: the document it reads, the sheet name, the scope and
/// the revision it was prepared against.
pub struct SheetSource<'a> {
    pub doc: &'a Document,
    pub name: String,
    pub scope: ExportScope,
    pub expected_revision: u64,
    /// Per-output-column pixel widths (only used with [`ExcelColumnWidths::Grid`]).
    pub grid_widths_px: Option<Vec<f64>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_err(e: XlsxError) -> AppError {
    AppError::Other(format!("could not read the Excel workbook: {e}"))
}

fn write_err(e: rust_xlsxwriter::XlsxError) -> AppError {
    AppError::Other(format!("could not write the Excel workbook: {e}"))
}

fn open(path: &Path) -> AppResult<Xlsx<std::io::BufReader<std::fs::File>>> {
    open_workbook(path).map_err(read_err)
}

fn visibility_name(v: SheetVisible) -> &'static str {
    match v {
        SheetVisible::Visible => "visible",
        SheetVisible::Hidden => "hidden",
        SheetVisible::VeryHidden => "veryHidden",
    }
}

fn sheet_kind_name(t: SheetType) -> &'static str {
    match t {
        SheetType::WorkSheet => "worksheet",
        SheetType::DialogSheet => "dialog",
        SheetType::MacroSheet => "macro",
        SheetType::ChartSheet => "chart",
        SheetType::Vba => "vba",
    }
}

/// The five distinguishable kinds a source cell contributes, so a column can be
/// type-inferred without losing leading-zero text to numeric coercion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellKind {
    Empty,
    Text,
    Int,
    Float,
    Bool,
    DateTime,
}

/// Convert a 0-based column index to A1 letters (0 → `A`, 26 → `AA`).
fn col_to_letters(mut col: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}

/// A1 cell reference (`(row, col)`, both 0-based).
fn a1_cell(row: u32, col: u32) -> String {
    format!("{}{}", col_to_letters(col), row + 1)
}

/// A1 range for a `(start, end)` inclusive rectangle.
fn a1_range(start: (u32, u32), end: (u32, u32)) -> String {
    format!("{}:{}", a1_cell(start.0, start.1), a1_cell(end.0, end.1))
}

/// An inclusive `(start, end)` cell rectangle, each corner `(row, col)` 0-based.
type A1Rect = ((u32, u32), (u32, u32));

fn col_letters_to_index(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut idx: u32 = 0;
    for ch in s.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        let v = ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1;
        idx = idx.checked_mul(26)?.checked_add(v)?;
    }
    idx.checked_sub(1)
}

/// Parse one A1 cell (`"B2"`, `"$B$2"`) to `(row, col)`, both 0-based.
fn parse_a1_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    let col = col_letters_to_index(letters)?;
    let row: u32 = digits.parse().ok()?;
    row.checked_sub(1).map(|r| (r, col))
}

/// Parse an A1 range (`"B2:F100"` or a single cell) to a normalised inclusive
/// `(start, end)` rectangle.
fn parse_a1_range(s: &str) -> AppResult<A1Rect> {
    let s = s.trim();
    let bad = || AppError::invalid(format!("\"{s}\" is not a valid A1 cell range"));
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a, b),
        None => (s, s),
    };
    let a = parse_a1_cell(a.trim()).ok_or_else(bad)?;
    let b = parse_a1_cell(b.trim()).ok_or_else(bad)?;
    Ok(((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))))
}

/// Parse a defined-name formula into `(sheet, range)` when it is a single
/// contiguous area on one sheet (`Sheet1!$A$1:$C$9`, `'My Sheet'!$B$2`).
fn parse_defined_name(formula: &str) -> Option<(String, A1Rect)> {
    let formula = formula.trim().trim_start_matches('=');
    // Multiple areas (comma-separated) or names/functions are not resolvable.
    if formula.contains(',') {
        return None;
    }
    let (sheet_part, range_part) = formula.rsplit_once('!')?;
    let sheet = sheet_part.trim();
    let sheet = if let Some(inner) = sheet.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        inner.replace("''", "'")
    } else {
        sheet.to_string()
    };
    let range = parse_a1_range(range_part).ok()?;
    Some((sheet, range))
}

/// Render a float cell without an exponent for whole numbers (`3.0` → `"3"`).
fn format_float(f: f64) -> String {
    if f == f.trunc() && f.is_finite() && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// Format an Excel datetime honouring the workbook epoch, WITHOUT shifting or
/// silently "correcting" dates. The epoch (1900 vs 1904) is baked into the
/// [`calamine::ExcelDateTime`] by the reader, and [`ExcelDateTime::to_ymd_hms_milli`]
/// decomposes the serial faithfully — including Excel's phantom 1900-02-29
/// leap-year bug (serial 60), which `chrono` cannot represent, so we format the
/// components as text directly rather than round-tripping through a `NaiveDate`.
fn format_excel_datetime(edt: &calamine::ExcelDateTime) -> String {
    if edt.is_duration() {
        // A `[hh]:mm:ss` elapsed-time cell: render the elapsed clock.
        if let Some(d) = edt.as_duration() {
            let secs = d.num_seconds().max(0);
            let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
            return format!("{h}:{m:02}:{s:02}");
        }
    }
    let (y, mo, d, h, mi, s, _ms) = edt.to_ymd_hms_milli();
    // A pure date (no time component) renders as a date; otherwise a datetime.
    if h == 0 && mi == 0 && s == 0 {
        format!("{y:04}-{mo:02}-{d:02}")
    } else {
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    }
}

/// Turn one calamine cell into `(text, kind)`.
fn data_to_cell(d: &Data) -> (String, CellKind) {
    match d {
        Data::Empty => (String::new(), CellKind::Empty),
        Data::String(s) => {
            let kind = if s.is_empty() {
                CellKind::Empty
            } else {
                CellKind::Text
            };
            (s.clone(), kind)
        }
        Data::Int(i) => (i.to_string(), CellKind::Int),
        Data::Float(f) => (format_float(*f), CellKind::Float),
        Data::Bool(b) => (
            if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellKind::Bool,
        ),
        Data::DateTime(edt) => (format_excel_datetime(edt), CellKind::DateTime),
        // ISO date/duration strings are already text; keep them verbatim.
        Data::DateTimeIso(s) => (s.clone(), CellKind::Text),
        Data::DurationIso(s) => (s.clone(), CellKind::Text),
        Data::Error(e) => (e.to_string(), CellKind::Text),
    }
}

// ---------------------------------------------------------------------------
// Inspection (the OPEN CHOOSER)
// ---------------------------------------------------------------------------

/// Inspect a workbook: sheets (with visibility), tables, named ranges, used
/// ranges, formula and merged-cell counts, header candidates and a bounded
/// preview per sheet. Read-only — nothing is imported.
pub fn inspect(path: &Path, ctx: Option<&JobCtx>) -> AppResult<WorkbookInfo> {
    let mut wb = open(path)?;
    let has_1904 = wb.has_1904_epoch();
    let sheets_meta: Vec<Sheet> = wb.sheets_metadata().to_vec();
    let defined: Vec<(String, String)> = wb.defined_names().to_vec();

    // Named tables (best effort: a workbook without any is fine).
    let mut tables = Vec::new();
    if wb.load_tables().is_ok() {
        let names: Vec<String> = wb.table_names().into_iter().cloned().collect();
        for name in names {
            if let Ok(t) = wb.table_by_name(&name) {
                let data = t.data();
                let (rows, range) = match (data.start(), data.end()) {
                    (Some(s), Some(e)) => (data.height() as u64, Some(a1_range(s, e))),
                    _ => (0, None),
                };
                tables.push(TableInfo {
                    name: t.name().to_string(),
                    sheet: t.sheet_name().to_string(),
                    columns: t.columns().to_vec(),
                    rows,
                    range,
                });
            }
        }
    }

    let named_ranges: Vec<NamedRangeInfo> = defined
        .into_iter()
        .map(|(name, formula)| {
            let (sheet, range) = match parse_defined_name(&formula) {
                Some((s, (start, end))) => (Some(s), Some(a1_range(start, end))),
                None => (None, None),
            };
            NamedRangeInfo {
                name,
                formula,
                sheet,
                range,
            }
        })
        .collect();

    let mut sheets = Vec::with_capacity(sheets_meta.len());
    let mut total_uncached = 0u64;
    for meta in &sheets_meta {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let is_worksheet = meta.typ == SheetType::WorkSheet;
        let range = if is_worksheet {
            wb.worksheet_range(&meta.name).ok()
        } else {
            None
        };
        let formulas = if is_worksheet {
            wb.worksheet_formula(&meta.name).ok()
        } else {
            None
        };
        let merges = if is_worksheet {
            wb.merge_cells_by_sheet_name(&meta.name).unwrap_or_default()
        } else {
            Vec::new()
        };

        let (start_row, start_col, used_rows, used_cols, preview_rows, header_candidates) =
            match &range {
                Some(r) => {
                    let (sr, sc) = r.start().unwrap_or((0, 0));
                    (
                        sr,
                        sc,
                        r.height() as u32,
                        r.width() as u32,
                        preview_of(r),
                        header_candidates(r),
                    )
                }
                None => (0, 0, 0, 0, Vec::new(), Vec::new()),
            };

        let formula_count = formulas
            .as_ref()
            .map(|f| f.used_cells().filter(|(_, _, s)| !s.is_empty()).count() as u64)
            .unwrap_or(0);
        let uncached = match (&range, &formulas) {
            (Some(values), Some(f)) => f
                .used_cells()
                .filter(|(r, c, formula)| {
                    !formula.is_empty()
                        && values
                            .get_value((*r as u32, *c as u32))
                            .map(|d| d.is_empty())
                            .unwrap_or(true)
                })
                .count() as u64,
            _ => 0,
        };
        total_uncached += uncached;

        sheets.push(SheetInfo {
            name: meta.name.clone(),
            visibility: visibility_name(meta.visible).to_string(),
            kind: sheet_kind_name(meta.typ).to_string(),
            // A worksheet with no used cells still yields `Some(range)` whose
            // extents are empty; that is not importable data, so gate on the
            // used dimensions rather than the mere presence of a range.
            has_data: used_rows > 0 && used_cols > 0,
            start_row,
            start_col,
            used_rows,
            used_cols,
            formula_count,
            merged_count: merges.len() as u64,
            formulas_without_cached_results: uncached,
            header_candidates,
            preview_rows,
        });
    }

    let mut warnings = Vec::new();
    if total_uncached > 0 {
        warnings.push(format!(
            "{total_uncached} formula cell(s) have no cached result; under the default formula \
             policy they import blank because CEESVEE does not evaluate formulas"
        ));
    }

    Ok(WorkbookInfo {
        has_1904_epoch: has_1904,
        sheets,
        tables,
        named_ranges,
        warnings,
    })
}

/// Bounded top-left preview of a sheet range.
fn preview_of(range: &calamine::Range<Data>) -> Vec<Vec<String>> {
    range
        .rows()
        .take(PREVIEW_ROWS)
        .map(|row| {
            row.iter()
                .take(PREVIEW_COLS)
                .map(|cell| data_to_cell(cell).0)
                .collect()
        })
        .collect()
}

/// Header-row candidates: rows near the top whose non-empty cells are all text
/// (a numeric/date row is data, not a header). Offsets are relative to the
/// range start.
fn header_candidates(range: &calamine::Range<Data>) -> Vec<u32> {
    let mut out = Vec::new();
    for (offset, row) in range.rows().take(HEADER_SCAN_ROWS as usize).enumerate() {
        let mut any = false;
        let mut all_text = true;
        for cell in row {
            match cell {
                Data::Empty => {}
                Data::String(s) if !s.is_empty() => any = true,
                Data::String(_) => {}
                _ => {
                    any = true;
                    all_text = false;
                }
            }
        }
        if any && all_text {
            out.push(offset as u32);
            if out.len() >= MAX_HEADER_CANDIDATES {
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Materialisation (shared by preview and import)
// ---------------------------------------------------------------------------

/// The imported grid plus the facts preview and import both need.
struct Grid {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    has_header: bool,
    col_types: Vec<LogicalType>,
    source: String,
    has_1904: bool,
    uncached: u64,
    warnings: Vec<String>,
}

/// A resolved source region on one sheet, plus any table-supplied headers.
struct Region {
    sheet: String,
    start: (u32, u32),
    end: (u32, u32),
    /// `Some` when the source is a table (its column names are authoritative
    /// and the header row is intrinsic).
    table_headers: Option<Vec<String>>,
    source_label: String,
}

/// Resolve the import options to a concrete sheet region.
fn resolve_region(
    wb: &mut Xlsx<std::io::BufReader<std::fs::File>>,
    opts: &ExcelImportOptions,
) -> AppResult<Region> {
    if let Some(table) = &opts.table {
        wb.load_tables()
            .map_err(|e| AppError::invalid(format!("this workbook has no readable tables: {e}")))?;
        let t = wb.table_by_name(table).map_err(|_| {
            AppError::invalid(format!("no table named \"{table}\" in this workbook"))
        })?;
        let sheet = t.sheet_name().to_string();
        let headers = t.columns().to_vec();
        let data = t.data();
        let (start, end) = match (data.start(), data.end()) {
            // The header row sits directly above the table body.
            (Some((dr, dc)), Some((er, ec))) => ((dr.saturating_sub(1), dc), (er, ec)),
            // A table with no body rows: read just its header row if we can
            // place it, otherwise fall back to header-only.
            _ => {
                return Ok(Region {
                    sheet,
                    start: (0, 0),
                    end: (0, 0),
                    table_headers: Some(headers),
                    source_label: format!("table \"{table}\""),
                });
            }
        };
        return Ok(Region {
            source_label: format!("table \"{table}\""),
            sheet,
            start,
            end,
            table_headers: Some(headers),
        });
    }

    if let Some(name) = &opts.named_range {
        let formula = wb
            .defined_names()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| f.clone())
            .ok_or_else(|| {
                AppError::invalid(format!("no named range \"{name}\" in this workbook"))
            })?;
        let (sheet, (start, end)) = parse_defined_name(&formula).ok_or_else(|| {
            AppError::invalid(format!(
                "the named range \"{name}\" ({formula}) is not a single contiguous cell range"
            ))
        })?;
        return Ok(Region {
            source_label: format!("named range \"{name}\""),
            sheet,
            start,
            end,
            table_headers: None,
        });
    }

    let sheet = opts
        .sheet
        .clone()
        .ok_or_else(|| AppError::invalid("choose a sheet, table or named range to import"))?;
    let range = wb.worksheet_range(&sheet).map_err(|_| {
        AppError::invalid(format!("no worksheet named \"{sheet}\" in this workbook"))
    })?;
    let (used_start, used_end) = match (range.start(), range.end()) {
        (Some(s), Some(e)) => (s, e),
        _ => return Err(AppError::invalid(format!("the sheet \"{sheet}\" is empty"))),
    };
    let (start, end, label) = match &opts.range {
        Some(a1) => {
            let (rs, re) = parse_a1_range(a1)?;
            // Clamp the requested range to the used range.
            let start = (rs.0.max(used_start.0), rs.1.max(used_start.1));
            let end = (re.0.min(used_end.0), re.1.min(used_end.1));
            if start.0 > end.0 || start.1 > end.1 {
                return Err(AppError::invalid(format!(
                    "the range {a1} does not overlap any data on sheet \"{sheet}\""
                )));
            }
            (start, end, format!("sheet \"{sheet}\" range {a1}"))
        }
        None => (used_start, used_end, format!("sheet \"{sheet}\"")),
    };
    Ok(Region {
        sheet,
        start,
        end,
        table_headers: None,
        source_label: label,
    })
}

/// The materialised cells of a region: `(cells, per-cell kinds, uncached-formula count)`.
type RegionCells = (Vec<Vec<String>>, Vec<Vec<CellKind>>, u64);

/// Read one region into a string grid, honouring the formula and merged-cell
/// policies. Returns `(cells, kinds, uncached)`.
fn read_region(
    values: &calamine::Range<Data>,
    formulas: Option<&calamine::Range<String>>,
    merges: &[Dimensions],
    region: &Region,
    opts: &ExcelImportOptions,
    ctx: Option<&JobCtx>,
) -> AppResult<RegionCells> {
    let (r0, c0) = region.start;
    let (r1, c1) = region.end;
    let n_cols = (c1 - c0 + 1) as usize;
    let n_rows = (r1 - r0 + 1) as usize;

    // Compute one cell's (text, kind), applying the formula policy but NOT the
    // merged-repeat override (which is layered on afterwards).
    let cell_at = |r: u32, c: u32| -> (String, CellKind, bool, bool) {
        let is_formula = formulas
            .and_then(|f| f.get_value((r, c)))
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let cached_empty = values
            .get_value((r, c))
            .map(|d| d.is_empty())
            .unwrap_or(true);
        if is_formula {
            match opts.formula {
                FormulaPolicy::FormulaText => {
                    let text = formulas
                        .and_then(|f| f.get_value((r, c)))
                        .cloned()
                        .unwrap_or_default();
                    (format!("={text}"), CellKind::Text, true, cached_empty)
                }
                FormulaPolicy::Blank => (String::new(), CellKind::Empty, true, cached_empty),
                FormulaPolicy::CachedResult => {
                    let (text, kind) = values
                        .get_value((r, c))
                        .map(data_to_cell)
                        .unwrap_or((String::new(), CellKind::Empty));
                    (text, kind, true, cached_empty)
                }
            }
        } else {
            let (text, kind) = values
                .get_value((r, c))
                .map(data_to_cell)
                .unwrap_or((String::new(), CellKind::Empty));
            (text, kind, false, false)
        }
    };

    let mut cells: Vec<Vec<String>> = Vec::with_capacity(n_rows);
    let mut kinds: Vec<Vec<CellKind>> = Vec::with_capacity(n_rows);
    let mut uncached = 0u64;
    let mut processed = 0usize;
    for r in r0..=r1 {
        let mut row_cells = Vec::with_capacity(n_cols);
        let mut row_kinds = Vec::with_capacity(n_cols);
        for c in c0..=c1 {
            let (text, kind, is_formula, cached_empty) = cell_at(r, c);
            if is_formula && cached_empty {
                uncached += 1;
            }
            row_cells.push(text);
            row_kinds.push(kind);
            processed += 1;
            if processed.is_multiple_of(CANCEL_EVERY_ROWS) {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
            }
        }
        cells.push(row_cells);
        kinds.push(row_kinds);
    }

    // Merged repeat: overwrite every covered cell within the selection with the
    // top-left cell's computed string (and kind).
    if opts.merged == MergedPolicy::Repeat {
        for dim in merges {
            let (tr, tc) = dim.start;
            // Skip regions that do not touch the selection at all.
            if dim.end.0 < r0 || dim.start.0 > r1 || dim.end.1 < c0 || dim.start.1 > c1 {
                continue;
            }
            let (text, kind, _, _) = cell_at(tr, tc);
            for r in dim.start.0..=dim.end.0 {
                for c in dim.start.1..=dim.end.1 {
                    if r < r0 || r > r1 || c < c0 || c > c1 {
                        continue;
                    }
                    let ri = (r - r0) as usize;
                    let ci = (c - c0) as usize;
                    cells[ri][ci] = text.clone();
                    kinds[ri][ci] = kind;
                }
            }
        }
    }

    Ok((cells, kinds, uncached))
}

/// Infer a logical type for a column from its data-cell kinds. A single text
/// cell keeps the column text (leading-zero protection); an all-numeric column
/// is integer or float; homogeneous boolean / datetime columns get their type.
fn infer_column(kinds: &[CellKind]) -> LogicalType {
    let mut non_empty = 0;
    let mut saw_text = false;
    let mut saw_float = false;
    let mut saw_int = false;
    let mut saw_bool = false;
    let mut saw_dt = false;
    for &k in kinds {
        match k {
            CellKind::Empty => {}
            CellKind::Text => {
                saw_text = true;
                non_empty += 1;
            }
            CellKind::Int => {
                saw_int = true;
                non_empty += 1;
            }
            CellKind::Float => {
                saw_float = true;
                non_empty += 1;
            }
            CellKind::Bool => {
                saw_bool = true;
                non_empty += 1;
            }
            CellKind::DateTime => {
                saw_dt = true;
                non_empty += 1;
            }
        }
    }
    if non_empty == 0 || saw_text {
        return LogicalType::Text;
    }
    let numeric = saw_int || saw_float;
    match (numeric, saw_bool, saw_dt) {
        (true, false, false) => {
            if saw_float {
                LogicalType::Float
            } else {
                LogicalType::Integer
            }
        }
        (false, true, false) => LogicalType::Boolean,
        (false, false, true) => LogicalType::Datetime,
        _ => LogicalType::Text,
    }
}

/// Materialise the selected source into a [`Grid`] (the shared core of
/// [`preview`] and [`import`]).
fn materialize(path: &Path, opts: &ExcelImportOptions, ctx: Option<&JobCtx>) -> AppResult<Grid> {
    let mut wb = open(path)?;
    let has_1904 = wb.has_1904_epoch();
    let region = resolve_region(&mut wb, opts)?;

    let values = wb
        .worksheet_range(&region.sheet)
        .map_err(|_| AppError::invalid(format!("could not read sheet \"{}\"", region.sheet)))?;
    let formulas = wb.worksheet_formula(&region.sheet).ok();
    let merges = if opts.merged == MergedPolicy::Repeat {
        wb.merge_cells_by_sheet_name(&region.sheet)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // An empty-body table degrades to a header-only import.
    let region_has_data = region.start.0 <= region.end.0 && region.start.1 <= region.end.1;
    let (mut cells, mut kinds, uncached) = if region_has_data {
        read_region(&values, formulas.as_ref(), &merges, &region, opts, ctx)?
    } else {
        (Vec::new(), Vec::new(), 0)
    };

    // Header extraction.
    let (headers, has_header) = if let Some(table_headers) = region.table_headers {
        // Tables: the top region row IS the header row; drop it, use the
        // table's authoritative column names.
        if region_has_data && !cells.is_empty() {
            cells.remove(0);
            kinds.remove(0);
        }
        (table_headers, true)
    } else {
        match opts.header {
            HeaderMode::None => {
                let n = cells.first().map(Vec::len).unwrap_or(0);
                ((0..n).map(|i| format!("Column {}", i + 1)).collect(), false)
            }
            HeaderMode::FirstRow => {
                if cells.is_empty() {
                    (Vec::new(), true)
                } else {
                    let headers = cells.remove(0);
                    kinds.remove(0);
                    (headers, true)
                }
            }
            HeaderMode::Row { index } => {
                let index = index as usize;
                if index >= cells.len() {
                    return Err(AppError::invalid(format!(
                        "the chosen header row {index} is outside the selected range"
                    )));
                }
                let headers = cells[index].clone();
                // Drop the header row and everything above it (title/notes rows).
                cells.drain(0..=index);
                kinds.drain(0..=index);
                (headers, true)
            }
        }
    };

    let mut headers = headers;
    let mut n_cols = headers.len().max(cells.first().map(Vec::len).unwrap_or(0));
    // Pad every row and the header to a rectangular width.
    headers.resize(n_cols, String::new());
    for row in &mut cells {
        row.resize(n_cols, String::new());
    }
    for row in &mut kinds {
        row.resize(n_cols, CellKind::Empty);
    }

    // Blank-column trimming: drop columns empty in the header AND every data row.
    if opts.trim_blank_columns && n_cols > 0 {
        let keep: Vec<bool> = (0..n_cols)
            .map(|c| !headers[c].is_empty() || cells.iter().any(|row| !row[c].is_empty()))
            .collect();
        if keep.iter().any(|&k| !k) {
            headers = filter_by(&headers, &keep);
            for row in &mut cells {
                *row = filter_by(row, &keep);
            }
            for row in &mut kinds {
                *row = filter_by(row, &keep);
            }
            n_cols = headers.len();
        }
    }

    // Blank-row trimming: drop data rows that are entirely empty.
    if opts.trim_blank_rows {
        let mut kept_kinds = Vec::with_capacity(cells.len());
        let mut kept_cells = Vec::with_capacity(cells.len());
        for (row, krow) in cells.into_iter().zip(kinds) {
            if row.iter().any(|c| !c.is_empty()) {
                kept_cells.push(row);
                kept_kinds.push(krow);
            }
        }
        cells = kept_cells;
        kinds = kept_kinds;
    }

    if n_cols == 0 {
        return Err(AppError::invalid(
            "the selection has no columns to import (everything was blank or trimmed away)",
        ));
    }

    // Column type inference from the data cells' kinds.
    let col_types: Vec<LogicalType> = (0..n_cols)
        .map(|c| infer_column(&kinds.iter().map(|row| row[c]).collect::<Vec<_>>()))
        .collect();

    let mut warnings = Vec::new();
    if uncached > 0 {
        warnings.push(format!(
            "{uncached} formula cell(s) in the selection have no cached result; CEESVEE does not \
             evaluate formulas, so they import blank under the cached-result policy"
        ));
    }

    Ok(Grid {
        headers,
        rows: cells,
        has_header,
        col_types,
        source: region.source_label,
        has_1904,
        uncached,
        warnings,
    })
}

fn filter_by<T: Clone>(items: &[T], keep: &[bool]) -> Vec<T> {
    items
        .iter()
        .zip(keep)
        .filter(|(_, &k)| k)
        .map(|(v, _)| v.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// Preview importing the selected source under the chosen options: columns with
/// inferred types and counts, sample rows, projected dimensions and warnings.
pub fn preview(
    path: &Path,
    opts: &ExcelImportOptions,
    ctx: Option<&JobCtx>,
) -> AppResult<ExcelImportPreview> {
    let grid = materialize(path, opts, ctx)?;
    let n_cols = grid.headers.len();
    let columns: Vec<PreviewColumn> = (0..n_cols)
        .map(|c| {
            let non_empty = grid.rows.iter().filter(|row| !row[c].is_empty()).count() as u64;
            PreviewColumn {
                name: grid.headers[c].clone(),
                inferred_type: grid.col_types[c],
                non_empty,
                empty: grid.rows.len() as u64 - non_empty,
            }
        })
        .collect();
    let sample_rows: Vec<Vec<String>> = grid.rows.iter().take(SAMPLE_ROWS).cloned().collect();
    Ok(ExcelImportPreview {
        has_1904_epoch: grid.has_1904,
        source: grid.source,
        has_header_row: grid.has_header,
        columns,
        row_count: grid.rows.len() as u64,
        column_count: n_cols,
        sample_rows,
        formulas_without_cached_results: grid.uncached,
        warnings: grid.warnings,
    })
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Import the selected source into a CEESVEE document through the standard
/// derived-document pipeline (in-memory editable for small results, indexed
/// read-only for large ones or when `forceIndexed` is set). Inferred column
/// schemas are attached so typed numbers/dates round-trip; text — including
/// leading-zero codes — stays text. The document is created fresh and marked
/// unsaved (dirty); the original workbook is never modified.
pub fn import(
    path: &Path,
    opts: &ExcelImportOptions,
    cache_root: &Path,
    doc_id: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<Document> {
    if let Some(ctx) = ctx {
        ctx.set_message("reading the workbook");
    }
    let grid = materialize(path, opts, ctx)?;
    if let Some(ctx) = ctx {
        ctx.set_total(grid.rows.len() as u64);
        ctx.set_message("building the document");
    }

    let budget = if opts.force_indexed {
        0
    } else {
        crate::derived::SPILL_BUDGET
    };
    let mut builder =
        DerivedDocumentBuilder::new(grid.headers.clone(), cache_root.to_path_buf(), budget)
            .with_header_row(grid.has_header);
    let mut emitted = 0u64;
    for row in grid.rows {
        builder.push_row(row)?;
        emitted += 1;
        if emitted.is_multiple_of(CANCEL_EVERY_ROWS as u64) {
            if let Some(ctx) = ctx {
                ctx.advance(CANCEL_EVERY_ROWS as u64)?;
            }
        }
    }
    let mut doc = builder.finish(doc_id, &mut |_| match ctx {
        Some(ctx) => ctx.check(),
        None => Ok(()),
    })?;

    // Attach the inferred schema for non-text columns (numbers/dates/booleans).
    let ids = doc.column_ids().to_vec();
    let headers = doc.headers().to_vec();
    for (i, id) in ids.iter().enumerate() {
        let logical = grid.col_types.get(i).copied().unwrap_or(LogicalType::Text);
        if logical != LogicalType::Text {
            let name = headers.get(i).cloned().unwrap_or_default();
            doc.set_column_schema(ColumnSchema::new(id.clone(), name, logical));
        }
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Validate an Excel sheet name (length, forbidden characters, apostrophe
/// edges, non-blank). Uniqueness is checked by the caller across the set.
fn validate_sheet_name(name: &str) -> AppResult<()> {
    if name.trim().is_empty() {
        return Err(AppError::invalid("a sheet name must not be blank"));
    }
    if name.chars().count() > MAX_SHEET_NAME_LEN {
        return Err(AppError::invalid(format!(
            "the sheet name \"{name}\" is longer than Excel's {MAX_SHEET_NAME_LEN}-character limit"
        )));
    }
    if let Some(bad) = name.chars().find(|c| INVALID_SHEET_CHARS.contains(c)) {
        return Err(AppError::invalid(format!(
            "the sheet name \"{name}\" contains the character '{bad}', which Excel forbids"
        )));
    }
    if name.starts_with('\'') || name.ends_with('\'') {
        return Err(AppError::invalid(format!(
            "the sheet name \"{name}\" must not start or end with an apostrophe"
        )));
    }
    Ok(())
}

/// The output shape of one export sheet: absolute rows, columns and whether a
/// header row is written. Computed and limit-checked before any byte is
/// produced.
struct SheetPlan {
    name: String,
    rows: Vec<usize>,
    cols: Vec<usize>,
    has_header: bool,
}

/// Validate the whole export up front — revisions, scopes, sheet names and
/// Excel's row/column limits — so a bad request is refused BEFORE writing.
pub fn plan_export(sheets: &[SheetSource<'_>]) -> AppResult<()> {
    if sheets.is_empty() {
        return Err(AppError::invalid(
            "an Excel export needs at least one sheet",
        ));
    }
    let mut seen: Vec<String> = Vec::with_capacity(sheets.len());
    for s in sheets {
        s.doc.check_revision(s.expected_revision)?;
        validate_sheet_name(&s.name)?;
        let lower = s.name.to_lowercase();
        if seen.contains(&lower) {
            return Err(AppError::invalid(format!(
                "two export sheets are both named \"{}\" (Excel sheet names must be unique)",
                s.name
            )));
        }
        seen.push(lower);
        let resolved = export_scope::resolve_scope(s.doc, &s.scope)?;
        let has_header = s.doc.has_header_row();
        let out_rows = resolved.rows.len() as u64 + u64::from(has_header);
        let out_cols = resolved.cols.len() as u64;
        if out_rows > EXCEL_MAX_ROWS {
            return Err(AppError::invalid(format!(
                "sheet \"{}\" would have {out_rows} rows, over Excel's limit of {EXCEL_MAX_ROWS}; \
                 export fewer rows or split the document",
                s.name
            )));
        }
        if out_cols > EXCEL_MAX_COLS {
            return Err(AppError::invalid(format!(
                "sheet \"{}\" would have {out_cols} columns, over Excel's limit of {EXCEL_MAX_COLS}",
                s.name
            )));
        }
    }
    Ok(())
}

/// A date/datetime number format so typed date cells display as dates rather
/// than serial numbers.
fn date_format() -> Format {
    Format::new().set_num_format("yyyy-mm-dd")
}

fn datetime_format() -> Format {
    Format::new().set_num_format("yyyy-mm-dd hh:mm:ss")
}

/// Header styling: bold text on a light fill.
fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0x00D9_E1F2))
        .set_pattern(FormatPattern::Solid)
}

/// Try to write `cell` as a typed value under `schema`; `Ok(true)` when a typed
/// value was written, `Ok(false)` when the caller should fall back to text.
fn write_typed(
    ws: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    cell: &str,
    schema: &ColumnSchema,
    date_fmt: &Format,
    datetime_fmt: &Format,
) -> AppResult<bool> {
    match schema::classify(Some(cell), schema) {
        CellState::Valid(TypedValue::Integer(v)) => {
            // Only integers f64 can hold exactly; larger ones stay text.
            if v.unsigned_abs() <= (1u128 << 53) {
                ws.write_number(row, col, v as f64).map_err(write_err)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        CellState::Valid(TypedValue::Decimal(d)) => match d.to_plain_string().parse::<f64>() {
            Ok(f) if f.is_finite() => {
                ws.write_number(row, col, f).map_err(write_err)?;
                Ok(true)
            }
            _ => Ok(false),
        },
        CellState::Valid(TypedValue::Float(f)) => {
            ws.write_number(row, col, f).map_err(write_err)?;
            Ok(true)
        }
        CellState::Valid(TypedValue::Boolean(b)) => {
            ws.write_boolean(row, col, b).map_err(write_err)?;
            Ok(true)
        }
        CellState::Valid(TypedValue::Date(d)) => {
            // Excel dates live in 1900..=9999. A schema-typed year outside that
            // window (reachable when a custom input format parses a signed
            // out-of-range year, e.g. `+67536-01-01`) must fall back to text —
            // the `as u16` cast below truncates modulo 65536, which could
            // otherwise pass rust_xlsxwriter's own 1900..=9999 check as a
            // bogus-but-valid in-range date and silently corrupt the value.
            if !(1900..=9999).contains(&d.year()) {
                return Ok(false);
            }
            match ExcelDateTime::from_ymd(d.year() as u16, d.month() as u8, d.day() as u8) {
                Ok(edt) => {
                    ws.write_datetime_with_format(row, col, &edt, date_fmt)
                        .map_err(write_err)?;
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        }
        CellState::Valid(TypedValue::DateTime(dt)) => {
            // Same 1900..=9999 guard as the date branch, before the `as u16`
            // truncation, so an out-of-range year falls back to text.
            if !(1900..=9999).contains(&dt.year()) {
                return Ok(false);
            }
            let built = ExcelDateTime::from_ymd(dt.year() as u16, dt.month() as u8, dt.day() as u8)
                .and_then(|d| d.and_hms(dt.hour() as u16, dt.minute() as u8, dt.second() as f64));
            match built {
                Ok(edt) => {
                    ws.write_datetime_with_format(row, col, &edt, datetime_fmt)
                        .map_err(write_err)?;
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        }
        // Null/empty/missing → leave the cell blank; invalid → fall back to text.
        CellState::NullToken | CellState::Empty | CellState::Missing => Ok(true),
        CellState::Invalid(_) | CellState::Valid(_) => Ok(false),
    }
}

/// Write one document's slice into a fresh worksheet.
fn write_sheet(
    ws: &mut rust_xlsxwriter::Worksheet,
    source: &SheetSource<'_>,
    plan: &SheetPlan,
    options: &ExcelExportOptions,
    ctx: &JobCtx,
) -> AppResult<()> {
    ws.set_name(&plan.name).map_err(write_err)?;
    let headers = source.doc.headers().to_vec();
    let date_fmt = date_format();
    let datetime_fmt = datetime_format();
    let hdr_fmt = header_format();

    // Header row.
    let data_start: u32 = if plan.has_header {
        for (j, &c) in plan.cols.iter().enumerate() {
            let name = headers.get(c).cloned().unwrap_or_default();
            if options.header_style {
                ws.write_string_with_format(0, j as u16, name, &hdr_fmt)
                    .map_err(write_err)?;
            } else {
                ws.write_string(0, j as u16, name).map_err(write_err)?;
            }
        }
        1
    } else {
        0
    };

    // Per-column schemas (only consulted with `typed`).
    let schemas: Vec<Option<ColumnSchema>> = plan
        .cols
        .iter()
        .map(|&c| {
            if options.typed {
                source.doc.column_schema_at(c).cloned()
            } else {
                None
            }
        })
        .collect();

    let mut written = 0u32;
    let mut pending = 0u64;
    let mut err: Option<AppError> = None;
    source.doc.visit_rows_at(&plan.rows, &mut |_, row| {
        let out_row = data_start + written;
        for (j, &c) in plan.cols.iter().enumerate() {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            let col = j as u16;
            let typed_ok = match &schemas[j] {
                Some(schema) => {
                    match write_typed(ws, out_row, col, cell, schema, &date_fmt, &datetime_fmt) {
                        Ok(v) => v,
                        Err(e) => {
                            err = Some(e);
                            return Ok(false);
                        }
                    }
                }
                None => false,
            };
            if !typed_ok && !cell.is_empty() {
                // Default (and fallback) path: text stays text — leading zeros
                // and codes are preserved exactly.
                if let Err(e) = ws.write_string(out_row, col, cell) {
                    err = Some(write_err(e));
                    return Ok(false);
                }
            }
        }
        written += 1;
        pending += 1;
        if pending >= CANCEL_EVERY_ROWS as u64 {
            if let Err(e) = ctx.advance(pending) {
                err = Some(e);
                return Ok(false);
            }
            pending = 0;
        }
        Ok(true)
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    ctx.advance(pending)?;

    // Freeze the header row.
    if options.freeze_header && plan.has_header {
        ws.set_freeze_panes(1, 0).map_err(write_err)?;
    }
    // Autofilter over the used range (only meaningful with a header row).
    if options.autofilter && plan.has_header && !plan.cols.is_empty() {
        let last_row = if written == 0 {
            0
        } else {
            data_start + written - 1
        };
        ws.autofilter(0, 0, last_row, (plan.cols.len() - 1) as u16)
            .map_err(write_err)?;
    }
    // Column widths.
    match options.column_widths {
        ExcelColumnWidths::Default => {}
        ExcelColumnWidths::Autofit => {
            ws.autofit();
        }
        ExcelColumnWidths::Grid => {
            if let Some(widths) = &source.grid_widths_px {
                for (j, w) in widths.iter().enumerate().take(plan.cols.len()) {
                    if *w > 0.0 {
                        ws.set_column_width_pixels(j as u16, *w as u32)
                            .map_err(write_err)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Produce a workbook from one or more document slices and commit it atomically.
/// Revisions, scopes, sheet names and Excel's limits are validated first; the
/// workbook is built entirely in memory and only the final commit touches disk,
/// so a failure or cancellation leaves any existing destination untouched.
pub fn export(
    sheets: &[SheetSource<'_>],
    dest: &Path,
    options: &ExcelExportOptions,
    ctx: &JobCtx,
) -> AppResult<u64> {
    plan_export(sheets)?;

    // Resolve every sheet's plan (rows/cols) up front.
    let mut plans = Vec::with_capacity(sheets.len());
    let mut total_rows = 0u64;
    for s in sheets {
        s.doc.check_revision(s.expected_revision)?;
        let resolved = export_scope::resolve_scope(s.doc, &s.scope)?;
        total_rows += resolved.rows.len() as u64;
        plans.push(SheetPlan {
            name: s.name.clone(),
            rows: resolved.rows,
            cols: resolved.cols,
            has_header: s.doc.has_header_row(),
        });
    }
    ctx.set_total(total_rows);

    let mut workbook = Workbook::new();
    for (s, plan) in sheets.iter().zip(&plans) {
        ctx.check()?;
        let ws = workbook.add_worksheet();
        write_sheet(ws, s, plan, options, ctx)?;
    }

    ctx.check()?;
    let buffer = workbook.save_to_buffer().map_err(write_err)?;
    ctx.check()?;

    // Commit the finished workbook through the atomic-save pipeline.
    let bytes = save::atomic_write(dest, options.backup, |file| {
        file.write_all(&buffer)?;
        Ok(buffer.len() as u64)
    })?;
    ctx.add_bytes(bytes);
    ctx.flush_progress();
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Preview caches (fetched after the `job-finished` event)
// ---------------------------------------------------------------------------

/// Finished workbook inspections keyed by the job id that produced them.
#[derive(Default)]
pub struct ExcelInspectCache(Arc<Mutex<HashMap<u64, WorkbookInfo>>>);

impl ExcelInspectCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, WorkbookInfo>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, job_id: u64) -> Option<WorkbookInfo> {
        self.0.lock().ok()?.get(&job_id).cloned()
    }
}

/// Finished import previews keyed by the job id that produced them.
#[derive(Default)]
pub struct ExcelPreviewCache(Arc<Mutex<HashMap<u64, ExcelImportPreview>>>);

impl ExcelPreviewCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, ExcelImportPreview>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, job_id: u64) -> Option<ExcelImportPreview> {
        self.0.lock().ok()?.get(&job_id).cloned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::path::PathBuf;

    use calamine::{
        open_workbook as cal_open, Data, DataType, ExcelDateTime as CalDateTime, ExcelDateTimeType,
        Xlsx,
    };
    use rust_xlsxwriter::{Format as XFormat, Formula, Table, TableColumn, Workbook};

    use crate::document::Document;
    use crate::dto::{ExcelExportOptions, ExportScope};
    use crate::job::{JobCtx, JobRegistry};
    use crate::parse::{parse, ParseSettings};
    use crate::schema::{ColumnSchema, LogicalType};

    // ----- fixtures -----------------------------------------------------------

    fn book(
        build: impl FnOnce(&mut Workbook) -> Result<(), rust_xlsxwriter::XlsxError>,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.xlsx");
        let mut wb = Workbook::new();
        build(&mut wb).unwrap();
        wb.save(&path).unwrap();
        (dir, path)
    }

    fn sheet_opts(name: &str) -> ExcelImportOptions {
        ExcelImportOptions {
            sheet: Some(name.to_string()),
            ..Default::default()
        }
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let reg = JobRegistry::default();
        let ctx = reg.begin("test", None, |_| {});
        (reg, ctx)
    }

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    /// Rewrite one entry of an xlsx (a zip) in place, for scenarios rust_xlsxwriter
    /// cannot express directly (a formula with no cached `<v>`).
    fn patch_entry(src: &Path, dst: &Path, entry: &str, edit: impl Fn(&str) -> String) {
        let file = std::fs::File::open(src).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let out = std::fs::File::create(dst).unwrap();
        let mut writer = zip::ZipWriter::new(out);
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            drop(f);
            writer
                .start_file(name.clone(), zip::write::SimpleFileOptions::default())
                .unwrap();
            if name == entry {
                let patched = edit(&String::from_utf8(buf).unwrap());
                writer.write_all(patched.as_bytes()).unwrap();
            } else {
                writer.write_all(&buf).unwrap();
            }
        }
        writer.finish().unwrap();
    }

    fn import_grid(path: &Path, opts: &ExcelImportOptions) -> (tempfile::TempDir, Document) {
        let cache = tempfile::tempdir().unwrap();
        let doc = import(path, opts, cache.path(), 1, None).unwrap();
        (cache, doc)
    }

    // ----- date systems -------------------------------------------------------

    #[test]
    fn excel_1900_leap_bug_and_1904_epoch_are_honoured_without_shifting() {
        // Serial 60 is Excel's phantom 1900-02-29 (the 1900 leap-year bug); 61
        // is the real 1900-03-01.
        let leap = CalDateTime::new(60.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(format_excel_datetime(&leap), "1900-02-29");
        let after = CalDateTime::new(61.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(format_excel_datetime(&after), "1900-03-01");

        // The same serial reads as one date under the 1900 epoch and a DIFFERENT
        // (1462-days-later) date under the 1904 epoch — the epoch is honoured, so
        // a 1904 workbook's dates are not silently shifted to the 1900 reading.
        let serial = 43831.0; // 2020-01-01 under the 1900 system
        let d1900 = CalDateTime::new(serial, ExcelDateTimeType::DateTime, false);
        let d1904 = CalDateTime::new(serial, ExcelDateTimeType::DateTime, true);
        assert_eq!(format_excel_datetime(&d1900), "2020-01-01");
        assert_eq!(format_excel_datetime(&d1904), "2024-01-02");
        assert_ne!(format_excel_datetime(&d1900), format_excel_datetime(&d1904));
    }

    /// A cell written as a raw serial number carrying a date number-format, so
    /// calamine classifies it as a real `Data::DateTime` on the workbook epoch
    /// (the path `write_datetime` cannot express — it refuses pre-1900 serials
    /// like the leap-bug's serial 60).
    fn date_cell(ws: &mut rust_xlsxwriter::Worksheet, row: u32, col: u16, serial: f64) {
        let fmt = XFormat::new().set_num_format("yyyy-mm-dd");
        ws.write_number_with_format(row, col, serial, &fmt).unwrap();
    }

    #[test]
    fn real_date_cell_reproduces_1900_leap_bug_through_import() {
        // Serial 60 is Excel's phantom 1900-02-29. Drive it through the FULL
        // read pipeline (open_workbook → worksheet_range → data_to_cell →
        // materialize → import), not just the isolated formatter.
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "d")?;
            date_cell(ws, 1, 0, 60.0);
            date_cell(ws, 2, 0, 61.0);
            Ok(())
        });
        let (_cache, doc) = import_grid(&path, &sheet_opts("Sheet1"));
        assert_eq!(doc.headers(), &["d"]);
        let rows = doc.fetch_rows(&[0, 1]).unwrap();
        assert_eq!(rows[0][0], "1900-02-29", "phantom leap day survives import");
        assert_eq!(rows[1][0], "1900-03-01");
        // The column is inferred as a datetime and carries that schema.
        assert_eq!(
            doc.column_schema_at(0).map(|s| s.logical_type),
            Some(LogicalType::Datetime),
        );
    }

    #[test]
    fn workbook_1904_epoch_is_plumbed_through_inspect_and_import() {
        // Serial 43831 is 2020-01-01 under the default 1900 epoch.
        let (dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "d")?;
            date_cell(ws, 1, 0, 43831.0);
            Ok(())
        });

        // Baseline (default epoch): the flag is false and the date reads 2020.
        let info0 = inspect(&path, None).unwrap();
        assert!(!info0.has_1904_epoch);
        let (_c0, doc0) = import_grid(&path, &sheet_opts("Sheet1"));
        assert_eq!(doc0.fetch_rows(&[0]).unwrap()[0][0], "2020-01-01");

        // Inject the 1904 epoch flag exactly as a 1904 workbook stores it
        // (`<workbookPr date1904="1"/>`), leaving the serial untouched.
        let patched = dir.path().join("book1904.xlsx");
        patch_entry(&path, &patched, "xl/workbook.xml", |xml| {
            assert!(xml.contains("<workbookPr"), "workbook.xml has a workbookPr");
            xml.replace("<workbookPr", "<workbookPr date1904=\"1\"")
        });

        // The epoch flag is read from the real file …
        let info = inspect(&patched, None).unwrap();
        assert!(
            info.has_1904_epoch,
            "the 1904 epoch flag is plumbed from the workbook into WorkbookInfo"
        );
        // … and honoured, not shifted: the SAME serial now reads 1462 days
        // later (2024-01-02), never silently re-interpreted on the 1900 epoch.
        let (_c1, doc1) = import_grid(&patched, &sheet_opts("Sheet1"));
        assert_eq!(doc1.fetch_rows(&[0]).unwrap()[0][0], "2024-01-02");
    }

    // ----- leading-zero text + type inference ---------------------------------

    #[test]
    fn leading_zero_text_stays_text_and_numbers_infer_numeric() {
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "code")?;
            ws.write_string(0, 1, "qty")?;
            ws.write_string(1, 0, "00501")?; // a ZIP-like string cell
            ws.write_number(1, 1, 42)?;
            ws.write_string(2, 0, "00777")?;
            ws.write_number(2, 1, 7)?;
            Ok(())
        });
        let (_cache, doc) = import_grid(&path, &sheet_opts("Sheet1"));
        assert_eq!(doc.headers(), &["code", "qty"]);
        let rows = doc.fetch_rows(&[0, 1]).unwrap();
        assert_eq!(rows[0][0], "00501", "leading-zero text preserved exactly");
        assert_eq!(rows[0][1], "42");
        // The code column is text (never numeric-coerced); qty is numeric.
        assert!(doc.column_schema_at(0).is_none(), "code stays plain text");
        assert!(
            doc.column_schema_at(1)
                .map(|s| s.logical_type.is_numeric())
                .unwrap_or(false),
            "qty inferred numeric"
        );
    }

    // ----- merged-cell policies -----------------------------------------------

    fn merged_book() -> (tempfile::TempDir, PathBuf) {
        book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "a")?;
            ws.write_string(0, 1, "b")?;
            ws.write_string(0, 2, "c")?;
            ws.merge_range(1, 0, 1, 2, "MERGED", &XFormat::new())?;
            ws.write_string(2, 0, "x")?;
            ws.write_string(2, 1, "y")?;
            ws.write_string(2, 2, "z")?;
            Ok(())
        })
    }

    #[test]
    fn merged_top_left_only_blanks_the_rest() {
        let (_dir, path) = merged_book();
        let opts = ExcelImportOptions {
            merged: MergedPolicy::TopLeftOnly,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        let rows = doc.fetch_rows(&[0, 1]).unwrap();
        assert_eq!(
            rows[0],
            vec!["MERGED".to_string(), String::new(), String::new()]
        );
        assert_eq!(
            rows[1],
            vec!["x".to_string(), "y".to_string(), "z".to_string()]
        );
    }

    #[test]
    fn merged_repeat_fills_the_region() {
        let (_dir, path) = merged_book();
        let opts = ExcelImportOptions {
            merged: MergedPolicy::Repeat,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        let rows = doc.fetch_rows(&[0]).unwrap();
        assert_eq!(
            rows[0],
            vec![
                "MERGED".to_string(),
                "MERGED".to_string(),
                "MERGED".to_string()
            ]
        );
    }

    // ----- formula policies ---------------------------------------------------

    fn formula_book() -> (tempfile::TempDir, PathBuf) {
        book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "n")?;
            ws.write_string(0, 1, "doubled")?;
            ws.write_number(1, 0, 2)?;
            ws.write_formula(1, 1, Formula::new("A2*10").set_result("424242"))?;
            Ok(())
        })
    }

    #[test]
    fn formula_cached_result_policy_uses_the_cached_value() {
        let (_dir, path) = formula_book();
        let opts = ExcelImportOptions {
            formula: FormulaPolicy::CachedResult,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.fetch_rows(&[0]).unwrap()[0][1], "424242");
    }

    #[test]
    fn formula_text_policy_emits_the_source() {
        let (_dir, path) = formula_book();
        let opts = ExcelImportOptions {
            formula: FormulaPolicy::FormulaText,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.fetch_rows(&[0]).unwrap()[0][1], "=A2*10");
    }

    #[test]
    fn formula_blank_policy_drops_the_formula() {
        let (_dir, path) = formula_book();
        let opts = ExcelImportOptions {
            formula: FormulaPolicy::Blank,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.fetch_rows(&[0]).unwrap()[0][1], "");
    }

    #[test]
    fn formula_without_cached_result_is_flagged_and_imports_blank() {
        let (dir, path) = formula_book();
        // Strip the cached <v> so the formula cell has no stored result, exactly
        // as Excel/LibreOffice write it when the workbook was never recalculated.
        let patched = dir.path().join("no-cache.xlsx");
        patch_entry(&path, &patched, "xl/worksheets/sheet1.xml", |xml| {
            xml.replace("<v>424242</v>", "")
        });

        // The preview surfaces the no-cached-result warning flag.
        let (_reg, c) = ctx();
        let preview = preview(&patched, &sheet_opts("Sheet1"), Some(&c)).unwrap();
        assert_eq!(preview.formulas_without_cached_results, 1);
        assert!(preview
            .warnings
            .iter()
            .any(|w| w.contains("no cached result")));

        // And under the cached-result policy the cell imports blank.
        let (_cache, doc) = import_grid(&patched, &sheet_opts("Sheet1"));
        assert_eq!(doc.fetch_rows(&[0]).unwrap()[0][1], "");
    }

    // ----- range and table selection ------------------------------------------

    #[test]
    fn a1_range_selection_clips_to_the_requested_rectangle() {
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            for r in 0..5u32 {
                for col in 0..3u16 {
                    ws.write_string(r, col, format!("r{r}c{col}"))?;
                }
            }
            Ok(())
        });
        let opts = ExcelImportOptions {
            range: Some("A1:B3".to_string()),
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.headers(), &["r0c0", "r0c1"]);
        assert_eq!(doc.n_rows(), 2, "rows 2-3 of the range are data");
        assert_eq!(doc.n_cols(), 2, "column C is outside the range");
        assert_eq!(
            doc.fetch_rows(&[0]).unwrap()[0],
            vec!["r1c0".to_string(), "r1c1".to_string()]
        );
    }

    #[test]
    fn named_table_selection_uses_the_table_columns_and_body() {
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(1, 0, "Apple")?;
            ws.write_number(1, 1, 5)?;
            ws.write_string(2, 0, "Pear")?;
            ws.write_number(2, 1, 3)?;
            let table = Table::new().set_name("Inventory").set_columns(&[
                TableColumn::new().set_header("Item"),
                TableColumn::new().set_header("Qty"),
            ]);
            ws.add_table(0, 0, 2, 1, &table)?;
            Ok(())
        });
        let opts = ExcelImportOptions {
            table: Some("Inventory".to_string()),
            ..Default::default()
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.headers(), &["Item", "Qty"]);
        assert_eq!(doc.n_rows(), 2);
        assert_eq!(
            doc.fetch_rows(&[0]).unwrap()[0],
            vec!["Apple".to_string(), "5".to_string()]
        );
    }

    // ----- header modes + trimming --------------------------------------------

    #[test]
    fn chosen_header_row_drops_the_rows_above_it() {
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "Quarterly report")?; // title junk
            ws.write_string(1, 0, "id")?;
            ws.write_string(1, 1, "name")?;
            ws.write_string(2, 0, "1")?;
            ws.write_string(2, 1, "Ada")?;
            ws.write_string(3, 0, "2")?;
            ws.write_string(3, 1, "Bob")?;
            Ok(())
        });
        let opts = ExcelImportOptions {
            header: HeaderMode::Row { index: 1 },
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.headers(), &["id", "name"]);
        assert_eq!(doc.n_rows(), 2);
        assert_eq!(
            doc.fetch_rows(&[0]).unwrap()[0],
            vec!["1".to_string(), "Ada".to_string()]
        );
    }

    #[test]
    fn blank_rows_and_columns_are_trimmed() {
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "a")?; // column B header + body all blank
            ws.write_string(0, 2, "c")?;
            ws.write_string(1, 0, "1")?;
            ws.write_string(1, 2, "3")?;
            // row 2 entirely blank
            ws.write_string(3, 0, "4")?;
            ws.write_string(3, 2, "6")?;
            Ok(())
        });
        let opts = ExcelImportOptions {
            trim_blank_rows: true,
            trim_blank_columns: true,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert_eq!(doc.headers(), &["a", "c"], "empty column B removed");
        assert_eq!(doc.n_rows(), 2, "the all-blank row removed");
        let rows = doc.fetch_rows(&[0, 1]).unwrap();
        assert_eq!(rows[0], vec!["1".to_string(), "3".to_string()]);
        assert_eq!(rows[1], vec!["4".to_string(), "6".to_string()]);
    }

    #[test]
    fn forced_indexed_import_is_read_only_but_starts_unsaved() {
        // `forceIndexed` spills to the read-only indexed backing. The result is
        // still a brand-new document with no source CSV, so it must start dirty
        // (closing warns, Save routes to Save As) rather than looking clean and
        // silently dropping the just-imported data on close.
        let (_dir, path) = book(|wb| {
            let ws = wb.add_worksheet();
            ws.set_name("Sheet1")?;
            ws.write_string(0, 0, "id")?;
            ws.write_string(0, 1, "name")?;
            ws.write_number(1, 0, 1)?;
            ws.write_string(1, 1, "Ada")?;
            Ok(())
        });
        let opts = ExcelImportOptions {
            force_indexed: true,
            ..sheet_opts("Sheet1")
        };
        let (_cache, doc) = import_grid(&path, &opts);
        assert!(
            !doc.is_editable(),
            "forceIndexed opens the read-only backing"
        );
        assert!(doc.is_dirty(), "a fresh indexed import starts unsaved");
        assert!(doc.meta().path.is_none(), "no source path yet");
        assert_eq!(doc.n_rows(), 1);
    }

    // ----- inspection ---------------------------------------------------------

    #[test]
    fn inspect_reports_visibility_formulas_and_merges() {
        let (_dir, path) = book(|wb| {
            let ws1 = wb.add_worksheet();
            ws1.set_name("Data")?;
            ws1.write_string(0, 0, "a")?;
            ws1.write_number(1, 0, 1)?;
            ws1.write_formula(1, 1, Formula::new("A2+1").set_result("2"))?;
            ws1.merge_range(2, 0, 2, 1, "m", &XFormat::new())?;
            let ws2 = wb.add_worksheet();
            ws2.set_name("Secret")?;
            ws2.set_hidden(true);
            ws2.write_string(0, 0, "x")?;
            Ok(())
        });
        let info = inspect(&path, None).unwrap();
        assert!(!info.has_1904_epoch);
        assert_eq!(info.sheets.len(), 2);
        let data = info.sheets.iter().find(|s| s.name == "Data").unwrap();
        assert_eq!(data.visibility, "visible");
        assert_eq!(data.formula_count, 1);
        assert_eq!(data.merged_count, 1);
        let secret = info.sheets.iter().find(|s| s.name == "Secret").unwrap();
        assert_eq!(secret.visibility, "hidden");
    }

    #[test]
    fn inspect_does_not_mark_an_empty_worksheet_as_having_data() {
        // A blank first sheet must not look importable: calamine returns a
        // `Some(range)` with empty extents for it, so `has_data` has to gate on
        // the used dimensions, letting the chooser fall through to a real sheet.
        let (_dir, path) = book(|wb| {
            let blank = wb.add_worksheet();
            blank.set_name("Blank")?;
            let data = wb.add_worksheet();
            data.set_name("Data")?;
            data.write_string(0, 0, "id")?;
            data.write_number(1, 0, 1)?;
            Ok(())
        });
        let info = inspect(&path, None).unwrap();
        let blank = info.sheets.iter().find(|s| s.name == "Blank").unwrap();
        assert!(!blank.has_data, "an empty worksheet is not importable");
        assert_eq!(blank.used_rows, 0);
        assert_eq!(blank.used_cols, 0);
        let data = info.sheets.iter().find(|s| s.name == "Data").unwrap();
        assert!(data.has_data, "the populated worksheet has data");
    }

    // ----- export limits ------------------------------------------------------

    #[test]
    fn excel_limits_are_the_documented_maxima() {
        assert_eq!(EXCEL_MAX_ROWS, 1_048_576);
        assert_eq!(EXCEL_MAX_COLS, 16_384);
    }

    #[test]
    fn over_column_limit_export_is_refused_before_writing() {
        let doc = Document::new_empty(1, (EXCEL_MAX_COLS + 1) as usize, 1);
        let sources = [SheetSource {
            doc: &doc,
            name: "S".to_string(),
            scope: ExportScope::All,
            expected_revision: doc.revision(),
            grid_widths_px: None,
        }];
        let err = plan_export(&sources).unwrap_err();
        assert!(err.to_string().contains("columns"), "got: {err}");

        // And the full export path refuses too, leaving no file behind.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.xlsx");
        let (_reg, c) = ctx();
        assert!(export(&sources, &dest, &ExcelExportOptions::default(), &c).is_err());
        assert!(!dest.exists(), "refused export writes nothing");
    }

    // ----- multi-sheet export -------------------------------------------------

    #[test]
    fn multi_sheet_export_writes_one_workbook_per_tab() {
        let d1 = doc_from("a,b\n1,2\n", true);
        let d2 = doc_from("x\nhi\n", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("multi.xlsx");
        let sources = [
            SheetSource {
                doc: &d1,
                name: "First".to_string(),
                scope: ExportScope::All,
                expected_revision: d1.revision(),
                grid_widths_px: None,
            },
            SheetSource {
                doc: &d2,
                name: "Second".to_string(),
                scope: ExportScope::All,
                expected_revision: d2.revision(),
                grid_widths_px: None,
            },
        ];
        let (_reg, c) = ctx();
        let bytes = export(&sources, &dest, &ExcelExportOptions::default(), &c).unwrap();
        assert!(bytes > 0);
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), bytes);

        let mut wb: Xlsx<_> = cal_open(&dest).unwrap();
        let names = wb.sheet_names();
        assert!(names.contains(&"First".to_string()) && names.contains(&"Second".to_string()));
        let first = wb.worksheet_range("First").unwrap();
        assert_eq!(first.get_value((0, 0)), Some(&Data::String("a".into())));
        assert_eq!(first.get_value((1, 1)), Some(&Data::String("2".into())));
        let second = wb.worksheet_range("Second").unwrap();
        assert_eq!(second.get_value((1, 0)), Some(&Data::String("hi".into())));
    }

    #[test]
    fn duplicate_sheet_names_are_rejected() {
        let d = doc_from("a\n1\n", true);
        let sources = [
            SheetSource {
                doc: &d,
                name: "Same".to_string(),
                scope: ExportScope::All,
                expected_revision: d.revision(),
                grid_widths_px: None,
            },
            SheetSource {
                doc: &d,
                name: "same".to_string(),
                scope: ExportScope::All,
                expected_revision: d.revision(),
                grid_widths_px: None,
            },
        ];
        assert!(plan_export(&sources)
            .unwrap_err()
            .to_string()
            .contains("unique"));
    }

    // ----- typed export -------------------------------------------------------

    #[test]
    fn typed_export_writes_numbers_booleans_and_dates() {
        let mut d = doc_from("n,flag,when\n42,true,2020-01-15\n", true);
        let ids = d.column_ids().to_vec();
        d.set_column_schema(ColumnSchema::new(ids[0].clone(), "n", LogicalType::Integer));
        d.set_column_schema(ColumnSchema::new(
            ids[1].clone(),
            "flag",
            LogicalType::Boolean,
        ));
        d.set_column_schema(ColumnSchema::new(ids[2].clone(), "when", LogicalType::Date));
        let revision = d.revision();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("typed.xlsx");
        let sources = [SheetSource {
            doc: &d,
            name: "Typed".to_string(),
            scope: ExportScope::All,
            expected_revision: revision,
            grid_widths_px: None,
        }];
        let opts = ExcelExportOptions {
            typed: true,
            ..Default::default()
        };
        let (_reg, c) = ctx();
        export(&sources, &dest, &opts, &c).unwrap();

        let mut wb: Xlsx<_> = cal_open(&dest).unwrap();
        let r = wb.worksheet_range("Typed").unwrap();
        assert_eq!(r.get_value((1, 0)).and_then(|d| d.as_f64()), Some(42.0));
        assert_eq!(r.get_value((1, 1)).and_then(|d| d.get_bool()), Some(true));
        assert_eq!(
            r.get_value((1, 2)).and_then(|d| d.as_date()),
            chrono::NaiveDate::from_ymd_opt(2020, 1, 15)
        );
    }

    #[test]
    fn typed_export_out_of_range_year_falls_back_to_text() {
        // A custom input format can parse a signed year outside Excel's
        // 1900..=9999 window. `67536 as u16 == 2000`, which would pass
        // rust_xlsxwriter's own bounds check and write a bogus year-2000 date;
        // the guard must instead fall back to text and preserve the source.
        let mut d = doc_from("d\n+67536-01-01\n", true);
        let ids = d.column_ids().to_vec();
        let mut sch = ColumnSchema::new(ids[0].clone(), "d", LogicalType::Date);
        sch.input_formats = Some(vec!["%Y-%m-%d".to_string()]);
        // Precondition: the value really is a Valid Date under the format (chrono
        // accepts signed out-of-range years), so the text fallback below is the
        // year-guard's doing, not a mere parse failure.
        assert!(
            matches!(
                schema::classify(Some("+67536-01-01"), &sch),
                CellState::Valid(TypedValue::Date(_))
            ),
            "the out-of-range year must classify as a Valid Date"
        );
        d.set_column_schema(sch);
        let revision = d.revision();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("oor.xlsx");
        let sources = [SheetSource {
            doc: &d,
            name: "S".to_string(),
            scope: ExportScope::All,
            expected_revision: revision,
            grid_widths_px: None,
        }];
        let opts = ExcelExportOptions {
            typed: true,
            ..Default::default()
        };
        let (_reg, c) = ctx();
        export(&sources, &dest, &opts, &c).unwrap();

        let mut wb: Xlsx<_> = cal_open(&dest).unwrap();
        let r = wb.worksheet_range("S").unwrap();
        // Preserved as text — NOT a truncated year-2000 date cell.
        assert_eq!(
            r.get_value((1, 0)),
            Some(&Data::String("+67536-01-01".into()))
        );
    }

    #[test]
    fn untyped_export_keeps_leading_zero_codes_as_text() {
        // Without a schema, a numeric-looking code stays a string cell so its
        // leading zero survives the round-trip.
        let d = doc_from("code\n00501\n", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("codes.xlsx");
        let sources = [SheetSource {
            doc: &d,
            name: "Codes".to_string(),
            scope: ExportScope::All,
            expected_revision: d.revision(),
            grid_widths_px: None,
        }];
        let (_reg, c) = ctx();
        export(&sources, &dest, &ExcelExportOptions::default(), &c).unwrap();
        let mut wb: Xlsx<_> = cal_open(&dest).unwrap();
        let r = wb.worksheet_range("Codes").unwrap();
        assert_eq!(r.get_value((1, 0)), Some(&Data::String("00501".into())));
    }

    // ----- cancellation -------------------------------------------------------

    #[test]
    fn cancelled_export_leaves_no_output() {
        let d = doc_from("a\n1\n2\n", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.xlsx");
        std::fs::write(&dest, b"precious").unwrap();

        let reg = JobRegistry::default();
        let c = reg.begin("export", None, |_| {});
        reg.cancel(c.id);
        let sources = [SheetSource {
            doc: &d,
            name: "S".to_string(),
            scope: ExportScope::All,
            expected_revision: d.revision(),
            grid_widths_px: None,
        }];
        assert!(matches!(
            export(&sources, &dest, &ExcelExportOptions::default(), &c),
            Err(AppError::Cancelled)
        ));
        // The pre-existing destination is untouched (the atomic pipeline never
        // ran) and no staging file is left behind.
        assert_eq!(std::fs::read(&dest).unwrap(), b"precious");
        let stray = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".ceesvee-save-"));
        assert!(!stray);
    }

    // ----- A1 helpers ---------------------------------------------------------

    #[test]
    fn a1_parsing_round_trips() {
        assert_eq!(parse_a1_cell("A1"), Some((0, 0)));
        assert_eq!(parse_a1_cell("$B$2"), Some((1, 1)));
        assert_eq!(parse_a1_cell("AA10"), Some((9, 26)));
        assert_eq!(col_to_letters(0), "A");
        assert_eq!(col_to_letters(26), "AA");
        assert_eq!(
            parse_a1_range("C3:A1").unwrap(),
            ((0, 0), (2, 2)),
            "normalised"
        );
        assert!(parse_a1_range("nonsense").is_err());
    }

    #[test]
    fn defined_name_parsing_extracts_sheet_and_range() {
        assert_eq!(
            parse_defined_name("Sheet1!$A$1:$C$9"),
            Some(("Sheet1".to_string(), ((0, 0), (8, 2))))
        );
        assert_eq!(
            parse_defined_name("'My Sheet'!$B$2"),
            Some(("My Sheet".to_string(), ((1, 1), (1, 1))))
        );
        // A multi-area name is not a single contiguous range.
        assert_eq!(parse_defined_name("Sheet1!$A$1,Sheet1!$C$3"), None);
    }
}
