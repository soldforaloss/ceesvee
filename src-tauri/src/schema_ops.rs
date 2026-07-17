//! F31 document-level schema operations: the glue between the pure schema
//! core ([`crate::schema`]) and a live [`Document`].
//!
//! * five-state column scans (invalid-value samples, conversion previews),
//!   threading the RAGGED-row information from the import diagnostics so a
//!   field that never existed in the source classifies as `Missing`, not
//!   `Empty`;
//! * canonical conversion — the ONLY path that ever rewrites cell text from
//!   a schema, applied as ONE undoable `set_cells` batch;
//! * strict/advisory edit validation applied BEFORE an edit reaches the
//!   document model (strict rejects; advisory applies and records a bounded
//!   issue list on the document).
//!
//! Ragged mapping caveat: import diagnostics describe the file AS READ.
//! Row indices are exact on an unedited document; after row inserts,
//! deletes or destructive sorts the mapping is best-effort (rows may have
//! moved), and records past the diagnostics' sample cap count as not
//! missing.

use std::collections::HashMap;

use serde::Serialize;

use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::schema::{self, CellState, ColumnSchema, DocumentSchema, SchemaIssue, ValidationMode};

/// Rows scanned on an INDEXED document (editable documents scan everything).
/// Mirrors the inference sample in [`crate::schema`].
const INDEXED_SCAN_ROWS: usize = 100_000;

/// Hard cap on returned samples, whatever the caller asks for.
const MAX_SAMPLES: usize = 500;

/// Longest cell text echoed in a sample.
const SAMPLE_VALUE_CAP: usize = 200;

/// Progress/cancellation granularity.
const ROW_CHUNK: usize = 4096;

fn truncate_value(value: &str) -> String {
    let truncated = value.chars().nth(SAMPLE_VALUE_CAP).is_some();
    let mut out: String = value.chars().take(SAMPLE_VALUE_CAP).collect();
    if truncated {
        out.push('…');
    }
    out
}

/// Resolve a stable column ID to its CURRENT position.
pub fn column_index(doc: &Document, column_id: &str) -> AppResult<usize> {
    doc.column_ids()
        .iter()
        .position(|id| id == column_id)
        .ok_or_else(|| AppError::invalid(format!("no column with id \"{column_id}\"")))
}

/// The declared schema for a column ID, or a clear error.
fn declared_schema<'d>(doc: &'d Document, column_id: &str) -> AppResult<&'d ColumnSchema> {
    doc.schema().column(column_id).ok_or_else(|| {
        AppError::invalid(format!(
            "column \"{column_id}\" has no declared schema — assign a logical type first"
        ))
    })
}

/// Data-row → ORIGINAL field count, for rows the import diagnostics recorded
/// as ragged (see the module docs for the exactness caveat). Record 0 is the
/// header when one is present.
fn ragged_fields_by_row(doc: &Document) -> HashMap<usize, usize> {
    let header_offset = usize::from(doc.has_header_row());
    doc.import_info()
        .ragged_samples
        .iter()
        .filter_map(|s| {
            s.record
                .checked_sub(header_offset)
                .map(|row| (row, s.fields))
        })
        .collect()
}

/// How many rows a read-only schema scan covers (all of an editable
/// document, a bounded leading sample of an indexed one).
fn scan_len(doc: &Document) -> usize {
    if doc.is_editable() {
        doc.n_rows()
    } else {
        doc.n_rows().min(INDEXED_SCAN_ROWS)
    }
}

// ---------------------------------------------------------------------------
// Schema info / import (names refreshed from headers by stable ID)
// ---------------------------------------------------------------------------

/// The schema surface returned to the front end. `schemaRevision` tracks
/// schema-only changes; `revision` is the ordinary document revision (which
/// schema edits deliberately do NOT move).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaInfo {
    pub schema: DocumentSchema,
    pub schema_revision: u64,
    pub revision: u64,
}

/// Snapshot the document's schema with every entry's `name` refreshed from
/// the CURRENT header of its column (the header is the source of truth; the
/// stored name goes stale by design after renames).
pub fn schema_info(doc: &Document) -> SchemaInfo {
    let mut schema = doc.schema().clone();
    for (id, entry) in schema.columns.iter_mut() {
        if let Some(pos) = doc.column_ids().iter().position(|c| c == id) {
            entry.name = doc.headers()[pos].clone();
        }
    }
    SchemaInfo {
        schema,
        schema_revision: doc.schema_revision(),
        revision: doc.revision(),
    }
}

