//! Record-form view (F41): the backend join and pre-checks behind the
//! single-record editing form for very wide / record-oriented tables.
//!
//! The form reads ONE visible row at a time and shows, per field: the stored
//! raw text, the F31 display-formatted rendering, the five-way classification
//! of the stored value, its current validity under the declared type, and the
//! schema / dictionary / semantic metadata joined together. Editing collects a
//! draft of changed fields; the front end pre-checks it with
//! [`validate_draft`] (mirroring exactly what a real save would do), then
//! commits every changed field of the row as ONE `set_cells` batch — a single
//! undo step (verified in the tests here and in `document.rs`).
//!
//! This module is pure and bounded: it never mutates, and every entry point
//! touches exactly one row. The command layer ([`crate::commands`]) supplies
//! the row cells (read through the backing-aware path so an indexed document
//! streams a single row from disk) and the cached semantic report.

use serde::{Deserialize, Serialize};

use crate::dictionary::DictionaryField;
use crate::document::Document;
use crate::schema::{self, CellState, LogicalType, SchemaIssue, ValidationMode};
use crate::schema_ops;
use crate::semantic::{SemanticReport, SemanticType};

// ---------------------------------------------------------------------------
// Record fetch (schema + dictionary + semantic join for one row)
// ---------------------------------------------------------------------------

/// Wire form of [`schema::CellState`]: the KIND only. The raw and display
/// texts on the field already carry the content, so the typed value is not
/// re-sent; the reason for an invalid classification rides alongside as
/// `invalidReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CellClass {
    /// Field absent from the source record (a ragged short row).
    Missing,
    /// Matches a configured null token.
    NullToken,
    /// Empty/whitespace-only and not a configured token.
    Empty,
    /// Present and well-formed under the declared type (or plain text when the
    /// column has no schema entry).
    Valid,
    /// Present but does not parse as the declared type.
    Invalid,
}

impl CellClass {
    /// Split a classified state into its wire kind and (for `Invalid`) reason.
    fn split(state: &CellState) -> (CellClass, Option<String>) {
        match state {
            CellState::Missing => (CellClass::Missing, None),
            CellState::NullToken => (CellClass::NullToken, None),
            CellState::Empty => (CellClass::Empty, None),
            CellState::Valid(_) => (CellClass::Valid, None),
            CellState::Invalid(reason) => (CellClass::Invalid, Some(reason.clone())),
        }
    }
}

/// One field of the record form: the cell joined with everything the form
/// needs to label, validate and act on it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordField {
    /// Grid column position — the "jump back to grid column" target and the
    /// coordinate a draft edit is committed at.
    pub col: usize,
    /// Stable logical column ID (F12); dictionary and schema join on this.
    pub column_id: String,
    /// The technical header — the source of truth for the field name (the
    /// dictionary `displayName`, when present, is a friendlier label).
    pub header: String,
    /// The stored cell text, verbatim.
    pub raw: String,
    /// Display-formatted rendering (F31 [`schema::format_value`]). Equal to
    /// `raw` whenever no display pattern applies or the cell is not a valid
    /// typed value, so raw and display NEVER disagree about the stored value.
    pub display: String,
    /// Five-way classification of the STORED value under the declared schema.
    pub class: CellClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    /// Declared logical type, when the column carries a schema entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_type: Option<LogicalType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    /// Configured null tokens (empty unless the schema declares them). Drives
    /// the null-vs-blank control: only offered when this list is non-empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub null_tokens: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_mode: Option<ValidationMode>,
    /// Whether the STORED value currently satisfies the declared type (always
    /// `true` when the column has no schema). A stored value can be invalid
    /// when the schema was declared/tightened after the data landed — the
    /// form surfaces it even though a fresh strict edit could not create it.
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Joined data-dictionary entry (F38), by column ID: `displayName`,
    /// `description`, `unit`, and the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary: Option<DictionaryField>,
    /// Detected semantic badge (F26) from the last cached scan, by position.
    /// Best-effort/advisory: it reflects the report as scanned and may lag a
    /// structural edit until the next scan (the form reconciles via the
    /// semantic report's own revision).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_confidence: Option<f64>,
    /// Recorded advisory schema issues for THIS cell (absolute row + col),
    /// oldest first — the deliberate F31 gap the form closes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<SchemaIssue>,
}

