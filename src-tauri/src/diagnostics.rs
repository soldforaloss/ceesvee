//! Data-fidelity diagnostics: detect structural, encoding and data-quality
//! problems in an open document without mutating it.
//!
//! Two families of issues are reported:
//! * **source** issues describe the file as it was parsed (decode damage,
//!   ragged records). They are captured at import time (see
//!   [`crate::parse::ImportInfo`]) and refreshed only by a reparse.
//! * **current** issues describe the in-memory grid right now (duplicate or
//!   blank headers, empty columns, mixed types, stray whitespace, …) and are
//!   recomputed on every scan.
//!
//! Scans run as cancellable background jobs (see [`crate::job`]) while holding
//! the document's read lock, so a report is always self-consistent and tagged
//! with the revision it was computed against. Reports never mutate the
//! document; the only write path here is [`issue_rows`], which the
//! `apply_diagnostic_filter` command uses to build a (non-undoable) filter
//! view.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::analyze::{classify, CellClass};
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;

/// Cap on per-issue samples: enough to browse without bloating the report
/// when a million rows are affected (the exact count is always reported).
const SAMPLE_LIMIT: usize = 50;
/// A column is "blank-heavy" strictly above this share of empty cells.
const BLANK_HEAVY_THRESHOLD: f64 = 0.5;
/// Cancellation/advance granularity for row loops.
const ROW_CHUNK: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A pointer at (or description of) one affected place.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSample {
    /// Data-row index in the current document, when the issue maps to one.
    pub row: Option<usize>,
    /// Column index, when the issue maps to one.
    pub col: Option<usize>,
    /// Truncated cell/header value for display.
    pub value: Option<String>,
    /// Extra context (e.g. "line 1042 had 3 fields (expected 5)").
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticIssue {
    /// Stable identifier: the kind, plus `:column` when column-scoped.
    pub id: String,
    pub kind: String,
    pub severity: Severity,
    pub title: String,
    pub description: String,
    pub affected_count: usize,
    pub samples: Vec<DiagnosticSample>,
    pub suggested_action: Option<String>,
    /// Whether "filter to affected rows" is meaningful for this issue.
    pub row_filterable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub doc_id: u64,
    /// Document revision this report was computed against. The UI treats the
    /// report as stale (and offers a rescan) once the document moves on.
    pub revision: u64,
    /// Issues describing the imported source file.
    pub source: Vec<DiagnosticIssue>,
    /// Issues describing the current in-memory document.
    pub current: Vec<DiagnosticIssue>,
}

/// Last completed report per document, managed by Tauri. Written by scan jobs
/// off the UI thread, read by `get_diagnostics`, pruned on document close.
#[derive(Default)]
pub struct DiagnosticsCache(Arc<Mutex<HashMap<u64, DiagnosticsReport>>>);

impl DiagnosticsCache {
    /// Clone the shared map handle for use inside a job closure.
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, DiagnosticsReport>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<DiagnosticsReport> {
        self.0.lock().ok()?.get(&doc_id).cloned()
    }

    pub fn remove(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&doc_id);
        }
    }
}

// ----- per-cell presentation --------------------------------------------------

fn class_name(class: CellClass) -> &'static str {
    match class {
        CellClass::Blank => "blank",
        CellClass::Number => "number",
        CellClass::Date => "date",
        CellClass::Bool => "boolean",
        CellClass::Text => "text",
    }
}

fn has_edge_whitespace(cell: &str) -> bool {
    !cell.is_empty() && cell.trim() != cell
}

fn has_replacement_char(cell: &str) -> bool {
    cell.contains('\u{FFFD}')
}

fn preview(value: &str) -> String {
    const MAX: usize = 80;
    if value.chars().count() <= MAX {
        value.to_string()
    } else {
        let head: String = value.chars().take(MAX).collect();
        format!("{head}…")
    }
}

fn column_label(doc: &Document, col: usize) -> String {
    let header = doc.headers().get(col).map(String::as_str).unwrap_or("");
    if header.trim().is_empty() {
        format!("column {}", col + 1)
    } else {
        format!("“{}”", preview(header))
    }
}

