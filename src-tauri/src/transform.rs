//! Previewable data-cleaning transformations (F06): a fixed set of cleanup
//! operations (no formulas, no scripting, no arbitrary expressions), each
//! previewed before it is applied and applied as ONE undoable operation.
//!
//! Cell transforms compute their full change list under the document's read
//! lock (cancellable), then commit under a brief write lock guarded by the
//! preview's revision. Split/merge go through
//! [`Document::replace_columns`] so structure, headers and values revert
//! together on a single undo.

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;
use crate::job::JobCtx;

const EXAMPLE_LIMIT: usize = 20;
const FAILURE_EXAMPLE_LIMIT: usize = 10;
const ROW_CHUNK: usize = 4096;

/// The supported cleanup operations. Deliberately closed: no user-supplied
/// code ever executes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransformSpec {
    Trim,
    CollapseWhitespace,
    Uppercase,
    Lowercase,
    TitleCase,
    ReplaceText {
        find: String,
        replace: String,
        case_sensitive: bool,
    },
    ReplaceRegex {
        pattern: String,
        replace: String,
    },
    FillBlank {
        value: String,
    },
    NormalizeBooleans {
        true_value: String,
        false_value: String,
    },
    NormalizeDates {
        /// strftime output format, e.g. "%Y-%m-%d".
        format: String,
    },
    NormalizeNumbers {
        /// Whether the source uses a decimal comma ("1.234,56").
        decimal_comma: bool,
    },
    AddPrefix {
        prefix: String,
    },
    AddSuffix {
        suffix: String,
    },
    SplitByDelimiter {
        column: usize,
        delimiter: String,
    },
    SplitByRegex {
        column: usize,
        pattern: String,
    },
    MergeColumns {
        columns: Vec<usize>,
        separator: String,
    },
}

/// What to do when a cell cannot be converted (bad date/number/boolean).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransformErrorPolicy {
    FailAll,
    SkipInvalid,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformExample {
    pub row: usize,
    pub col: usize,
    pub before: String,
    pub after: String,
}

/// Everything the preview shows; `expected_revision` echoes back to apply.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformPreview {
    pub affected_cells: usize,
    pub parse_failures: usize,
    pub examples: Vec<TransformExample>,
    pub failure_examples: Vec<TransformExample>,
    pub columns_inserted: Vec<String>,
    pub columns_removed: Vec<String>,
    /// True when the operation's VALUES touch every row regardless of the row
    /// scope (merging removes the source columns for all rows).
    pub applies_to_all_rows: bool,
    pub expected_revision: u64,
}

/// A computed transform, ready to commit.
#[derive(Debug)]
pub struct Computed {
    pub preview: TransformPreview,
    pub changes: Changes,
}

#[derive(Debug)]
pub enum Changes {
    Cells(Vec<(usize, usize, String)>),
    Structure {
        remove: Vec<usize>,
        insert_at: usize,
        columns: Vec<(String, Vec<String>)>,
    },
}

// ----- compiled cell operations ---------------------------------------------------

enum Compiled {
    Trim,
    Collapse,
    Upper,
    Lower,
    Title,
    ReplaceText {
        find: String,
        replace: String,
        case_sensitive: bool,
    },
    ReplaceRegex {
        regex: regex::Regex,
        replace: String,
    },
    FillBlank {
        value: String,
    },
    Booleans {
        true_value: String,
        false_value: String,
    },
    Dates {
        format: String,
    },
    Numbers {
        decimal_comma: bool,
    },
    Prefix(String),
    Suffix(String),
}

