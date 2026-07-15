//! Non-destructive "reopen with settings" support (F02): build a preview of
//! what reparsing the source file would produce, and diff it against the
//! current interpretation. Nothing here mutates a document — the swap itself
//! happens in the `apply_reparse` command, guarded by the preview's revision.

use crate::document::Document;
use crate::dto::{ReparseDiff, ReparsePreview};
use crate::parse::ParsedFile;

/// How many ragged-record samples ride along on a preview.
const PREVIEW_RAGGED_SAMPLES: usize = 20;

/// Snapshot of how the open document currently interprets its source, taken
/// under the document's read lock.
#[derive(Debug, Clone)]
pub struct CurrentInterpretation {
    pub delimiter: String,
    pub encoding: String,
    pub had_bom: bool,
    pub line_ending: String,
    pub has_header_row: bool,
    pub row_count: usize,
    pub col_count: usize,
    pub revision: u64,
}

impl CurrentInterpretation {
    pub fn of(doc: &Document) -> CurrentInterpretation {
        let meta = doc.meta();
        CurrentInterpretation {
            delimiter: meta.delimiter,
            encoding: meta.encoding,
            had_bom: meta.had_bom,
            line_ending: meta.line_ending,
            has_header_row: meta.has_header_row,
            row_count: meta.total_row_count,
            col_count: meta.col_count,
            revision: meta.revision,
        }
    }
}

/// Build the preview for a completed (but unapplied) parse. `has_header` is
/// the effective header mode; `max_rows` bounds the records carried to the UI.
pub fn build_preview(
    parsed: ParsedFile,
    has_header: bool,
    current: &CurrentInterpretation,
    max_rows: usize,
) -> ReparsePreview {
    let delimiter = String::from_utf8_lossy(&[parsed.delimiter]).to_string();
    let encoding = parsed.encoding.name().to_string();
    let line_ending = if parsed.uses_crlf { "crlf" } else { "lf" };
    let row_count = parsed
        .records
        .len()
        .saturating_sub(usize::from(has_header && !parsed.records.is_empty()));
    let col_count = parsed.n_cols;

    let mut differences = Vec::new();
    let mut diff = |field: &str, cur: String, new: String| {
        if cur != new {
            differences.push(ReparseDiff {
                field: field.to_string(),
                current: cur,
                proposed: new,
            });
        }
    };
    diff("delimiter", current.delimiter.clone(), delimiter.clone());
    diff("encoding", current.encoding.clone(), encoding.clone());
    diff(
        "bom",
        current.had_bom.to_string(),
        parsed.had_bom.to_string(),
    );
    diff(
        "lineEnding",
        current.line_ending.clone(),
        line_ending.to_string(),
    );
    diff(
        "headerMode",
        current.has_header_row.to_string(),
        has_header.to_string(),
    );
    diff(
        "rowCount",
        current.row_count.to_string(),
        row_count.to_string(),
    );
    diff(
        "colCount",
        current.col_count.to_string(),
        col_count.to_string(),
    );

    let mut records = parsed.records;
    records.truncate(max_rows);

    ReparsePreview {
        records,
        delimiter,
        encoding,
        had_bom: parsed.had_bom,
        line_ending: line_ending.to_string(),
        has_header_row: has_header,
        row_count,
        col_count,
        had_decode_errors: parsed.import.had_decode_errors,
        ragged_total: parsed.import.ragged_total,
        modal_field_count: parsed.import.modal_field_count,
        ragged_samples: parsed
            .import
            .ragged_samples
            .iter()
            .take(PREVIEW_RAGGED_SAMPLES)
            .cloned()
            .collect(),
        differences,
        expected_revision: current.revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn current() -> CurrentInterpretation {
        CurrentInterpretation {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            had_bom: false,
            line_ending: "lf".into(),
            has_header_row: true,
            row_count: 2,
            col_count: 2,
            revision: 7,
        }
    }

    #[test]
    fn preview_matches_unchanged_interpretation() {
        let parsed = parse(b"a,b\n1,2\n3,4\n", &ParseSettings::default()).unwrap();
        let preview = build_preview(parsed, true, &current(), 100);
        assert_eq!(preview.records.len(), 3, "header + 2 data rows");
        assert_eq!(preview.row_count, 2);
        assert_eq!(preview.col_count, 2);
        assert_eq!(preview.expected_revision, 7);
        assert!(preview.differences.is_empty(), "{:?}", preview.differences);
    }

    #[test]
    fn preview_reports_differences_for_new_settings() {
        let settings = ParseSettings {
            delimiter: Some(b';'),
            encoding: None,
        };
        // Semicolon-delimited view of the same bytes: 1 column instead of 2,
        // and header mode turned off.
        let parsed = parse(b"a,b\n1,2\n3,4\n", &settings).unwrap();
        let preview = build_preview(parsed, false, &current(), 100);
        let fields: Vec<&str> = preview
            .differences
            .iter()
            .map(|d| d.field.as_str())
            .collect();
        assert!(fields.contains(&"delimiter"));
        assert!(fields.contains(&"headerMode"));
        assert!(fields.contains(&"rowCount"), "2 -> 3 without a header");
        assert!(fields.contains(&"colCount"), "2 -> 1 with ';'");
        let delim = preview
            .differences
            .iter()
            .find(|d| d.field == "delimiter")
            .unwrap();
        assert_eq!(delim.current, ",");
        assert_eq!(delim.proposed, ";");
    }

    #[test]
    fn preview_truncates_records_but_counts_everything() {
        let csv = "h\n1\n2\n3\n4\n5\n";
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        let preview = build_preview(parsed, true, &current(), 3);
        assert_eq!(preview.records.len(), 3, "truncated to max_rows");
        assert_eq!(preview.row_count, 5, "count reflects the full file");
    }

    #[test]
    fn preview_carries_import_diagnostics() {
        let parsed = parse(b"a,b,c\n1,2\n4,5,6\n", &ParseSettings::default()).unwrap();
        let preview = build_preview(parsed, true, &current(), 100);
        assert_eq!(preview.ragged_total, 1);
        assert_eq!(preview.modal_field_count, 3);
        assert_eq!(preview.ragged_samples.len(), 1);
        assert_eq!(preview.ragged_samples[0].line, 2);
        assert!(!preview.had_decode_errors);
    }

    #[test]
    fn empty_file_previews_safely() {
        let parsed = parse(b"", &ParseSettings::default()).unwrap();
        let preview = build_preview(parsed, true, &current(), 100);
        assert_eq!(preview.row_count, 0);
        assert_eq!(preview.records.len(), 0);
    }
}