// ----- column accumulator ----------------------------------------------------

#[derive(Default, Clone)]
struct ColumnStats {
    blank: usize,
    number: usize,
    date: usize,
    boolean: usize,
    text: usize,
    blank_samples: Vec<usize>,
    whitespace: usize,
    whitespace_samples: Vec<(usize, String)>,
    replacement: usize,
    replacement_samples: Vec<(usize, String)>,
}

impl ColumnStats {
    fn record(&mut self, row: usize, cell: &str) {
        match classify(cell) {
            CellClass::Blank => {
                self.blank += 1;
                if self.blank_samples.len() < SAMPLE_LIMIT {
                    self.blank_samples.push(row);
                }
            }
            CellClass::Number => self.number += 1,
            CellClass::Date => self.date += 1,
            CellClass::Bool => self.boolean += 1,
            CellClass::Text => self.text += 1,
        }
        if has_edge_whitespace(cell) {
            self.whitespace += 1;
            if self.whitespace_samples.len() < SAMPLE_LIMIT {
                self.whitespace_samples.push((row, preview(cell)));
            }
        }
        if has_replacement_char(cell) {
            self.replacement += 1;
            if self.replacement_samples.len() < SAMPLE_LIMIT {
                self.replacement_samples.push((row, preview(cell)));
            }
        }
    }

    fn non_blank(&self) -> usize {
        self.number + self.date + self.boolean + self.text
    }

    /// The dominant class among non-blank cells, when the column reads as a
    /// typed (non-text) column. A text-dominant column can hold anything, so
    /// it is never "mixed".
    fn dominant_class(&self) -> Option<CellClass> {
        if self.non_blank() == 0 {
            return None;
        }
        let mut dominant = (CellClass::Text, self.text);
        for candidate in [
            (CellClass::Number, self.number),
            (CellClass::Date, self.date),
            (CellClass::Bool, self.boolean),
        ] {
            if candidate.1 > dominant.1 {
                dominant = candidate;
            }
        }
        (dominant.0 != CellClass::Text).then_some(dominant.0)
    }

    /// Number of non-blank cells that do not match the dominant class, when
    /// the column is typed and inconsistent.
    fn mixed_minority(&self) -> Option<(CellClass, usize)> {
        let dominant = self.dominant_class()?;
        let dominant_count = match dominant {
            CellClass::Number => self.number,
            CellClass::Date => self.date,
            CellClass::Bool => self.boolean,
            CellClass::Blank | CellClass::Text => return None,
        };
        let minority = self.non_blank() - dominant_count;
        (minority > 0).then_some((dominant, minority))
    }
}

// ----- the scan ---------------------------------------------------------------

/// Compute a full diagnostics report. Read-only; progress and cancellation
/// via `ctx`.
pub fn scan(doc: &Document, ctx: &JobCtx) -> AppResult<DiagnosticsReport> {
    let n_rows = doc.n_rows();
    let n_cols = doc.n_cols();
    ctx.set_total(n_rows as u64);

    let mut stats: Vec<ColumnStats> = vec![ColumnStats::default(); n_cols];
    let mut pending = 0u64;
    doc.visit_rows(0..n_rows, &mut |row_index, row| {
        for (col, cell) in row.iter().enumerate().take(n_cols) {
            stats[col].record(row_index, cell);
        }
        pending += 1;
        if pending >= ROW_CHUNK as u64 {
            ctx.advance(pending)?;
            pending = 0;
        }
        Ok(true)
    })?;
    ctx.advance(pending)?;

    let mut current = Vec::new();
    header_issues(doc, &mut current);
    column_issues(doc, &stats, ctx, &mut current)?;

    Ok(DiagnosticsReport {
        doc_id: doc.id,
        revision: doc.revision(),
        source: source_issues(doc),
        current,
    })
}