/// The complete record-form view for one visible row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordView {
    /// The visible (display) row index requested.
    pub display_row: usize,
    /// The absolute (unfiltered) row index the fields were read from.
    pub abs_row: usize,
    /// Visible row count at fetch time — the bound for go-to-record and the
    /// prev/next-visible-record navigation.
    pub visible_len: usize,
    /// Read-only form: an indexed or follow-mode document accepts no edits.
    pub read_only: bool,
    /// Document revision the fields were read at. Any data / filter / view-sort
    /// change bumps it, so the form refetches (a display row can remap under a
    /// changed filter) and a later draft save is guarded against a stale row.
    pub revision: u64,
    /// Schema and dictionary revisions move INDEPENDENTLY of `revision`; the
    /// form refetches when either changes so its labels and badges never drift.
    pub schema_revision: u64,
    pub dictionary_revision: u64,
    pub fields: Vec<RecordField>,
}

/// Assemble the record view for ONE row, joining the declared schema, the data
/// dictionary and (when supplied) the cached semantic report. Pure and
/// bounded: `cells` is the single already-read row, `abs_row` its absolute
/// index. Ragged short rows are tolerated (missing cells read as absent).
pub fn assemble_record(
    doc: &Document,
    display_row: usize,
    abs_row: usize,
    cells: &[String],
    semantic: Option<&SemanticReport>,
) -> RecordView {
    let ids = doc.column_ids();
    let headers = doc.headers();
    let dictionary = doc.dictionary();
    let issues = doc.schema_issues();

    let mut fields = Vec::with_capacity(doc.n_cols());
    for col in 0..doc.n_cols() {
        let column_id = ids.get(col).cloned().unwrap_or_default();
        let header = headers.get(col).cloned().unwrap_or_default();
        // A cell absent from a ragged short row classifies as Missing; an
        // in-range cell carries its (possibly empty) stored text.
        let present = col < cells.len();
        let raw = cells.get(col).cloned().unwrap_or_default();

        let field = match doc.column_schema_at(col) {
            Some(cs) => {
                let raw_opt = present.then_some(raw.as_str());
                let (class, invalid_reason) = CellClass::split(&schema::classify(raw_opt, cs));
                // Display formatting only ever touches valid typed cells, and
                // returns raw otherwise — raw and display cannot disagree.
                let display = schema::format_value(cs, &raw);
                let verdict = schema::validate_value(cs, &raw);
                RecordFieldParts {
                    class,
                    invalid_reason,
                    display,
                    logical_type: Some(cs.logical_type),
                    nullable: Some(cs.nullable),
                    null_tokens: cs.null_tokens.clone(),
                    validation_mode: Some(cs.validation_mode),
                    valid: verdict.is_ok(),
                    reason: verdict.err(),
                }
            }
            None => {
                // No schema entry: the column is implicit plain text. A blank
                // is Empty, anything else is a well-formed text value; display
                // is the raw text unchanged.
                let class = if !present {
                    CellClass::Missing
                } else if raw.trim().is_empty() {
                    CellClass::Empty
                } else {
                    CellClass::Valid
                };
                RecordFieldParts {
                    class,
                    invalid_reason: None,
                    display: raw.clone(),
                    logical_type: None,
                    nullable: None,
                    null_tokens: Vec::new(),
                    validation_mode: None,
                    valid: true,
                    reason: None,
                }
            }
        };

        let (semantic_type, semantic_confidence) = semantic
            .and_then(|r| r.columns.iter().find(|c| c.column == col))
            .and_then(|c| c.detected.map(|d| (d, c.confidence)))
            .map_or((None, None), |(d, conf)| (Some(d), Some(conf)));

        let field_issues: Vec<SchemaIssue> = issues
            .iter()
            .filter(|i| i.row == abs_row && i.col == col)
            .cloned()
            .collect();

        fields.push(RecordField {
            col,
            dictionary: dictionary.field(&column_id).cloned(),
            column_id,
            header,
            raw,
            display: field.display,
            class: field.class,
            invalid_reason: field.invalid_reason,
            logical_type: field.logical_type,
            nullable: field.nullable,
            null_tokens: field.null_tokens,
            validation_mode: field.validation_mode,
            valid: field.valid,
            reason: field.reason,
            semantic: semantic_type,
            semantic_confidence,
            issues: field_issues,
        });
    }

    RecordView {
        display_row,
        abs_row,
        visible_len: doc.visible_len(),
        read_only: doc.ensure_editable().is_err(),
        revision: doc.revision(),
        schema_revision: doc.schema_revision(),
        dictionary_revision: doc.dictionary_revision(),
        fields,
    }
}