/// Result of importing a schema file into a document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaImportOutcome {
    /// Column entries applied (their IDs exist in this document).
    pub applied: usize,
    /// Entry IDs skipped because no current column carries them.
    pub skipped_unknown: Vec<String>,
    pub info: SchemaInfo,
}

/// Replace the document's schema with an imported one. Entries whose column
/// ID does not exist in this document are dropped (and reported) instead of
/// silently lingering; surviving entries are validated like manual edits.
pub fn import_into(doc: &mut Document, imported: DocumentSchema) -> AppResult<SchemaImportOutcome> {
    let mut kept = DocumentSchema::default();
    let mut skipped_unknown = Vec::new();
    for (id, entry) in imported.columns {
        if doc.column_ids().iter().any(|c| c == &id) {
            schema::validate_column_schema(&entry)?;
            kept.set_column(entry);
        } else {
            skipped_unknown.push(id);
        }
    }
    let applied = kept.columns.len();
    doc.set_document_schema(kept);
    Ok(SchemaImportOutcome {
        applied,
        skipped_unknown,
        info: schema_info(doc),
    })
}

// ---------------------------------------------------------------------------
// Five-state scans: invalid samples + conversion preview
// ---------------------------------------------------------------------------

/// Counts of the five distinguishable cell states over the scanned rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnStateCounts {
    pub valid: usize,
    pub invalid: usize,
    pub empty: usize,
    pub null_token: usize,
    pub missing: usize,
}

/// One invalid cell under the declared type.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidSample {
    /// Absolute (unfiltered) row index.
    pub row: usize,
    pub value: String,
    pub reason: String,
}

/// Bounded report of a column's invalid values under its declared type.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidSampleReport {
    pub column_id: String,
    pub counts: ColumnStateCounts,
    pub samples: Vec<InvalidSample>,
    /// Rows the scan covered (a leading sample on indexed documents).
    pub scanned_rows: usize,
    pub total_rows: usize,
    pub revision: u64,
}

struct ColumnScan {
    counts: ColumnStateCounts,
    invalid: Vec<InvalidSample>,
    changes: Vec<(usize, usize, String)>,
    convert_samples: Vec<ConvertSample>,
    scanned: usize,
}

/// One streaming pass over a column: five-state counts, bounded invalid
/// samples, and (when `collect_changes`) the canonical-conversion change
/// list plus before/after samples.
fn scan_column(
    doc: &Document,
    column_id: &str,
    max_samples: usize,
    collect_changes: bool,
    ctx: Option<&JobCtx>,
) -> AppResult<ColumnScan> {
    let schema = declared_schema(doc, column_id)?.clone();
    let col = column_index(doc, column_id)?;
    let ragged = ragged_fields_by_row(doc);
    let max_samples = max_samples.min(MAX_SAMPLES);
    let scan_end = scan_len(doc);
    if let Some(ctx) = ctx {
        ctx.set_total(scan_end as u64);
    }

    let mut out = ColumnScan {
        counts: ColumnStateCounts::default(),
        invalid: Vec::new(),
        changes: Vec::new(),
        convert_samples: Vec::new(),
        scanned: scan_end,
    };
    let mut pending = 0u64;
    doc.visit_rows(0..scan_end, &mut |r, row| {
        pending += 1;
        if pending >= ROW_CHUNK as u64 {
            if let Some(ctx) = ctx {
                ctx.advance(pending)?;
            }
            pending = 0;
        }
        let cell = row.get(col).map(String::as_str).unwrap_or("");
        // A field the source record never had classifies as Missing: the
        // grid is padded rectangular, so the diagnostics decide.
        let missing = ragged.get(&r).is_some_and(|&fields| col >= fields);
        let raw = if missing { None } else { Some(cell) };
        match schema::classify(raw, &schema) {
            CellState::Missing => out.counts.missing += 1,
            CellState::NullToken => out.counts.null_token += 1,
            CellState::Empty => out.counts.empty += 1,
            CellState::Valid(_) => {
                out.counts.valid += 1;
                if collect_changes {
                    if let Some(canonical) = schema::canonical_text(&schema, cell) {
                        if canonical != cell {
                            if out.convert_samples.len() < max_samples {
                                out.convert_samples.push(ConvertSample {
                                    row: r,
                                    before: truncate_value(cell),
                                    after: truncate_value(&canonical),
                                });
                            }
                            out.changes.push((r, col, canonical));
                        }
                    }
                }
            }
            CellState::Invalid(reason) => {
                out.counts.invalid += 1;
                if out.invalid.len() < max_samples {
                    out.invalid.push(InvalidSample {
                        row: r,
                        value: truncate_value(cell),
                        reason,
                    });
                }
            }
        }
        Ok(true)
    })?;
    if let Some(ctx) = ctx {
        ctx.advance(pending)?;
        ctx.flush_progress();
    }
    Ok(out)
}