fn source_issues(doc: &Document) -> Vec<DiagnosticIssue> {
    let info = doc.import_info();
    let mut issues = Vec::new();

    if info.had_decode_errors {
        issues.push(DiagnosticIssue {
            id: "decodeErrors".into(),
            kind: "decodeErrors".into(),
            severity: Severity::Error,
            title: "Malformed bytes in source file".into(),
            description: format!(
                "Some byte sequences were not valid {} and were replaced with \u{FFFD} while \
                 decoding. The original characters cannot be recovered unless the file is \
                 reopened with the correct encoding.",
                doc.encoding_name()
            ),
            affected_count: 1,
            samples: Vec::new(),
            suggested_action: Some("Reopen the file with a different encoding".into()),
            row_filterable: false,
        });
    }

    if info.ragged_total > 0 {
        let samples = info
            .ragged_samples
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|s| DiagnosticSample {
                row: None,
                col: None,
                value: None,
                note: Some(format!(
                    "line {} had {} field{} (expected {})",
                    s.line,
                    s.fields,
                    if s.fields == 1 { "" } else { "s" },
                    info.modal_field_count,
                )),
            })
            .collect();
        issues.push(DiagnosticIssue {
            id: "raggedRows".into(),
            kind: "raggedRows".into(),
            severity: Severity::Warning,
            title: "Ragged records in source file".into(),
            description: format!(
                "{} source record{} had a field count different from the most common count \
                 ({}). Short records were padded with empty cells to keep the grid \
                 rectangular, so saving will normalise their structure.",
                info.ragged_total,
                if info.ragged_total == 1 { "" } else { "s" },
                info.modal_field_count,
            ),
            affected_count: info.ragged_total,
            samples,
            suggested_action: Some(
                "Check the delimiter setting; a wrong delimiter often shows up as ragged rows"
                    .into(),
            ),
            row_filterable: false,
        });
    }

    issues
}

fn header_issues(doc: &Document, out: &mut Vec<DiagnosticIssue>) {
    if !doc.has_header_row() {
        return;
    }
    let headers = doc.headers();

    // Exact duplicates (blank headers are reported separately).
    let mut exact: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, h) in headers.iter().enumerate() {
        if !h.trim().is_empty() {
            exact.entry(h.as_str()).or_default().push(i);
        }
    }
    let mut dup_samples: Vec<DiagnosticSample> = Vec::new();
    let mut dup_count = 0usize;
    for (name, cols) in &exact {
        if cols.len() > 1 {
            dup_count += cols.len();
            for &c in cols {
                dup_samples.push(DiagnosticSample {
                    row: None,
                    col: Some(c),
                    value: Some(preview(name)),
                    note: None,
                });
            }
        }
    }
    dup_samples.sort_by_key(|s| s.col);
    dup_samples.truncate(SAMPLE_LIMIT);
    if dup_count > 0 {
        out.push(DiagnosticIssue {
            id: "duplicateHeaders".into(),
            kind: "duplicateHeaders".into(),
            severity: Severity::Error,
            title: "Duplicate column names".into(),
            description: "Multiple columns share exactly the same header. Tools that look \
                          columns up by name will silently pick one of them."
                .into(),
            affected_count: dup_count,
            samples: dup_samples,
            suggested_action: Some("Rename the duplicated columns".into()),
            row_filterable: false,
        });
    }

    // Case-insensitive duplicates that are not already exact duplicates.
    let mut folded: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, h) in headers.iter().enumerate() {
        if !h.trim().is_empty() {
            folded.entry(h.to_lowercase()).or_default().push(i);
        }
    }
    let mut ci_samples: Vec<DiagnosticSample> = Vec::new();
    let mut ci_count = 0usize;
    for cols in folded.values() {
        if cols.len() > 1 {
            // Only flag groups with at least two distinct spellings; identical
            // spellings are already covered by the exact-duplicate issue.
            let distinct: std::collections::HashSet<&str> =
                cols.iter().map(|&c| headers[c].as_str()).collect();
            if distinct.len() > 1 {
                ci_count += cols.len();
                for &c in cols {
                    ci_samples.push(DiagnosticSample {
                        row: None,
                        col: Some(c),
                        value: Some(preview(&headers[c])),
                        note: None,
                    });
                }
            }
        }
    }
    ci_samples.sort_by_key(|s| s.col);
    ci_samples.truncate(SAMPLE_LIMIT);
    if ci_count > 0 {
        out.push(DiagnosticIssue {
            id: "duplicateHeadersCaseInsensitive".into(),
            kind: "duplicateHeadersCaseInsensitive".into(),
            severity: Severity::Warning,
            title: "Column names differing only by case".into(),
            description: "Several headers are identical when compared case-insensitively. \
                          Case-insensitive tools (SQL imports, spreadsheets) may conflate \
                          them."
                .into(),
            affected_count: ci_count,
            samples: ci_samples,
            suggested_action: Some("Rename the columns so they differ by more than case".into()),
            row_filterable: false,
        });
    }

    // Blank headers.
    let blank_cols: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    if !blank_cols.is_empty() {
        out.push(DiagnosticIssue {
            id: "blankHeaders".into(),
            kind: "blankHeaders".into(),
            severity: Severity::Warning,
            title: "Blank column names".into(),
            description: format!(
                "{} column{} no header text. Consumers keying by column name cannot \
                 address {}.",
                blank_cols.len(),
                if blank_cols.len() == 1 {
                    " has"
                } else {
                    "s have"
                },
                if blank_cols.len() == 1 { "it" } else { "them" },
            ),
            affected_count: blank_cols.len(),
            samples: blank_cols
                .iter()
                .take(SAMPLE_LIMIT)
                .map(|&c| DiagnosticSample {
                    row: None,
                    col: Some(c),
                    value: None,
                    note: Some(format!("column {}", c + 1)),
                })
                .collect(),
            suggested_action: Some("Give the blank columns names".into()),
            row_filterable: false,
        });
    }
}