/// The schema-dependent parts of one field, kept together so the join loop can
/// build them from either the declared or the implicit-text branch.
struct RecordFieldParts {
    class: CellClass,
    invalid_reason: Option<String>,
    display: String,
    logical_type: Option<LogicalType>,
    nullable: Option<bool>,
    null_tokens: Vec<String>,
    validation_mode: Option<ValidationMode>,
    valid: bool,
    reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Draft validation (batched pre-check for the whole record)
// ---------------------------------------------------------------------------

/// One proposed field edit in a record draft. `col` is the grid column
/// position (validation is row-independent, so no row coordinate is needed).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftField {
    pub col: usize,
    pub value: String,
}

/// The verdict on one drafted field.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftFieldVerdict {
    pub col: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_id: Option<String>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The column's validation mode; `None` = no schema (anything goes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ValidationMode>,
    /// Whether THIS field's violation would BLOCK the commit — i.e. it is
    /// invalid AND its column is strict.
    pub blocks: bool,
}

/// The verdict on a whole record draft: per-field results plus whether the
/// batch save would be rejected, computed exactly the way
/// [`schema_ops::apply_validated_cells`] decides at commit time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftValidation {
    pub fields: Vec<DraftFieldVerdict>,
    /// Any strict column carries an invalid value: the `set_cells` batch would
    /// be rejected before ANY cell of the model changes.
    pub strict_blocks: bool,
    /// How many advisory columns would record an issue on save.
    pub advisory_warnings: usize,
    /// Document revision the verdict was computed against.
    pub revision: u64,
}

/// Validate a whole record draft against the declared schema, per field, and
/// summarise whether a real save would commit or be blocked. Reuses
/// [`schema_ops::check_edit`] so the verdict agrees, value-for-value, with the
/// strict/advisory decision [`schema_ops::apply_validated_cells`] makes.
pub fn validate_draft(doc: &Document, edits: &[DraftField]) -> DraftValidation {
    let mut fields = Vec::with_capacity(edits.len());
    let mut strict_blocks = false;
    let mut advisory_warnings = 0usize;

    for edit in edits {
        let verdict = schema_ops::check_edit(doc, edit.col, &edit.value);
        let strict = matches!(verdict.mode, Some(ValidationMode::Strict));
        let blocks = !verdict.valid && strict;
        if blocks {
            strict_blocks = true;
        }
        if !verdict.valid && !strict && verdict.mode.is_some() {
            advisory_warnings += 1;
        }
        fields.push(DraftFieldVerdict {
            col: edit.col,
            column_id: verdict.column_id,
            valid: verdict.valid,
            reason: verdict.reason,
            mode: verdict.mode,
            blocks,
        });
    }

    DraftValidation {
        fields,
        strict_blocks,
        advisory_warnings,
        revision: doc.revision(),
    }
}

// ---------------------------------------------------------------------------
// Per-row advisory schema-issue lookup
// ---------------------------------------------------------------------------