/// Bounded sample of a column's invalid values under its declared type.
pub fn invalid_samples(
    doc: &Document,
    column_id: &str,
    max_samples: usize,
) -> AppResult<InvalidSampleReport> {
    let scan = scan_column(doc, column_id, max_samples, false, None)?;
    Ok(InvalidSampleReport {
        column_id: column_id.to_string(),
        counts: scan.counts,
        samples: scan.invalid,
        scanned_rows: scan.scanned,
        total_rows: doc.n_rows(),
        revision: doc.revision(),
    })
}

/// One before/after pair of a canonical conversion.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertSample {
    pub row: usize,
    pub before: String,
    pub after: String,
}

/// Preview of an explicit canonical conversion. Computed WITHOUT mutating;
/// apply is separate and guarded by the echoed revision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertPreview {
    pub column_id: String,
    pub counts: ColumnStateCounts,
    /// Valid cells whose text would actually change.
    pub changed: usize,
    pub samples: Vec<ConvertSample>,
    pub invalid_samples: Vec<InvalidSample>,
    pub scanned_rows: usize,
    /// The revision to hand back to `convert_column_apply`.
    pub revision: u64,
}

/// Compute a conversion preview: per-state counts, how many cells would
/// change, bounded before/after samples, and the invalid cells that will be
/// left untouched.
pub fn convert_preview(
    doc: &Document,
    column_id: &str,
    max_samples: usize,
    ctx: Option<&JobCtx>,
) -> AppResult<ConvertPreview> {
    doc.ensure_editable()?;
    let scan = scan_column(doc, column_id, max_samples, true, ctx)?;
    Ok(ConvertPreview {
        column_id: column_id.to_string(),
        counts: scan.counts,
        changed: scan.changes.len(),
        samples: scan.convert_samples,
        invalid_samples: scan.invalid,
        scanned_rows: scan.scanned,
        revision: doc.revision(),
    })
}

/// Outcome of an applied canonical conversion.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertOutcome {
    pub column_id: String,
    /// Cells rewritten to canonical text (ONE undoable operation).
    pub changed: usize,
    /// Invalid cells left untouched (original text kept), for the report.
    pub invalid: usize,
    /// Null-ish cells left untouched (empty + null tokens + missing).
    pub null_like: usize,
    /// Document revision AFTER the conversion.
    pub revision: u64,
}

/// Apply the canonical conversion of one column as ONE undoable operation.
/// Invalid cells keep their original text (their count is reported);
/// empty/null-token/missing cells are never touched. Guarded by
/// `expected_revision` so it can only apply against the data the preview saw.
pub fn apply_conversion(
    doc: &mut Document,
    column_id: &str,
    expected_revision: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<ConvertOutcome> {
    doc.ensure_editable()?;
    doc.check_revision(expected_revision)?;
    let scan = scan_column(doc, column_id, 0, true, ctx)?;
    if let Some(ctx) = ctx {
        ctx.check()?; // last cancellation point before the commit
    }
    let changed = scan.changes.len();
    // A single set_cells batch = a single undo step.
    doc.set_cells(scan.changes)?;
    Ok(ConvertOutcome {
        column_id: column_id.to_string(),
        changed,
        invalid: scan.counts.invalid,
        null_like: scan.counts.empty + scan.counts.null_token + scan.counts.missing,
        revision: doc.revision(),
    })
}

// ---------------------------------------------------------------------------
// Strict / advisory edit validation (BEFORE the model)
// ---------------------------------------------------------------------------

/// The verdict on one proposed edit, for the front end's pre-checks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellEditValidation {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The declared column's validation mode; `None` = no schema (anything
    /// goes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ValidationMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_id: Option<String>,
}