/// Validate parameters and pre-compile patterns. Every user error (bad regex,
/// bad date format) surfaces HERE — before anything is scanned or mutated.
fn compile(spec: &TransformSpec) -> AppResult<Compiled> {
    Ok(match spec {
        TransformSpec::Trim => Compiled::Trim,
        TransformSpec::CollapseWhitespace => Compiled::Collapse,
        TransformSpec::Uppercase => Compiled::Upper,
        TransformSpec::Lowercase => Compiled::Lower,
        TransformSpec::TitleCase => Compiled::Title,
        TransformSpec::ReplaceText {
            find,
            replace,
            case_sensitive,
        } => {
            if find.is_empty() {
                return Err(AppError::invalid("the text to find cannot be empty"));
            }
            Compiled::ReplaceText {
                find: find.clone(),
                replace: replace.clone(),
                case_sensitive: *case_sensitive,
            }
        }
        TransformSpec::ReplaceRegex { pattern, replace } => Compiled::ReplaceRegex {
            regex: regex::Regex::new(pattern)
                .map_err(|e| AppError::invalid(format!("invalid regex: {e}")))?,
            replace: replace.clone(),
        },
        TransformSpec::FillBlank { value } => Compiled::FillBlank {
            value: value.clone(),
        },
        TransformSpec::NormalizeBooleans {
            true_value,
            false_value,
        } => Compiled::Booleans {
            true_value: true_value.clone(),
            false_value: false_value.clone(),
        },
        TransformSpec::NormalizeDates { format } => {
            // Reject invalid strftime specs up front (chrono would otherwise
            // surface them as a formatting error mid-write).
            let items: Vec<_> = chrono::format::StrftimeItems::new(format).collect();
            if items
                .iter()
                .any(|i| matches!(i, chrono::format::Item::Error))
            {
                return Err(AppError::invalid("invalid date format string"));
            }
            Compiled::Dates {
                format: format.clone(),
            }
        }
        TransformSpec::NormalizeNumbers { decimal_comma } => Compiled::Numbers {
            decimal_comma: *decimal_comma,
        },
        TransformSpec::AddPrefix { prefix } => Compiled::Prefix(prefix.clone()),
        TransformSpec::AddSuffix { suffix } => Compiled::Suffix(suffix.clone()),
        TransformSpec::SplitByDelimiter { .. }
        | TransformSpec::SplitByRegex { .. }
        | TransformSpec::MergeColumns { .. } => {
            unreachable!("structural specs are handled separately")
        }
    })
}

/// Apply one compiled operation to one cell. `Ok(None)` = unchanged/skipped;
/// `Err(())` = the cell cannot be converted (a parse failure).
fn apply_cell(op: &Compiled, value: &str) -> Result<Option<String>, ()> {
    let changed = |new: String| if new == value { None } else { Some(new) };
    match op {
        Compiled::Trim => Ok(changed(value.trim().to_string())),
        Compiled::Collapse => {
            let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
            Ok(changed(collapsed))
        }
        Compiled::Upper => Ok(changed(value.to_uppercase())),
        Compiled::Lower => Ok(changed(value.to_lowercase())),
        Compiled::Title => Ok(changed(title_case(value))),
        Compiled::ReplaceText {
            find,
            replace,
            case_sensitive,
        } => {
            let new = if *case_sensitive {
                value.replace(find.as_str(), replace)
            } else {
                replace_case_insensitive(value, find, replace)
            };
            Ok(changed(new))
        }
        Compiled::ReplaceRegex { regex, replace } => Ok(changed(
            regex.replace_all(value, replace.as_str()).into_owned(),
        )),
        Compiled::FillBlank { value: fill } => {
            if value.trim().is_empty() && !fill.is_empty() {
                Ok(Some(fill.clone()))
            } else {
                Ok(None)
            }
        }
        Compiled::Booleans {
            true_value,
            false_value,
        } => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            match trimmed.to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "t" | "1" => Ok(changed(true_value.clone())),
                "false" | "no" | "n" | "f" | "0" => Ok(changed(false_value.clone())),
                _ => Err(()),
            }
        }
        Compiled::Dates { format } => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            match analyze::parse_date(trimmed) {
                Some(parsed) => Ok(changed(parsed.format(format).to_string())),
                None => Err(()),
            }
        }
        Compiled::Numbers { decimal_comma } => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let cleaned = if *decimal_comma {
                trimmed.replace(['.', ' '], "").replace(',', ".")
            } else {
                trimmed.replace([',', ' '], "")
            };
            if analyze::as_number(&cleaned).is_some() {
                Ok(changed(cleaned))
            } else {
                Err(())
            }
        }
        Compiled::Prefix(prefix) => {
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(changed(format!("{prefix}{value}")))
            }
        }
        Compiled::Suffix(suffix) => {
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(changed(format!("{value}{suffix}")))
            }
        }
    }
}

