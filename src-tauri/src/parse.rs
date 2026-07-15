//! Turn a raw byte buffer into an in-memory grid of string cells, auto-detecting
//! (or honouring overrides for) encoding and delimiter along the way.

use std::collections::HashMap;

use encoding_rs::Encoding;

use crate::error::{AppError, AppResult};
use crate::{delimiter, encoding};

/// Cap on retained ragged-record samples (the total count is always exact).
const RAGGED_SAMPLE_LIMIT: usize = 1000;

/// One source record whose field count differed from the modal count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaggedSample {
    /// 1-based line number in the source file where the record starts
    /// (embedded newlines inside quoted fields are accounted for).
    pub line: u64,
    /// Original field count of the record, before padding.
    pub fields: usize,
}

/// Fidelity information captured while parsing the source bytes. Describes the
/// file as it was read — not the current in-memory grid — so the diagnostics
/// panel can report import-time damage that normalisation has since hidden.
#[derive(Debug, Clone, Default)]
pub struct ImportInfo {
    /// Whether decoding replaced malformed byte sequences with U+FFFD.
    pub had_decode_errors: bool,
    /// Records whose original field count differed from the modal count.
    pub ragged_total: usize,
    /// First [`RAGGED_SAMPLE_LIMIT`] ragged records, in file order.
    pub ragged_samples: Vec<RaggedSample>,
    /// The most common field count (ties resolved toward the larger count).
    pub modal_field_count: usize,
}

/// The result of parsing a file: a ragged-normalised grid plus the settings
/// that were actually used (so the UI can show and override them).
pub struct ParsedFile {
    pub records: Vec<Vec<String>>,
    pub n_cols: usize,
    pub delimiter: u8,
    pub encoding: &'static Encoding,
    pub had_bom: bool,
    pub uses_crlf: bool,
    pub import: ImportInfo,
}

/// Optional overrides; `None` means "auto-detect".
#[derive(Default)]
pub struct ParseSettings {
    pub delimiter: Option<u8>,
    pub encoding: Option<&'static Encoding>,
}

/// Parse `bytes` into a [`ParsedFile`]. Rows shorter than the widest row are
/// padded with empty cells so the grid is rectangular.
pub fn parse(bytes: &[u8], settings: &ParseSettings) -> AppResult<ParsedFile> {
    // 1. Encoding: honour the override, else detect.
    let (encoding, had_bom) = match settings.encoding {
        Some(enc) => {
            let had_bom = Encoding::for_bom(bytes)
                .map(|(bom_enc, _)| bom_enc == enc)
                .unwrap_or(false);
            (enc, had_bom)
        }
        None => encoding::detect(bytes),
    };

    let (text, had_decode_errors) = encoding::decode(bytes, encoding);

    // A NUL byte in the decoded text is a strong signal this is a binary file,
    // not delimited text (real text encodings, incl. UTF-16, decode without
    // NULs). Reject early with a clear message instead of producing garbage.
    if text.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return Err(AppError::invalid(
            "this does not look like a delimited text file",
        ));
    }

    // 2. Delimiter: honour the override, else sniff.
    let delimiter = settings
        .delimiter
        .unwrap_or_else(|| delimiter::detect(&text));

    // 3. Line ending: CRLF if the file uses it anywhere, otherwise LF.
    let uses_crlf = text.contains("\r\n");

    // 4. Parse. We manage headers ourselves, so the reader treats every line as
    // a data record; `flexible` tolerates ragged rows.
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());

    let mut records: Vec<Vec<String>> = Vec::new();
    let mut n_cols = 0usize;
    // (start line, original field count) per record, for import fidelity.
    let mut shapes: Vec<(u64, usize)> = Vec::new();
    for result in reader.records() {
        let record = result?;
        let line = record.position().map(|p| p.line()).unwrap_or(0);
        let row: Vec<String> = record.iter().map(|field| field.to_string()).collect();
        shapes.push((line, row.len()));
        n_cols = n_cols.max(row.len());
        records.push(row);
    }

    // Normalise ragged rows to a rectangle.
    if records.iter().any(|row| row.len() < n_cols) {
        for row in &mut records {
            row.resize(n_cols, String::new());
        }
    }

    Ok(ParsedFile {
        records,
        n_cols,
        delimiter,
        encoding,
        had_bom,
        uses_crlf,
        import: import_info(had_decode_errors, &shapes),
    })
}