fn column_issues(
    doc: &Document,
    stats: &[ColumnStats],
    ctx: &JobCtx,
    out: &mut Vec<DiagnosticIssue>,
) -> AppResult<()> {
    let total_rows = doc.n_rows();

    // Completely empty columns (single issue).
    let empty_cols: Vec<usize> = (0..stats.len())
        .filter(|&c| total_rows > 0 && stats[c].non_blank() == 0)
        .collect();
    if !empty_cols.is_empty() {
        out.push(DiagnosticIssue {
            id: "emptyColumns".into(),
            kind: "emptyColumns".into(),
            severity: Severity::Info,
            title: "Completely empty columns".into(),
            description: format!(
                "{} column{} no data at all.",
                empty_cols.len(),
                if empty_cols.len() == 1 {
                    " contains"
                } else {
                    "s contain"
                },
            ),
            affected_count: empty_cols.len(),
            samples: empty_cols
                .iter()
                .take(SAMPLE_LIMIT)
                .map(|&c| DiagnosticSample {
                    row: None,
                    col: Some(c),
                    value: None,
                    note: Some(column_label(doc, c)),
                })
                .collect(),
            suggested_action: Some("Consider deleting the empty columns".into()),
            row_filterable: false,
        });
    }

    // Replacement characters anywhere in the data (single issue).
    let replacement_total: usize = stats.iter().map(|s| s.replacement).sum();
    if replacement_total > 0 {
        let mut samples = Vec::new();
        'outer: for (c, stat) in stats.iter().enumerate() {
            for (row, value) in &stat.replacement_samples {
                if samples.len() >= SAMPLE_LIMIT {
                    break 'outer;
                }
                samples.push(DiagnosticSample {
                    row: Some(*row),
                    col: Some(c),
                    value: Some(value.clone()),
                    note: None,
                });
            }
        }
        out.push(DiagnosticIssue {
            id: "replacementChars".into(),
            kind: "replacementChars".into(),
            severity: Severity::Error,
            title: "Replacement characters in data".into(),
            description: format!(
                "{} cell{} the Unicode replacement character (\u{FFFD}), which usually means \
                 characters were lost to a wrong encoding at some point.",
                replacement_total,
                if replacement_total == 1 {
                    " contains"
                } else {
                    "s contain"
                },
            ),
            affected_count: replacement_total,
            samples,
            suggested_action: Some(
                "Reopen the file with the correct encoding, or fix the affected cells".into(),
            ),
            row_filterable: true,
        });
    }

    // Per-column issues: mixed types, whitespace, blank-heavy.
    for (c, stat) in stats.iter().enumerate() {
        if let Some((dominant, minority)) = stat.mixed_minority() {
            let samples = minority_samples(doc, c, dominant, ctx)?;
            out.push(DiagnosticIssue {
                id: format!("mixedTypes:{c}"),
                kind: "mixedTypes".into(),
                severity: Severity::Warning,
                title: format!("Mixed types in {}", column_label(doc, c)),
                description: format!(
                    "The column reads as {} but {} non-blank cell{} not. Sorting and numeric \
                     analysis will treat those cells as text.",
                    class_name(dominant),
                    minority,
                    if minority == 1 { " is" } else { "s are" },
                ),
                affected_count: minority,
                samples,
                suggested_action: Some(
                    "Review the outlier cells; a cleanup transform can normalise them".into(),
                ),
                row_filterable: true,
            });
        }

        if stat.whitespace > 0 {
            out.push(DiagnosticIssue {
                id: format!("whitespace:{c}"),
                kind: "whitespace".into(),
                severity: Severity::Warning,
                title: format!("Leading/trailing whitespace in {}", column_label(doc, c)),
                description: format!(
                    "{} cell{} leading or trailing whitespace. Lookups and joins on this \
                     column will silently miss.",
                    stat.whitespace,
                    if stat.whitespace == 1 {
                        " has"
                    } else {
                        "s have"
                    },
                ),
                affected_count: stat.whitespace,
                samples: stat
                    .whitespace_samples
                    .iter()
                    .map(|(row, value)| DiagnosticSample {
                        row: Some(*row),
                        col: Some(c),
                        value: Some(value.clone()),
                        note: None,
                    })
                    .collect(),
                suggested_action: Some("Trim the affected cells".into()),
                row_filterable: true,
            });
        }

        let blank_share = if total_rows == 0 {
            0.0
        } else {
            stat.blank as f64 / total_rows as f64
        };
        if stat.non_blank() > 0 && blank_share > BLANK_HEAVY_THRESHOLD {
            out.push(DiagnosticIssue {
                id: format!("blankHeavy:{c}"),
                kind: "blankHeavy".into(),
                severity: Severity::Info,
                title: format!("Mostly blank column {}", column_label(doc, c)),
                description: format!(
                    "{:.0}% of the cells in this column are blank ({} of {}).",
                    blank_share * 100.0,
                    stat.blank,
                    total_rows,
                ),
                affected_count: stat.blank,
                samples: stat
                    .blank_samples
                    .iter()
                    .map(|&row| DiagnosticSample {
                        row: Some(row),
                        col: Some(c),
                        value: None,
                        note: None,
                    })
                    .collect(),
                suggested_action: None,
                row_filterable: true,
            });
        }
    }

    Ok(())
}