fn title_case(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut at_word_start = true;
    for c in value.chars() {
        if c.is_alphanumeric() {
            if at_word_start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            at_word_start = false;
        } else {
            out.push(c);
            at_word_start = true;
        }
    }
    out
}

fn replace_case_insensitive(value: &str, find: &str, replace: &str) -> String {
    let lower_value = value.to_lowercase();
    let lower_find = find.to_lowercase();
    if lower_find.is_empty() {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(pos) = lower_value[cursor..].find(&lower_find) {
        let start = cursor + pos;
        // The lowercase haystack keeps char boundaries aligned closely enough
        // for find(), but slice the ORIGINAL string by byte offsets computed
        // on the lowercase one only when they are valid boundaries.
        if !value.is_char_boundary(start) || !value.is_char_boundary(start + find.len()) {
            break;
        }
        out.push_str(&value[cursor..start]);
        out.push_str(replace);
        cursor = start + find.len();
    }
    out.push_str(&value[cursor..]);
    out
}

// ----- computing --------------------------------------------------------------------

/// Compute the full effect of a transform without mutating anything.
pub fn compute(
    doc: &Document,
    spec: &TransformSpec,
    scope: &ExportScope,
    ctx: Option<&JobCtx>,
) -> AppResult<Computed> {
    match spec {
        TransformSpec::SplitByDelimiter { column, delimiter } => {
            if delimiter.is_empty() {
                return Err(AppError::invalid("the split delimiter cannot be empty"));
            }
            compute_split(doc, *column, scope, ctx, |cell| {
                cell.split(delimiter.as_str()).map(str::to_string).collect()
            })
        }
        TransformSpec::SplitByRegex { column, pattern } => {
            let regex = regex::Regex::new(pattern)
                .map_err(|e| AppError::invalid(format!("invalid regex: {e}")))?;
            compute_split(doc, *column, scope, ctx, move |cell| {
                regex.split(cell).map(str::to_string).collect()
            })
        }
        TransformSpec::MergeColumns { columns, separator } => {
            compute_merge(doc, columns, separator, ctx)
        }
        _ => compute_cells(doc, spec, scope, ctx),
    }
}

fn compute_cells(
    doc: &Document,
    spec: &TransformSpec,
    scope: &ExportScope,
    ctx: Option<&JobCtx>,
) -> AppResult<Computed> {
    let op = compile(spec)?;
    let resolved = resolve_scope(doc, scope)?;
    if let Some(ctx) = ctx {
        ctx.set_total(resolved.rows.len() as u64);
    }

    let rows = doc.rows();
    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut failures = 0usize;
    let mut examples = Vec::new();
    let mut failure_examples = Vec::new();

    for (i, &r) in resolved.rows.iter().enumerate() {
        if let Some(ctx) = ctx {
            if i.is_multiple_of(ROW_CHUNK) {
                ctx.advance(if i == 0 { 0 } else { ROW_CHUNK as u64 })?;
            }
        }
        for &c in &resolved.cols {
            let value = &rows[r][c];
            match apply_cell(&op, value) {
                Ok(Some(new)) => {
                    if examples.len() < EXAMPLE_LIMIT {
                        examples.push(TransformExample {
                            row: r,
                            col: c,
                            before: value.clone(),
                            after: new.clone(),
                        });
                    }
                    changes.push((r, c, new));
                }
                Ok(None) => {}
                Err(()) => {
                    failures += 1;
                    if failure_examples.len() < FAILURE_EXAMPLE_LIMIT {
                        failure_examples.push(TransformExample {
                            row: r,
                            col: c,
                            before: value.clone(),
                            after: String::new(),
                        });
                    }
                }
            }
        }
    }
    if let Some(ctx) = ctx {
        ctx.flush_progress();
    }

    Ok(Computed {
        preview: TransformPreview {
            affected_cells: changes.len(),
            parse_failures: failures,
            examples,
            failure_examples,
            columns_inserted: Vec::new(),
            columns_removed: Vec::new(),
            applies_to_all_rows: false,
            expected_revision: doc.revision(),
        },
        changes: Changes::Cells(changes),
    })
}

fn compute_split(
    doc: &Document,
    column: usize,
    scope: &ExportScope,
    ctx: Option<&JobCtx>,
    split: impl Fn(&str) -> Vec<String>,
) -> AppResult<Computed> {
    if column >= doc.n_cols() {
        return Err(AppError::invalid("split column out of range"));
    }
    let resolved = resolve_scope(doc, scope)?;
    let in_scope: std::collections::HashSet<usize> = resolved.rows.iter().copied().collect();
    let rows = doc.rows();
    if let Some(ctx) = ctx {
        ctx.set_total(rows.len() as u64);
    }

    // First pass: how many parts does the widest scoped cell produce?
    let mut parts_per_row: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    let mut max_parts = 1usize;
    let mut affected = 0usize;
    let mut examples = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        if let Some(ctx) = ctx {
            if r.is_multiple_of(ROW_CHUNK) {
                ctx.advance(if r == 0 { 0 } else { ROW_CHUNK as u64 })?;
            }
        }
        let cell = &row[column];
        // Rows outside the scope keep their value intact in the first part.
        let parts = if in_scope.contains(&r) {
            split(cell)
        } else {
            vec![cell.clone()]
        };
        if parts.len() > 1 {
            affected += 1;
            if examples.len() < EXAMPLE_LIMIT {
                examples.push(TransformExample {
                    row: r,
                    col: column,
                    before: cell.clone(),
                    after: parts.join(" ⇒ "),
                });
            }
        }
        max_parts = max_parts.max(parts.len());
        parts_per_row.push(parts);
    }
    if let Some(ctx) = ctx {
        ctx.flush_progress();
    }

    let base = doc.headers()[column].clone();
    let base = if base.trim().is_empty() {
        format!("Column {}", column + 1)
    } else {
        base
    };
    let mut columns: Vec<(String, Vec<String>)> = (0..max_parts)
        .map(|i| (format!("{base} {}", i + 1), Vec::with_capacity(rows.len())))
        .collect();
    for parts in &parts_per_row {
        for (i, slot) in columns.iter_mut().enumerate() {
            slot.1.push(parts.get(i).cloned().unwrap_or_default());
        }
    }

    Ok(Computed {
        preview: TransformPreview {
            affected_cells: affected,
            parse_failures: 0,
            examples,
            failure_examples: Vec::new(),
            columns_inserted: columns.iter().map(|(h, _)| h.clone()).collect(),
            columns_removed: vec![doc.headers()[column].clone()],
            applies_to_all_rows: false,
            expected_revision: doc.revision(),
        },
        changes: Changes::Structure {
            remove: vec![column],
            insert_at: column,
            columns,
        },
    })
}