/// The recorded advisory schema issues for one ABSOLUTE row, oldest first —
/// the per-row slice of [`Document::schema_issues`] the record form surfaces.
pub fn issues_for_row(doc: &Document, abs_row: usize) -> Vec<SchemaIssue> {
    doc.schema_issues()
        .iter()
        .filter(|issue| issue.row == abs_row)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};
    use crate::schema::ColumnSchema;
    use crate::semantic::{ColumnSemantics, SemanticReport};

    fn doc_from_csv(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    /// Declare a schema for the column at `col`, returning its stable ID.
    fn declare(doc: &mut Document, col: usize, build: impl FnOnce(&mut ColumnSchema)) -> String {
        let id = doc.column_ids()[col].clone();
        let name = doc.headers()[col].clone();
        let mut cs = ColumnSchema::new(id.clone(), name, LogicalType::Text);
        build(&mut cs);
        doc.set_column_schema(cs);
        id
    }

    fn row_cells(doc: &Document, abs: usize) -> Vec<String> {
        doc.fetch_rows(&[abs]).unwrap().into_iter().next().unwrap()
    }

    fn fetch(doc: &Document, display: usize, semantic: Option<&SemanticReport>) -> RecordView {
        let abs = doc.display_to_abs(display).unwrap();
        assemble_record(doc, display, abs, &row_cells(doc, abs), semantic)
    }

    // ----- record fetch join correctness ----------------------------------

    #[test]
    fn fetch_joins_schema_dictionary_and_semantic_from_their_sources() {
        let mut doc = doc_from_csv("id,email,amount\n7,a@b.co,1500\n");

        // Schema on two columns; the third stays implicit text.
        let id_col = declare(&mut doc, 0, |cs| cs.logical_type = LogicalType::Integer);
        let amount_col = declare(&mut doc, 2, |cs| {
            cs.logical_type = LogicalType::Integer;
            cs.display_format = Some("thousands".into());
        });

        // Dictionary on the email column.
        let email_id = doc.column_ids()[1].clone();
        doc.set_dictionary_field(DictionaryField {
            column_id: email_id.clone(),
            display_name: Some("Customer email".into()),
            description: Some("Primary contact address".into()),
            unit: Some("email".into()),
            ..DictionaryField::empty(email_id.clone())
        });

        // A cached semantic report: email column detected as Email.
        let report = SemanticReport {
            revision: doc.revision(),
            sampled: false,
            scanned_rows: 1,
            threshold: 0.9,
            columns: vec![ColumnSemantics {
                column: 1,
                detected: Some(SemanticType::Email),
                best_candidate: Some(SemanticType::Email),
                confidence: 1.0,
                matching: 1,
                conflicting: 0,
                non_blank: 1,
            }],
        };

        let view = fetch(&doc, 0, Some(&report));
        assert_eq!(view.fields.len(), 3);
        assert!(!view.read_only);

        // Field 0: schema present, no dictionary, no semantic badge.
        let f_id = &view.fields[0];
        assert_eq!(f_id.column_id, id_col);
        assert_eq!(f_id.logical_type, Some(LogicalType::Integer));
        assert_eq!(f_id.class, CellClass::Valid);
        assert!(f_id.dictionary.is_none());
        assert!(f_id.semantic.is_none());

        // Field 1: dictionary joined by column ID, semantic badge joined by
        // position — each agreeing with its source.
        let f_email = &view.fields[1];
        assert_eq!(
            f_email.dictionary.as_ref().unwrap().display_name.as_deref(),
            Some("Customer email")
        );
        assert_eq!(f_email.semantic, Some(SemanticType::Email));
        assert_eq!(f_email.semantic_confidence, Some(1.0));
        assert!(f_email.logical_type.is_none(), "email column has no schema");

        // Field 2: display formatting matches F31 format_value exactly.
        let f_amount = &view.fields[2];
        assert_eq!(f_amount.column_id, amount_col);
        assert_eq!(f_amount.raw, "1500");
        let cs = doc.column_schema_at(2).unwrap();
        assert_eq!(f_amount.display, schema::format_value(cs, "1500"));
        assert_ne!(f_amount.display, f_amount.raw, "grouping applied");
    }

    #[test]
    fn raw_and_display_never_disagree_for_invalid_or_unpatterned_cells() {
        let mut doc = doc_from_csv("d\nnot-a-date\n");
        declare(&mut doc, 0, |cs| {
            cs.logical_type = LogicalType::Date;
            cs.display_format = Some("dd Mon yyyy".into());
        });
        let view = fetch(&doc, 0, None);
        let f = &view.fields[0];
        // Invalid under the type: display falls back to the raw stored text.
        assert_eq!(f.class, CellClass::Invalid);
        assert!(f.invalid_reason.is_some());
        assert_eq!(f.display, f.raw);
        assert_eq!(f.display, "not-a-date");
    }

    #[test]
    fn null_token_and_empty_are_distinguished() {
        // One row, two integer columns: the first holds a configured null token,
        // the second an empty cell (trailing comma) — so the classifier must
        // tell NullToken from Empty within the same record.
        let mut doc = doc_from_csv("tok,blank\nN/A,\n");
        declare(&mut doc, 0, |cs| {
            cs.logical_type = LogicalType::Integer;
            cs.nullable = true;
            cs.null_tokens = vec!["N/A".into()];
        });
        declare(&mut doc, 1, |cs| {
            cs.logical_type = LogicalType::Integer;
            cs.nullable = true;
        });

        let row = fetch(&doc, 0, None);
        assert_eq!(row.fields[0].class, CellClass::NullToken);
        assert_eq!(row.fields[0].null_tokens, vec!["N/A".to_string()]);
        assert_eq!(row.fields[1].class, CellClass::Empty);
    }

    // ----- draft validation matrix ----------------------------------------

    #[test]
    fn draft_validation_strict_advisory_and_mixed() {
        let mut doc = doc_from_csv("s,a,t\n1,1,hello\n");
        declare(&mut doc, 0, |cs| {
            cs.logical_type = LogicalType::Integer;
            cs.validation_mode = ValidationMode::Strict;
        });
        declare(&mut doc, 1, |cs| {
            cs.logical_type = LogicalType::Integer;
            cs.validation_mode = ValidationMode::Advisory;
        });
        // Column 2 stays implicit text (anything goes).

        // All valid: nothing blocks, nothing warns.
        let ok = validate_draft(
            &doc,
            &[
                DraftField {
                    col: 0,
                    value: "42".into(),
                },
                DraftField {
                    col: 1,
                    value: "43".into(),
                },
                DraftField {
                    col: 2,
                    value: "world".into(),
                },
            ],
        );
        assert!(!ok.strict_blocks);
        assert_eq!(ok.advisory_warnings, 0);
        assert!(ok.fields.iter().all(|f| f.valid && !f.blocks));

        // Advisory-only violation: warns, does not block.
        let advisory = validate_draft(
            &doc,
            &[DraftField {
                col: 1,
                value: "oops".into(),
            }],
        );
        assert!(!advisory.strict_blocks);
        assert_eq!(advisory.advisory_warnings, 1);
        assert!(!advisory.fields[0].valid);
        assert!(!advisory.fields[0].blocks);
        assert_eq!(advisory.fields[0].mode, Some(ValidationMode::Advisory));

        // Strict violation: blocks the whole batch.
        let strict = validate_draft(
            &doc,
            &[DraftField {
                col: 0,
                value: "nope".into(),
            }],
        );
        assert!(strict.strict_blocks);
        assert!(strict.fields[0].blocks);
        assert_eq!(strict.fields[0].mode, Some(ValidationMode::Strict));

        // Mixed: one strict-invalid, one advisory-invalid, one no-schema.
        let mixed = validate_draft(
            &doc,
            &[
                DraftField {
                    col: 0,
                    value: "bad".into(),
                },
                DraftField {
                    col: 1,
                    value: "bad".into(),
                },
                DraftField {
                    col: 2,
                    value: "anything".into(),
                },
            ],
        );
        assert!(mixed.strict_blocks, "the strict violation blocks");
        assert_eq!(mixed.advisory_warnings, 1);
        assert!(mixed.fields[0].blocks);
        assert!(!mixed.fields[1].blocks);
        assert!(mixed.fields[2].valid);
        assert!(
            mixed.fields[2].mode.is_none(),
            "no schema on the text column"
        );
    }

    #[test]
    fn draft_verdict_agrees_with_the_apply_path() {
        // The pre-check must match what a real save decides: a strict-invalid
        // batch is rejected by apply_validated_cells, and validate_draft says
        // strict_blocks; an advisory-invalid batch is accepted and recorded,
        // and validate_draft says it only warns. Built fresh each time since
        // apply mutates (Document is not Clone).
        let build = || {
            let mut doc = doc_from_csv("s,a\n1,1\n");
            declare(&mut doc, 0, |cs| {
                cs.logical_type = LogicalType::Integer;
                cs.validation_mode = ValidationMode::Strict;
            });
            declare(&mut doc, 1, |cs| {
                cs.logical_type = LogicalType::Integer;
                cs.validation_mode = ValidationMode::Advisory;
            });
            doc
        };

        let mut strict_doc = build();
        assert!(
            validate_draft(
                &strict_doc,
                &[DraftField {
                    col: 0,
                    value: "x".into()
                }]
            )
            .strict_blocks
        );
        assert!(
            schema_ops::apply_validated_cells(&mut strict_doc, vec![(0, 0, "x".into())]).is_err(),
            "strict-invalid really is rejected"
        );

        let mut advisory_doc = build();
        let v = validate_draft(
            &advisory_doc,
            &[DraftField {
                col: 1,
                value: "y".into(),
            }],
        );
        assert!(!v.strict_blocks);
        assert_eq!(v.advisory_warnings, 1);
        let recorded =
            schema_ops::apply_validated_cells(&mut advisory_doc, vec![(0, 1, "y".into())]).unwrap();
        assert_eq!(
            recorded, 1,
            "advisory-invalid really is accepted + recorded"
        );
    }

    // ----- one-undo batch verification (does not reimplement set_cells) ----

    #[test]
    fn multi_field_draft_save_is_one_undo_op() {
        let mut doc = doc_from_csv("a,b,c\n1,2,3\n");
        // The record form commits ALL changed fields of one row as a single
        // set_cells batch (here via the validated apply path the command uses).
        let changed = schema_ops::apply_validated_cells(
            &mut doc,
            vec![(0, 0, "X".into()), (0, 1, "Y".into()), (0, 2, "Z".into())],
        )
        .unwrap();
        assert_eq!(changed, 0, "no schema, no advisory issues");

        // Exactly ONE undoable operation for the whole batch.
        assert_eq!(doc.changes_since_save().len(), 1, "one undo op");
        assert_eq!(row_cells(&doc, 0), vec!["X", "Y", "Z"]);

        // A single undo reverts every field at once.
        doc.undo().unwrap();
        assert_eq!(row_cells(&doc, 0), vec!["1", "2", "3"]);
        assert!(!doc.can_undo());
    }

    // ----- per-row advisory schema-issue lookup ---------------------------

    #[test]
    fn issues_lookup_filters_to_the_requested_row() {
        let mut doc = doc_from_csv("n\n1\n2\n");
        declare(&mut doc, 0, |cs| {
            cs.logical_type = LogicalType::Integer;
            cs.validation_mode = ValidationMode::Advisory;
        });
        // Record an advisory issue on row 0 only.
        schema_ops::apply_validated_cells(&mut doc, vec![(0, 0, "bad".into())]).unwrap();

        assert_eq!(issues_for_row(&doc, 0).len(), 1);
        assert!(issues_for_row(&doc, 1).is_empty());

        // The record view attaches the same issue to the offending field.
        let view = fetch(&doc, 0, None);
        assert_eq!(view.fields[0].issues.len(), 1);
        assert_eq!(view.fields[0].issues[0].value, "bad");
        let clean = fetch(&doc, 1, None);
        assert!(clean.fields[0].issues.is_empty());
    }

    // ----- indexed document read-only enforcement -------------------------

    #[test]
    fn indexed_document_form_is_read_only_but_fetches() {
        use crate::index::{build_index, IndexSettings};

        let root = std::env::temp_dir().join(format!(
            "ceesvee-f41-record-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("data.csv");
        let csv = "a,b\n1,2\n3,4\n";
        std::fs::write(&source, csv.as_bytes()).unwrap();
        let indexed_file = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings::default(),
            &mut |_| Ok(()),
        )
        .unwrap();
        let mut doc = Document::from_index(2, Some(source), indexed_file);

        // The form still reads one row out of the index.
        let view = fetch(&doc, 1, None);
        assert!(view.read_only, "indexed document form is read-only");
        assert_eq!(view.fields.len(), 2);
        assert_eq!(view.fields[0].raw, "3");
        assert_eq!(view.fields[1].raw, "4");

        // Draft pre-checks still run (pure read), but a commit is refused.
        let _ = validate_draft(
            &doc,
            &[DraftField {
                col: 0,
                value: "9".into(),
            }],
        );
        assert!(matches!(
            schema_ops::apply_validated_cells(&mut doc, vec![(0, 0, "9".into())]),
            Err(crate::error::AppError::ReadOnly)
        ));

        drop(doc);
        let _ = std::fs::remove_dir_all(root);
    }
}