/// Collect sample cells whose class differs from the column's dominant class.
fn minority_samples(
    doc: &Document,
    col: usize,
    dominant: CellClass,
    ctx: &JobCtx,
) -> AppResult<Vec<DiagnosticSample>> {
    let mut samples = Vec::new();
    doc.visit_rows(0..doc.n_rows(), &mut |row_index, row| {
        if row_index.is_multiple_of(ROW_CHUNK) {
            ctx.check()?;
        }
        let cell = &row[col];
        let class = classify(cell);
        if class != CellClass::Blank && class != dominant {
            samples.push(DiagnosticSample {
                row: Some(row_index),
                col: Some(col),
                value: Some(preview(cell)),
                note: Some(format!(
                    "{} in a {} column",
                    class_name(class),
                    class_name(dominant)
                )),
            });
        }
        Ok(samples.len() < SAMPLE_LIMIT)
    })?;
    Ok(samples)
}

// ----- row filtering -----------------------------------------------------------

/// Absolute indices of the rows affected by a row-filterable issue, resolved
/// against the CURRENT document (callers must have already validated the
/// revision the caller captured).
pub fn issue_rows(doc: &Document, issue_id: &str) -> AppResult<Vec<usize>> {
    let (kind, col) = match issue_id.split_once(':') {
        Some((kind, col)) => {
            let col: usize = col
                .parse()
                .map_err(|_| AppError::invalid("malformed diagnostic issue id"))?;
            if col >= doc.n_cols() {
                return Err(AppError::invalid("diagnostic column out of range"));
            }
            (kind, Some(col))
        }
        None => (issue_id, None),
    };

    let matching = |predicate: &dyn Fn(&[String]) -> bool| -> AppResult<Vec<usize>> {
        let mut out = Vec::new();
        doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
            if predicate(row) {
                out.push(i);
            }
            Ok(true)
        })?;
        Ok(out)
    };

    match (kind, col) {
        ("replacementChars", None) => matching(&|row| row.iter().any(|c| has_replacement_char(c))),
        ("whitespace", Some(c)) => matching(&|row| has_edge_whitespace(&row[c])),
        ("blankHeavy", Some(c)) => matching(&|row| classify(&row[c]) == CellClass::Blank),
        ("mixedTypes", Some(c)) => {
            // Recompute the dominant class so the filter matches what a fresh
            // scan would report for the current data.
            let mut stat = ColumnStats::default();
            doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
                stat.record(i, &row[c]);
                Ok(true)
            })?;
            let Some(dominant) = stat.dominant_class() else {
                return Ok(Vec::new());
            };
            matching(&|row| {
                let class = classify(&row[c]);
                class != CellClass::Blank && class != dominant
            })
        }
        _ => Err(AppError::invalid(
            "this diagnostic does not support row filtering",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("diagnostics", Some(1), |_| {});
        (registry, ctx)
    }

    fn find<'r>(issues: &'r [DiagnosticIssue], kind: &str) -> Option<&'r DiagnosticIssue> {
        issues.iter().find(|i| i.kind == kind)
    }

    #[test]
    fn clean_document_reports_no_issues() {
        let d = doc_from("name,age\nAda,36\nBob,40", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();
        assert!(report.source.is_empty(), "{:?}", report.source);
        assert!(report.current.is_empty(), "{:?}", report.current);
        assert_eq!(report.revision, d.revision());
        assert_eq!(report.doc_id, 1);
    }

    #[test]
    fn malformed_bytes_produce_decode_error_issue() {
        let settings = ParseSettings {
            delimiter: Some(b','),
            encoding: Some(encoding_rs::UTF_8),
        };
        let parsed = parse(b"h1,h2\n\xFFbad,2\n", &settings).unwrap();
        let d = Document::from_parsed(1, None, parsed, true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();

        let issue = find(&report.source, "decodeErrors").expect("decode issue");
        assert_eq!(issue.severity, Severity::Error);

        // The replaced characters also show up as a current-data issue with a
        // jumpable sample.
        let repl = find(&report.current, "replacementChars").expect("replacement issue");
        assert_eq!(repl.affected_count, 1);
        assert_eq!(repl.samples[0].row, Some(0));
        assert_eq!(repl.samples[0].col, Some(0));
        assert!(repl.row_filterable);
    }

    #[test]
    fn ragged_rows_report_original_lines_and_field_counts() {
        let d = doc_from("a,b,c\n1,2\n4,5,6\n7\n", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();
        let issue = find(&report.source, "raggedRows").expect("ragged issue");
        assert_eq!(issue.affected_count, 2);
        let notes: Vec<&str> = issue
            .samples
            .iter()
            .map(|s| s.note.as_deref().unwrap())
            .collect();
        assert_eq!(
            notes,
            vec![
                "line 2 had 2 fields (expected 3)",
                "line 4 had 1 field (expected 3)",
            ]
        );
    }

    #[test]
    fn duplicate_blank_and_case_insensitive_headers_are_detected() {
        let d = doc_from("id,id,ID,,name\n1,2,3,4,x", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();

        let exact = find(&report.current, "duplicateHeaders").expect("exact dup issue");
        assert_eq!(exact.affected_count, 2);
        assert_eq!(exact.severity, Severity::Error);
        let cols: Vec<usize> = exact.samples.iter().map(|s| s.col.unwrap()).collect();
        assert_eq!(cols, vec![0, 1]);

        let ci = find(&report.current, "duplicateHeadersCaseInsensitive").expect("ci dup issue");
        assert_eq!(ci.affected_count, 3, "id, id and ID fold together");

        let blank = find(&report.current, "blankHeaders").expect("blank header issue");
        assert_eq!(blank.affected_count, 1);
        assert_eq!(blank.samples[0].col, Some(3));
    }

    #[test]
    fn no_header_issues_without_a_header_row() {
        let d = doc_from("1,1\n2,2", false);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();
        assert!(find(&report.current, "duplicateHeaders").is_none());
        assert!(find(&report.current, "blankHeaders").is_none());
    }

    #[test]
    fn empty_and_blank_heavy_columns_are_detected() {
        let d = doc_from("a,b,c\n1,,x\n2,,\n3,,\n4,,", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();

        let empty = find(&report.current, "emptyColumns").expect("empty issue");
        assert_eq!(empty.samples[0].col, Some(1));

        let heavy = find(&report.current, "blankHeavy").expect("blank-heavy issue");
        assert_eq!(heavy.id, "blankHeavy:2");
        assert_eq!(heavy.affected_count, 3);

        // Filtering to the blank rows of column c.
        let rows = issue_rows(&d, "blankHeavy:2").unwrap();
        assert_eq!(rows, vec![1, 2, 3]);
    }

    #[test]
    fn mixed_types_are_detected_with_minority_samples() {
        let d = doc_from("n\n1\n2\nx\n4\ny", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();

        let issue = find(&report.current, "mixedTypes").expect("mixed issue");
        assert_eq!(issue.id, "mixedTypes:0");
        assert_eq!(issue.affected_count, 2);
        let sample_rows: Vec<usize> = issue.samples.iter().map(|s| s.row.unwrap()).collect();
        assert_eq!(sample_rows, vec![2, 4]);
        assert_eq!(
            issue.samples[0].note.as_deref(),
            Some("text in a number column")
        );

        let rows = issue_rows(&d, "mixedTypes:0").unwrap();
        assert_eq!(rows, vec![2, 4]);
    }

    #[test]
    fn text_columns_are_not_mixed() {
        let d = doc_from("name\nAda\nBob\n42", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();
        // Dominant class is text (2 of 3), so the stray number is not flagged.
        assert!(find(&report.current, "mixedTypes").is_none());
    }

    #[test]
    fn whitespace_cells_are_detected_and_filterable() {
        let d = doc_from("a,b\n x,1\ny ,2\nz,3", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();

        let issue = find(&report.current, "whitespace").expect("whitespace issue");
        assert_eq!(issue.id, "whitespace:0");
        assert_eq!(issue.affected_count, 2);

        let rows = issue_rows(&d, "whitespace:0").unwrap();
        assert_eq!(rows, vec![0, 1]);
    }

    #[test]
    fn scan_is_cancellable() {
        let d = doc_from("a\n1\n2\n3", true);
        let registry = JobRegistry::default();
        let ctx = registry.begin("diagnostics", Some(1), |_| {});
        registry.cancel(ctx.id);
        assert!(matches!(scan(&d, &ctx), Err(AppError::Cancelled)));
    }

    #[test]
    fn issue_rows_rejects_unknown_and_malformed_ids() {
        let d = doc_from("a\n1", true);
        assert!(issue_rows(&d, "duplicateHeaders").is_err());
        assert!(issue_rows(&d, "mixedTypes:notanumber").is_err());
        assert!(issue_rows(&d, "whitespace:99").is_err());
    }

    #[test]
    fn cache_stores_and_prunes_reports() {
        let cache = DiagnosticsCache::default();
        let d = doc_from("a\n1", true);
        let (_r, ctx) = ctx();
        let report = scan(&d, &ctx).unwrap();
        cache.share().lock().unwrap().insert(1, report);
        assert!(cache.get(1).is_some());
        assert!(cache.get(2).is_none());
        cache.remove(1);
        assert!(cache.get(1).is_none());
    }
}