fn compute_merge(
    doc: &Document,
    columns: &[usize],
    separator: &str,
    ctx: Option<&JobCtx>,
) -> AppResult<Computed> {
    if columns.len() < 2 {
        return Err(AppError::invalid("merging needs at least two columns"));
    }
    let mut unique = columns.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != columns.len() {
        return Err(AppError::invalid("merge columns must be distinct"));
    }
    if unique.last().is_some_and(|&c| c >= doc.n_cols()) {
        return Err(AppError::invalid("merge column out of range"));
    }

    let rows = doc.rows();
    if let Some(ctx) = ctx {
        ctx.set_total(rows.len() as u64);
    }
    let mut values = Vec::with_capacity(rows.len());
    let mut examples = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        if let Some(ctx) = ctx {
            if r.is_multiple_of(ROW_CHUNK) {
                ctx.advance(if r == 0 { 0 } else { ROW_CHUNK as u64 })?;
            }
        }
        // Merge in the USER'S column order, not sorted order.
        let joined = columns
            .iter()
            .map(|&c| row[c].as_str())
            .collect::<Vec<_>>()
            .join(separator);
        if examples.len() < EXAMPLE_LIMIT && !joined.is_empty() {
            examples.push(TransformExample {
                row: r,
                col: columns[0],
                before: row[columns[0]].clone(),
                after: joined.clone(),
            });
        }
        values.push(joined);
    }
    if let Some(ctx) = ctx {
        ctx.flush_progress();
    }

    let headers = doc.headers();
    let merged_header = columns
        .iter()
        .map(|&c| {
            let h = headers[c].trim();
            if h.is_empty() {
                format!("Column {}", c + 1)
            } else {
                h.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("+");
    let insert_at = *unique.first().expect("validated non-empty");

    Ok(Computed {
        preview: TransformPreview {
            affected_cells: rows.len(),
            parse_failures: 0,
            examples,
            failure_examples: Vec::new(),
            columns_inserted: vec![merged_header.clone()],
            columns_removed: unique.iter().map(|&c| headers[c].clone()).collect(),
            // Removing the source columns necessarily rewrites every row.
            applies_to_all_rows: true,
            expected_revision: doc.revision(),
        },
        changes: Changes::Structure {
            remove: unique,
            insert_at,
            columns: vec![(merged_header, values)],
        },
    })
}

/// Commit a computed transform to the document as one undoable operation.
pub fn commit(doc: &mut Document, changes: Changes) -> AppResult<()> {
    match changes {
        Changes::Cells(cells) => doc.set_cells(cells),
        Changes::Structure {
            remove,
            insert_at,
            columns,
        } => doc.replace_columns(remove, insert_at, columns),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn run(doc: &Document, spec: TransformSpec, scope: ExportScope) -> Computed {
        compute(doc, &spec, &scope, None).unwrap()
    }

    #[test]
    fn cell_transforms_behave() {
        let cases: Vec<(TransformSpec, &str, &str)> = vec![
            (TransformSpec::Trim, "  x  ", "x"),
            (TransformSpec::CollapseWhitespace, "a   b\t c", "a b c"),
            (TransformSpec::Uppercase, "café", "CAFÉ"),
            (TransformSpec::Lowercase, "CaFé", "café"),
            (
                TransformSpec::TitleCase,
                "ada del rey-smith",
                "Ada Del Rey-Smith",
            ),
            (
                TransformSpec::ReplaceText {
                    find: "old".into(),
                    replace: "new".into(),
                    case_sensitive: true,
                },
                "old Old old",
                "new Old new",
            ),
            (
                TransformSpec::ReplaceText {
                    find: "old".into(),
                    replace: "new".into(),
                    case_sensitive: false,
                },
                "old OLD",
                "new new",
            ),
            (
                TransformSpec::ReplaceRegex {
                    pattern: r"(\d+)-(\d+)".into(),
                    replace: "$2/$1".into(),
                },
                "10-20",
                "20/10",
            ),
            (
                TransformSpec::NormalizeBooleans {
                    true_value: "TRUE".into(),
                    false_value: "FALSE".into(),
                },
                "yes",
                "TRUE",
            ),
            (
                TransformSpec::NormalizeDates {
                    format: "%Y-%m-%d".into(),
                },
                "03/01/2024",
                "2024-03-01",
            ),
            (
                TransformSpec::NormalizeNumbers {
                    decimal_comma: true,
                },
                "1.234,56",
                "1234.56",
            ),
            (
                TransformSpec::NormalizeNumbers {
                    decimal_comma: false,
                },
                "1,234.56",
                "1234.56",
            ),
            (
                TransformSpec::AddPrefix {
                    prefix: "ID-".into(),
                },
                "42",
                "ID-42",
            ),
            (TransformSpec::AddSuffix { suffix: "%".into() }, "42", "42%"),
        ];
        for (spec, before, after) in cases {
            let op = compile(&spec).unwrap();
            assert_eq!(
                apply_cell(&op, before).unwrap().as_deref(),
                Some(after),
                "{spec:?}"
            );
        }

        // Fill blank fills only blanks.
        let fill = compile(&TransformSpec::FillBlank {
            value: "n/a".into(),
        })
        .unwrap();
        assert_eq!(apply_cell(&fill, " ").unwrap().as_deref(), Some("n/a"));
        assert_eq!(apply_cell(&fill, "kept").unwrap(), None);
    }

    #[test]
    fn preview_reports_counts_examples_and_failures_without_mutating() {
        let mut d = doc_from("b\nyes\nno\nmaybe\n");
        let before_rev = d.revision();
        let computed = run(
            &d,
            TransformSpec::NormalizeBooleans {
                true_value: "1".into(),
                false_value: "0".into(),
            },
            ExportScope::All,
        );
        assert_eq!(computed.preview.affected_cells, 2);
        assert_eq!(computed.preview.parse_failures, 1);
        assert_eq!(computed.preview.failure_examples[0].before, "maybe");
        assert_eq!(d.revision(), before_rev, "preview never mutates");

        // Applying with the change list is one undo step.
        commit(&mut d, computed.changes).unwrap();
        assert_eq!(d.rows()[0][0], "1");
        assert_eq!(d.rows()[2][0], "maybe", "invalid cell skipped");
        d.undo().unwrap();
        assert_eq!(d.rows()[0][0], "yes");
    }

    #[test]
    fn filtered_transforms_do_not_modify_hidden_rows() {
        let mut d = doc_from("v\n a \n b \n c ");
        d.set_filter(vec![0, 2]); // rows " a " and " c " visible
        let computed = run(&d, TransformSpec::Trim, ExportScope::VisibleRows);
        assert_eq!(computed.preview.affected_cells, 2);
        commit(&mut d, computed.changes).unwrap();
        assert_eq!(d.rows()[0][0], "a");
        assert_eq!(d.rows()[1][0], " b ", "hidden row untouched");
        assert_eq!(d.rows()[2][0], "c");
    }

    #[test]
    fn regex_errors_are_reported_before_mutation() {
        let d = doc_from("v\nx");
        let err = compute(
            &d,
            &TransformSpec::ReplaceRegex {
                pattern: "([".into(),
                replace: "".into(),
            },
            &ExportScope::All,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid regex"));

        let err = compute(
            &d,
            &TransformSpec::NormalizeDates {
                format: "%Q".into(),
            },
            &ExportScope::All,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("date format"));
    }

    #[test]
    fn split_keeps_document_rectangular_and_respects_row_scope() {
        let mut d = doc_from("name,age\nAda Lovelace,36\nBob,40\nCher X Y,50");
        d.set_filter(vec![0, 2]); // Bob's row is hidden
        let computed = run(
            &d,
            TransformSpec::SplitByDelimiter {
                column: 0,
                delimiter: " ".into(),
            },
            ExportScope::VisibleRows,
        );
        assert_eq!(computed.preview.columns_removed, vec!["name"]);
        assert_eq!(
            computed.preview.columns_inserted,
            vec!["name 1", "name 2", "name 3"]
        );
        commit(&mut d, computed.changes).unwrap();

        assert_eq!(d.headers(), &["name 1", "name 2", "name 3", "age"]);
        assert!(d.rows().iter().all(|r| r.len() == 4), "rectangular");
        assert_eq!(
            d.rows()[0][..3],
            ["Ada".to_string(), "Lovelace".into(), "".into()]
        );
        // The hidden row's value is preserved unsplit in the first part.
        assert_eq!(d.rows()[1][..3], ["Bob".to_string(), "".into(), "".into()]);
        assert_eq!(
            d.rows()[2][..3],
            ["Cher".to_string(), "X".into(), "Y".into()]
        );

        // One undo restores everything.
        d.undo().unwrap();
        assert_eq!(d.headers(), &["name", "age"]);
        assert_eq!(d.rows()[0][0], "Ada Lovelace");
    }

    #[test]
    fn merge_joins_in_user_order_and_is_single_undo() {
        let mut d = doc_from("first,last,age\nAda,Lovelace,36\nBob,Ray,40");
        let computed = run(
            &d,
            TransformSpec::MergeColumns {
                columns: vec![1, 0], // user order: last first
                separator: ", ".into(),
            },
            ExportScope::All,
        );
        assert!(computed.preview.applies_to_all_rows);
        assert_eq!(computed.preview.columns_inserted, vec!["last+first"]);
        commit(&mut d, computed.changes).unwrap();

        assert_eq!(d.headers(), &["last+first", "age"]);
        assert_eq!(d.rows()[0][0], "Lovelace, Ada");
        assert!(d.rows().iter().all(|r| r.len() == 2), "rectangular");

        d.undo().unwrap();
        assert_eq!(d.headers(), &["first", "last", "age"]);
        assert_eq!(d.rows()[1][1], "Ray");
    }

    #[test]
    fn compute_is_cancellable() {
        let d = doc_from("v\n1\n2");
        let registry = JobRegistry::default();
        let ctx = registry.begin("transform", Some(1), |_| {});
        registry.cancel(ctx.id);
        let result = compute(&d, &TransformSpec::Trim, &ExportScope::All, Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
    }
}