/// Summarise record shapes into an [`ImportInfo`]: find the modal field count
/// and collect the records that deviate from it.
fn import_info(had_decode_errors: bool, shapes: &[(u64, usize)]) -> ImportInfo {
    let mut histogram: HashMap<usize, usize> = HashMap::new();
    for &(_, fields) in shapes {
        *histogram.entry(fields).or_insert(0) += 1;
    }
    // Modal count; ties resolve toward the larger count so a 50/50 split
    // reports the shorter rows as the ragged ones.
    let modal_field_count = histogram
        .iter()
        .max_by_key(|&(count, freq)| (*freq, *count))
        .map(|(count, _)| *count)
        .unwrap_or(0);

    let mut ragged_total = 0usize;
    let mut ragged_samples = Vec::new();
    for &(line, fields) in shapes {
        if fields != modal_field_count {
            ragged_total += 1;
            if ragged_samples.len() < RAGGED_SAMPLE_LIMIT {
                ragged_samples.push(RaggedSample { line, fields });
            }
        }
    }

    ImportInfo {
        had_decode_errors,
        ragged_total,
        ragged_samples,
        modal_field_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::UTF_8;

    #[test]
    fn parses_simple_csv() {
        let parsed = parse(b"a,b,c\n1,2,3\n4,5,6", &ParseSettings::default()).unwrap();
        assert_eq!(parsed.delimiter, b',');
        assert_eq!(parsed.n_cols, 3);
        assert_eq!(parsed.records.len(), 3);
        assert_eq!(parsed.records[1], vec!["1", "2", "3"]);
    }

    #[test]
    fn pads_ragged_rows() {
        let parsed = parse(b"a,b,c\n1,2\n4", &ParseSettings::default()).unwrap();
        assert_eq!(parsed.n_cols, 3);
        assert_eq!(parsed.records[1], vec!["1", "2", ""]);
        assert_eq!(parsed.records[2], vec!["4", "", ""]);
    }

    #[test]
    fn honours_quoted_fields_with_embedded_delimiter() {
        let parsed = parse(b"name,note\n\"Doe, John\",hi", &ParseSettings::default()).unwrap();
        assert_eq!(parsed.records[1], vec!["Doe, John", "hi"]);
    }

    #[test]
    fn detects_crlf() {
        let parsed = parse(b"a,b\r\n1,2\r\n", &ParseSettings::default()).unwrap();
        assert!(parsed.uses_crlf);
    }

    #[test]
    fn rejects_binary_with_nul() {
        // A buffer containing NUL bytes (e.g. the start of a zip/binary) is
        // rejected rather than parsed into garbage rows.
        assert!(parse(
            b"PK\x03\x04\x00\x00\x08\x00garbage",
            &ParseSettings::default()
        )
        .is_err());
    }

    #[test]
    fn respects_delimiter_override() {
        let settings = ParseSettings {
            delimiter: Some(b';'),
            encoding: Some(UTF_8),
        };
        // A comma-looking line but forced to split on ';'.
        let parsed = parse(b"a,b;c,d", &settings).unwrap();
        assert_eq!(parsed.records[0], vec!["a,b", "c,d"]);
    }

    #[test]
    fn import_info_reports_ragged_records_with_lines() {
        let parsed = parse(b"a,b,c\n1,2\n4,5,6\n7\n", &ParseSettings::default()).unwrap();
        let info = &parsed.import;
        assert_eq!(info.modal_field_count, 3);
        assert_eq!(info.ragged_total, 2);
        assert_eq!(
            info.ragged_samples,
            vec![
                RaggedSample { line: 2, fields: 2 },
                RaggedSample { line: 4, fields: 1 },
            ]
        );
        assert!(!info.had_decode_errors);
    }

    #[test]
    fn import_info_accounts_for_quoted_embedded_newlines() {
        // The quoted field spans two physical lines, so the record after it
        // starts on line 4, not line 3.
        let parsed = parse(b"a,b\n\"x\ny\",2\n3", &ParseSettings::default()).unwrap();
        let info = &parsed.import;
        assert_eq!(info.modal_field_count, 2);
        assert_eq!(info.ragged_total, 1);
        assert_eq!(
            info.ragged_samples,
            vec![RaggedSample { line: 4, fields: 1 }]
        );
    }

    #[test]
    fn import_info_flags_decode_errors() {
        // 0xFF is not valid UTF-8, so forcing UTF-8 produces a replacement
        // character and sets the decode-error flag.
        let settings = ParseSettings {
            delimiter: Some(b','),
            encoding: Some(UTF_8),
        };
        let parsed = parse(b"a,b\n\xFF,2\n", &settings).unwrap();
        assert!(parsed.import.had_decode_errors);
        assert!(parsed.records[1][0].contains('\u{FFFD}'));
    }

    #[test]
    fn import_info_clean_file_reports_nothing() {
        let parsed = parse(b"a,b\n1,2\n3,4\n", &ParseSettings::default()).unwrap();
        assert!(!parsed.import.had_decode_errors);
        assert_eq!(parsed.import.ragged_total, 0);
        assert!(parsed.import.ragged_samples.is_empty());
    }
}