/// Validate one proposed value against the declared schema of the column at
/// `col`. Pure check — records nothing (the apply path is the recorder, so
/// an issue is stored exactly once, when an edit actually lands).
pub fn check_edit(doc: &Document, col: usize, value: &str) -> CellEditValidation {
    match doc.column_schema_at(col) {
        None => CellEditValidation {
            valid: true,
            reason: None,
            mode: None,
            column_id: None,
        },
        Some(schema) => {
            let result = schema::validate_value(schema, value);
            CellEditValidation {
                valid: result.is_ok(),
                reason: result.err(),
                mode: Some(schema.validation_mode),
                column_id: Some(schema.column_id.clone()),
            }
        }
    }
}

/// Apply a batch of cell edits WITH schema validation:
/// * a violation on a STRICT column rejects the whole batch before any cell
///   of the model changes;
/// * violations on ADVISORY columns apply normally and are recorded as
///   issues on the document (bounded), retrievable by the front end.
///
/// `changes` carries ABSOLUTE row indices (the command layer translates
/// display coordinates first). Returns how many advisory issues were
/// recorded by this batch.
pub fn apply_validated_cells(
    doc: &mut Document,
    changes: Vec<(usize, usize, String)>,
) -> AppResult<usize> {
    let mut advisory: Vec<(usize, usize, String, String)> = Vec::new();
    for (row, col, value) in &changes {
        let Some(schema) = doc.column_schema_at(*col) else {
            continue;
        };
        if let Err(reason) = schema::validate_value(schema, value) {
            match schema.validation_mode {
                ValidationMode::Strict => {
                    return Err(AppError::invalid(format!(
                        "invalid value for column \"{}\": {reason}",
                        schema.name
                    )));
                }
                ValidationMode::Advisory => {
                    advisory.push((*row, *col, schema.column_id.clone(), reason));
                }
            }
        }
    }
    doc.set_cells(changes.clone())?;
    let revision = doc.revision();
    let recorded = advisory.len();
    // Values re-read from the change list so the issue echoes what was set.
    let by_coord: HashMap<(usize, usize), &str> = changes
        .iter()
        .map(|(r, c, v)| ((*r, *c), v.as_str()))
        .collect();
    doc.record_schema_issues(advisory.into_iter().map(|(row, col, column_id, reason)| {
        let value = by_coord.get(&(row, col)).copied().unwrap_or("");
        SchemaIssue::new(row, col, column_id, value, reason, revision)
    }));
    Ok(recorded)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::SortKey;
    use crate::parse::{parse, ParseSettings};
    use crate::schema::{ColumnSchema, LogicalType};

    fn doc_from_csv(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn declare(doc: &mut Document, col: usize, lt: LogicalType) -> String {
        let id = doc.column_ids()[col].clone();
        let name = doc.headers()[col].clone();
        doc.set_column_schema(ColumnSchema::new(id.clone(), name, lt));
        id
    }

    fn declare_with(
        doc: &mut Document,
        col: usize,
        lt: LogicalType,
        f: impl FnOnce(&mut ColumnSchema),
    ) -> String {
        let id = doc.column_ids()[col].clone();
        let name = doc.headers()[col].clone();
        let mut schema = ColumnSchema::new(id.clone(), name, lt);
        f(&mut schema);
        doc.set_column_schema(schema);
        id
    }

    // ----- acceptance: ZIP column declared text preserves leading zeroes ---

    #[test]
    fn zip_declared_text_preserves_leading_zeroes_through_conversion() {
        let mut doc = doc_from_csv("zip,city\n00501,Holtsville\n10001,NYC\n");
        let id = declare(&mut doc, 0, LogicalType::Text);
        // Inference itself refuses numeric for the leading-zero column.
        let inferred = schema::infer_schema(&doc).unwrap();
        assert_eq!(
            inferred.column(&id).unwrap().logical_type,
            LogicalType::Text
        );
        let preview = convert_preview(&doc, &id, 10, None).unwrap();
        // Text canonicalisation is the identity: nothing to change.
        assert_eq!(preview.changed, 0);
        assert_eq!(preview.counts.valid, 2);
        let revision = doc.revision();
        let outcome = apply_conversion(&mut doc, &id, revision, None).unwrap();
        assert_eq!(outcome.changed, 0);
        assert_eq!(doc.rows()[0][0], "00501");
        assert!(!doc.is_dirty(), "a no-op conversion records no undo step");
    }

    // ----- acceptance: conversion previewed + applied as ONE undo ----------

    #[test]
    fn conversion_applies_as_one_undo_operation() {
        let mut doc = doc_from_csv("n,keep\n\"1,234\",a\n007,b\nx,c\n,d\n");
        let id = declare(&mut doc, 0, LogicalType::Integer);

        let preview = convert_preview(&doc, &id, 10, None).unwrap();
        assert_eq!(preview.changed, 2); // "1,234" -> 1234, "007" -> 7
        assert_eq!(preview.counts.invalid, 1); // "x"
        assert_eq!(preview.counts.empty, 1);
        assert_eq!(preview.samples.len(), 2);

        let outcome = apply_conversion(&mut doc, &id, preview.revision, None).unwrap();
        assert_eq!(outcome.changed, 2);
        assert_eq!(outcome.invalid, 1);
        assert_eq!(doc.rows()[0][0], "1234");
        assert_eq!(doc.rows()[1][0], "7");
        assert_eq!(doc.rows()[2][0], "x", "invalid cells keep their text");
        assert_eq!(doc.rows()[3][0], "", "empty cells stay empty");

        // ONE undo restores every converted cell.
        doc.undo().unwrap();
        assert_eq!(doc.rows()[0][0], "1,234");
        assert_eq!(doc.rows()[1][0], "007");
        assert!(!doc.can_undo(), "the conversion was a single undo step");
    }

    #[test]
    fn conversion_apply_is_revision_guarded() {
        let mut doc = doc_from_csv("n\n01\n");
        let id = declare(&mut doc, 0, LogicalType::Integer);
        let preview = convert_preview(&doc, &id, 10, None).unwrap();
        // A concurrent edit lands after the preview…
        doc.set_cell(0, 0, "02".to_string()).unwrap();
        // …so the stale preview can no longer be applied.
        let err = apply_conversion(&mut doc, &id, preview.revision, None).unwrap_err();
        assert!(matches!(err, AppError::StaleRevision { .. }));
    }

    // ----- acceptance: strict rejects BEFORE the model ----------------------

    #[test]
    fn strict_mode_rejects_invalid_edit_before_model() {
        let mut doc = doc_from_csv("n\n1\n2\n");
        declare_with(&mut doc, 0, LogicalType::Integer, |s| {
            s.validation_mode = ValidationMode::Strict;
        });
        let err = apply_validated_cells(&mut doc, vec![(0, 0, "abc".to_string())]).unwrap_err();
        assert!(err.to_string().contains("invalid value for column"));
        assert_eq!(doc.rows()[0][0], "1", "the model was never touched");
        assert!(!doc.is_dirty());
        assert!(!doc.can_undo());
        assert!(doc.schema_issues().is_empty(), "strict records no issue");
    }

    #[test]
    fn strict_rejection_covers_whole_batch() {
        let mut doc = doc_from_csv("a,n\nx,1\n");
        declare_with(&mut doc, 1, LogicalType::Integer, |s| {
            s.validation_mode = ValidationMode::Strict;
        });
        // One valid edit + one strict violation: NOTHING applies.
        let err = apply_validated_cells(
            &mut doc,
            vec![(0, 0, "y".to_string()), (0, 1, "abc".to_string())],
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid value"));
        assert_eq!(doc.rows()[0][0], "x");
        assert_eq!(doc.rows()[0][1], "1");
    }

    // ----- acceptance: advisory applies + records an issue ------------------

    #[test]
    fn advisory_mode_accepts_edit_and_records_issue() {
        let mut doc = doc_from_csv("n\n1\n");
        let id = declare(&mut doc, 0, LogicalType::Integer); // advisory default
        let recorded = apply_validated_cells(&mut doc, vec![(0, 0, "abc".to_string())]).unwrap();
        assert_eq!(recorded, 1);
        assert_eq!(doc.rows()[0][0], "abc", "the edit was applied");
        assert!(doc.is_dirty());
        let issues = doc.schema_issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].row, 0);
        assert_eq!(issues[0].col, 0);
        assert_eq!(issues[0].column_id, id);
        assert_eq!(issues[0].value, "abc");
        assert_eq!(issues[0].revision, doc.revision());
        // A valid edit records nothing further.
        let recorded = apply_validated_cells(&mut doc, vec![(0, 0, "5".to_string())]).unwrap();
        assert_eq!(recorded, 0);
        assert_eq!(doc.schema_issues().len(), 1);
    }

    #[test]
    fn check_edit_is_pure_and_reports_mode() {
        let mut doc = doc_from_csv("n\n1\n");
        declare_with(&mut doc, 0, LogicalType::Integer, |s| {
            s.validation_mode = ValidationMode::Strict;
        });
        let verdict = check_edit(&doc, 0, "abc");
        assert!(!verdict.valid);
        assert_eq!(verdict.mode, Some(ValidationMode::Strict));
        assert!(verdict.reason.is_some());
        assert!(doc.schema_issues().is_empty(), "checking records nothing");
        // No schema on an unknown column index: anything goes.
        let verdict = check_edit(&doc, 5, "abc");
        assert!(verdict.valid);
        assert_eq!(verdict.mode, None);
    }

    // ----- acceptance: empty vs null token stay distinguishable -------------

    #[test]
    fn empty_and_null_token_counted_separately() {
        // Second column keeps the empty-cell row from being an empty line
        // (which the parser would skip entirely).
        let mut doc = doc_from_csv("n,k\n1,a\nNULL,b\n,c\n2,d\n");
        let id = declare_with(&mut doc, 0, LogicalType::Integer, |s| {
            s.null_tokens = vec!["NULL".to_string()];
        });
        let report = invalid_samples(&doc, &id, 10).unwrap();
        assert_eq!(report.counts.valid, 2);
        assert_eq!(report.counts.null_token, 1);
        assert_eq!(report.counts.empty, 1);
        assert_eq!(report.counts.invalid, 0);
    }

    // ----- ragged rows classify as Missing ---------------------------------

    #[test]
    fn ragged_short_row_counts_missing_not_empty() {
        // Row 1 ("only" record) has ONE field; the grid pads column b.
        let mut doc = doc_from_csv("a,b\n1,2\nonly\n3,\n");
        let id = declare(&mut doc, 1, LogicalType::Integer);
        let report = invalid_samples(&doc, &id, 10).unwrap();
        assert_eq!(report.counts.missing, 1, "padded field was never present");
        assert_eq!(report.counts.empty, 1, "a real empty field stays Empty");
        assert_eq!(report.counts.valid, 1);
    }

    // ----- invalid samples --------------------------------------------------

    #[test]
    fn invalid_samples_are_bounded_and_reasoned() {
        let mut doc = doc_from_csv("n\nx1\nx2\nx3\n5\n");
        let id = declare(&mut doc, 0, LogicalType::Integer);
        let report = invalid_samples(&doc, &id, 2).unwrap();
        assert_eq!(report.counts.invalid, 3, "counts stay exact");
        assert_eq!(report.samples.len(), 2, "samples honour the cap");
        assert_eq!(report.samples[0].row, 0);
        assert!(!report.samples[0].reason.is_empty());
        assert_eq!(report.total_rows, 4);
    }

    #[test]
    fn scans_need_a_declared_schema_and_known_column() {
        let doc = doc_from_csv("a\n1\n");
        let id = doc.column_ids()[0].clone();
        assert!(invalid_samples(&doc, &id, 5).is_err(), "no schema declared");
        assert!(invalid_samples(&doc, "c99", 5).is_err(), "unknown column");
    }

    // ----- acceptance: schema survives rename + reorder via stable IDs ------

    #[test]
    fn schema_survives_rename_and_reorder() {
        let mut doc = doc_from_csv("a,b\n1,x\n2,y\n");
        let id = declare(&mut doc, 0, LogicalType::Integer);
        doc.rename_column(0, "amount".to_string()).unwrap();
        doc.move_column(0, 1).unwrap();
        // The entry still resolves through the ID at its NEW position.
        assert_eq!(column_index(&doc, &id).unwrap(), 1);
        let schema = doc.column_schema_at(1).unwrap();
        assert_eq!(schema.logical_type, LogicalType::Integer);
        // schema_info refreshes the display name from the current header.
        let info = schema_info(&doc);
        assert_eq!(info.schema.column(&id).unwrap().name, "amount");
        // And the typed sort still applies to the moved column.
        doc.sort(&[SortKey {
            column: 1,
            descending: true,
        }])
        .unwrap();
        assert_eq!(doc.rows()[0][1], "2");
    }

    // ----- acceptance: schema edits never dirty the document ----------------

    #[test]
    fn schema_and_display_format_changes_do_not_dirty() {
        let mut doc = doc_from_csv("n\n1500\n");
        let id = declare(&mut doc, 0, LogicalType::Integer);
        assert!(!doc.is_dirty());
        let before = doc.revision();
        let rev0 = doc.schema_revision();
        // A displayFormat change is a schema edit: schemaRevision moves,
        // the document revision and dirty flag do not.
        let mut updated = doc.schema().column(&id).unwrap().clone();
        updated.display_format = Some("thousands".to_string());
        doc.set_column_schema(updated);
        assert!(!doc.is_dirty());
        assert!(!doc.can_undo());
        assert_eq!(doc.revision(), before);
        assert!(doc.schema_revision() > rev0);
        // Display formatting changes presentation, never storage.
        let schema = doc.column_schema_at(0).unwrap();
        assert_eq!(schema::format_value(schema, "1500"), "1,500");
        assert_eq!(doc.rows()[0][0], "1500");
    }

    // ----- import / export outcome -----------------------------------------

    #[test]
    fn import_drops_unknown_columns_and_reports_them() {
        let mut doc = doc_from_csv("a,b\n1,2\n");
        let id_a = doc.column_ids()[0].clone();
        let mut imported = DocumentSchema::default();
        imported.set_column(ColumnSchema::new(id_a.clone(), "a", LogicalType::Integer));
        imported.set_column(ColumnSchema::new("c999", "ghost", LogicalType::Uuid));
        let outcome = import_into(&mut doc, imported).unwrap();
        assert_eq!(outcome.applied, 1);
        assert_eq!(outcome.skipped_unknown, vec!["c999".to_string()]);
        assert!(doc.schema().column(&id_a).is_some());
        assert!(doc.schema().column("c999").is_none());
        assert!(!doc.is_dirty(), "importing a schema never dirties the data");
    }

    #[test]
    fn import_validates_entries() {
        let mut doc = doc_from_csv("a\n1\n");
        let id = doc.column_ids()[0].clone();
        let mut bad = ColumnSchema::new(id, "a", LogicalType::Datetime);
        bad.time_zone = Some("Mars/Olympus_Mons".to_string());
        let mut imported = DocumentSchema::default();
        imported.set_column(bad);
        assert!(import_into(&mut doc, imported).is_err());
    }

    // ----- issue list is bounded -------------------------------------------

    #[test]
    fn advisory_issue_list_is_bounded() {
        let mut doc = doc_from_csv("n\n1\n");
        declare(&mut doc, 0, LogicalType::Integer);
        for i in 0..(schema::MAX_SCHEMA_ISSUES + 10) {
            apply_validated_cells(&mut doc, vec![(0, 0, format!("bad{i}"))]).unwrap();
        }
        assert_eq!(doc.schema_issues().len(), schema::MAX_SCHEMA_ISSUES);
        // Newest kept: the very last bad value is present.
        let last = doc.schema_issues().last().unwrap();
        assert_eq!(last.value, format!("bad{}", schema::MAX_SCHEMA_ISSUES + 9));
        doc.clear_schema_issues();
        assert!(doc.schema_issues().is_empty());
    }
}
