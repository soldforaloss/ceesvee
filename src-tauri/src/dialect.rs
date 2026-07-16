//! Advanced CSV dialect and preamble import (F18): open real-world "CSV"
//! files with preambles, comment lines, unusual quoting/escaping, multi-row
//! headers, and skipped trailing records. Every knob is a closed, validated
//! option; the preview keeps ORIGINAL record numbers so preamble exclusion
//! is visible; a save writes only the current grid, so skipped preamble or
//! comment records are never silently re-added. Changing the dialect flows
//! through the same guarded reparse workflow as F02 (a dirty document is
//! never reinterpreted without explicit confirmation).

use serde::{Deserialize, Serialize};

use crate::encoding;
use crate::error::{AppError, AppResult};
use crate::parse::{ImportInfo, ParsedFile};

/// Records shown in a dialect preview.
const PREVIEW_RECORDS: usize = 50;

/// The full dialect (a closed set of validated options).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvDialectOptions {
    /// One Unicode scalar representable as a single byte by the parser.
    pub delimiter: String,
    /// `None` disables quoting entirely.
    pub quote_character: Option<String>,
    /// `""` inside quotes escapes a quote (RFC 4180 style).
    #[serde(default = "default_true")]
    pub double_quote: bool,
    /// Backslash-style escape character, if any.
    #[serde(default)]
    pub escape_character: Option<String>,
    /// Records STARTING with this are ignored (never inside quotes).
    #[serde(default)]
    pub comment_prefix: Option<String>,
    /// Records dropped from the front (metadata preambles).
    #[serde(default)]
    pub skip_leading_records: usize,
    /// Records dropped from the end (footers, totals).
    #[serde(default)]
    pub skip_trailing_records: usize,
    /// Which post-skip record holds the headers (`None` = no header row).
    #[serde(default)]
    pub header_row_index: Option<usize>,
    /// How many consecutive records combine into the headers.
    #[serde(default = "default_one")]
    pub header_row_count: usize,
    /// Joiner between combined header fragments.
    #[serde(default = "default_joiner")]
    pub header_joiner: String,
    /// Tokens treated as null by ANALYSIS (raw text is always retained;
    /// normalizing them is the explicit F29 repair step).
    #[serde(default)]
    pub null_tokens: Vec<String>,
    /// Encoding override (`None` = auto-detect).
    #[serde(default)]
    pub encoding: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_one() -> usize {
    1
}
fn default_joiner() -> String {
    " ".to_string()
}

fn single_byte(label: &str, value: &str) -> AppResult<u8> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii() => Ok(c as u8),
        _ => Err(AppError::invalid(format!(
            "{label} must be a single ASCII character"
        ))),
    }
}

pub fn validate(dialect: &CsvDialectOptions) -> AppResult<()> {
    single_byte("the delimiter", &dialect.delimiter)?;
    if let Some(q) = &dialect.quote_character {
        single_byte("the quote character", q)?;
    }
    if let Some(e) = &dialect.escape_character {
        single_byte("the escape character", e)?;
    }
    if let Some(c) = &dialect.comment_prefix {
        single_byte("the comment prefix", c)?;
    }
    if dialect.header_row_count == 0 {
        return Err(AppError::invalid(
            "the header must span at least one record",
        ));
    }
    if dialect.header_row_count > 1 && dialect.header_row_index.is_none() {
        return Err(AppError::invalid(
            "combining header rows needs a header row index",
        ));
    }
    Ok(())
}

/// The outcome of a dialect parse: the grid plus everything the preview
/// needs to explain itself.
pub struct DialectParsed {
    /// Post-skip, post-comment data records (headers already extracted).
    pub records: Vec<Vec<String>>,
    pub n_cols: usize,
    /// Combined headers (`None` = no header row configured).
    pub headers: Option<Vec<String>>,
    /// 1-based ORIGINAL record numbers (pre-skip, pre-comment) per data row.
    pub original_numbers: Vec<usize>,
    /// Combined header names appearing more than once.
    pub duplicate_headers: Vec<String>,
    /// Cells matching a configured null token (analysis-level counts only —
    /// the raw text is untouched).
    pub null_token_cells: usize,
    pub encoding_name: String,
    pub had_bom: bool,
    pub uses_crlf: bool,
}

/// Parse bytes under a dialect. Deterministic; never touches any document.
pub fn parse_with_dialect(bytes: &[u8], dialect: &CsvDialectOptions) -> AppResult<DialectParsed> {
    validate(dialect)?;
    let enc_override = dialect.encoding.as_deref().map(encoding::from_name);
    let (enc, had_bom) = match enc_override {
        Some(e) => (
            e,
            encoding_rs::Encoding::for_bom(bytes)
                .map(|(b, _)| b == e)
                .unwrap_or(false),
        ),
        None => encoding::detect(bytes),
    };
    let (text, _) = encoding::decode(bytes, enc);
    let uses_crlf = text.contains("\r\n");

    let mut builder = csv::ReaderBuilder::new();
    builder
        .delimiter(single_byte("delimiter", &dialect.delimiter)?)
        .has_headers(false)
        .flexible(true);
    match &dialect.quote_character {
        Some(q) => {
            builder.quote(single_byte("quote", q)?);
            builder.double_quote(dialect.double_quote);
        }
        None => {
            builder.quoting(false);
        }
    }
    builder.escape(match &dialect.escape_character {
        Some(e) => Some(single_byte("escape", e)?),
        None => None,
    });
    builder.comment(match &dialect.comment_prefix {
        Some(c) => Some(single_byte("comment", c)?),
        None => None,
    });

    // Track ORIGINAL record numbers: the csv reader reports byte positions
    // per record; we count records ourselves including comment lines by
    // reading positions' line numbers.
    let mut reader = builder.from_reader(text.as_bytes());
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut numbers: Vec<usize> = Vec::new();
    let mut record = csv::StringRecord::new();
    loop {
        let position_line = reader.position().line();
        match reader.read_record(&mut record) {
            Ok(true) => {
                records.push(record.iter().map(|s| s.to_string()).collect());
                numbers.push(position_line as usize);
            }
            Ok(false) => break,
            Err(e) => {
                return Err(AppError::invalid(format!(
                    "the file does not parse under this dialect: {e}"
                )));
            }
        }
    }

    // Leading / trailing skips.
    let skip_front = dialect.skip_leading_records.min(records.len());
    let skip_back = dialect
        .skip_trailing_records
        .min(records.len() - skip_front);
    let end = records.len() - skip_back;
    let mut records: Vec<Vec<String>> = records.drain(skip_front..end).collect();
    let mut numbers: Vec<usize> = numbers.drain(skip_front..end).collect();

    // Header extraction (post-skip indexing).
    let mut headers: Option<Vec<String>> = None;
    let mut duplicate_headers: Vec<String> = Vec::new();
    if let Some(index) = dialect.header_row_index {
        let count = dialect.header_row_count;
        if index + count > records.len() {
            return Err(AppError::invalid(
                "the header rows fall outside the remaining records",
            ));
        }
        let width = records[index..index + count]
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0);
        let mut combined: Vec<String> = Vec::with_capacity(width);
        for c in 0..width {
            let parts: Vec<&str> = records[index..index + count]
                .iter()
                .filter_map(|r| r.get(c).map(String::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            combined.push(parts.join(&dialect.header_joiner));
        }
        // Duplicate combined headers are reported (and the caller can make
        // them unique deterministically on apply).
        let mut seen = std::collections::HashSet::new();
        for h in &combined {
            if !h.is_empty() && !seen.insert(h.clone()) && !duplicate_headers.contains(h) {
                duplicate_headers.push(h.clone());
            }
        }
        headers = Some(combined);
        // Remove the header records (and everything before them counts as
        // additional preamble).
        records.drain(..index + count);
        numbers.drain(..index + count);
    }

    // Rectangularise to the widest row (headers included).
    let n_cols = records
        .iter()
        .map(Vec::len)
        .chain(headers.iter().map(Vec::len))
        .max()
        .unwrap_or(0)
        .max(1);
    for row in &mut records {
        row.resize(n_cols, String::new());
    }
    if let Some(h) = &mut headers {
        h.resize(n_cols, String::new());
    }

    let null_token_cells = if dialect.null_tokens.is_empty() {
        0
    } else {
        let tokens: Vec<&str> = dialect
            .null_tokens
            .iter()
            .map(String::as_str)
            .filter(|t| !t.trim().is_empty())
            .collect();
        records
            .iter()
            .flat_map(|r| r.iter())
            .filter(|cell| tokens.contains(&cell.trim()))
            .count()
    };

    Ok(DialectParsed {
        records,
        n_cols,
        headers,
        original_numbers: numbers,
        duplicate_headers,
        null_token_cells,
        encoding_name: enc.name().to_string(),
        had_bom,
        uses_crlf,
    })
}

/// Convert a dialect parse into the ordinary [`ParsedFile`] shape, making
/// duplicate combined headers unique deterministically (" (2)", " (3)", …).
/// Returns the file plus whether a header row is present.
pub fn into_parsed_file(parsed: DialectParsed, dialect: &CsvDialectOptions) -> (ParsedFile, bool) {
    let DialectParsed {
        mut records,
        n_cols,
        headers,
        had_bom,
        uses_crlf,
        encoding_name,
        ..
    } = parsed;
    let has_headers = headers.is_some();
    if let Some(headers) = headers {
        let mut unique: Vec<String> = Vec::with_capacity(headers.len());
        for h in headers {
            let name = crate::derived::unique_column_name(&unique, &h);
            unique.push(name);
        }
        records.insert(0, unique);
    }
    let file = ParsedFile {
        records,
        n_cols,
        delimiter: dialect.delimiter.bytes().next().unwrap_or(b','),
        encoding: encoding::from_name(&encoding_name),
        had_bom,
        uses_crlf,
        import: ImportInfo::default(),
    };
    (file, has_headers)
}

/// Bounded preview DTO for the dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialectPreview {
    /// First data rows (post skip/comment/header extraction).
    pub sample: Vec<Vec<String>>,
    /// 1-based ORIGINAL record numbers for the sampled rows.
    pub original_numbers: Vec<usize>,
    pub headers: Option<Vec<String>>,
    pub duplicate_headers: Vec<String>,
    pub total_rows: usize,
    pub n_cols: usize,
    pub null_token_cells: usize,
    pub encoding: String,
    /// The dialect echoed back (what actually applied).
    pub effective: CsvDialectOptions,
}

pub fn preview(bytes: &[u8], dialect: &CsvDialectOptions) -> AppResult<DialectPreview> {
    let parsed = parse_with_dialect(bytes, dialect)?;
    Ok(DialectPreview {
        sample: parsed
            .records
            .iter()
            .take(PREVIEW_RECORDS)
            .cloned()
            .collect(),
        original_numbers: parsed
            .original_numbers
            .iter()
            .take(PREVIEW_RECORDS)
            .copied()
            .collect(),
        headers: parsed.headers.clone(),
        duplicate_headers: parsed.duplicate_headers.clone(),
        total_rows: parsed.records.len(),
        n_cols: parsed.n_cols,
        null_token_cells: parsed.null_token_cells,
        encoding: parsed.encoding_name.clone(),
        effective: dialect.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialect() -> CsvDialectOptions {
        CsvDialectOptions {
            delimiter: ",".into(),
            quote_character: Some("\"".into()),
            double_quote: true,
            escape_character: None,
            comment_prefix: None,
            skip_leading_records: 0,
            skip_trailing_records: 0,
            header_row_index: Some(0),
            header_row_count: 1,
            header_joiner: " ".into(),
            null_tokens: vec![],
            encoding: None,
        }
    }

    #[test]
    fn preambles_and_footers_are_excluded_with_original_numbers_kept() {
        let bytes = b"# metadata line\nExported by tool v3\nid,name\n1,ann\n2,bob\nTOTAL,2\n";
        let mut d = dialect();
        d.skip_leading_records = 2;
        d.skip_trailing_records = 1;
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(
            parsed.headers.as_deref(),
            Some(&["id".to_string(), "name".into()][..])
        );
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.records[0], vec!["1", "ann"]);
        // Original record numbers survive the preamble skip (1-based).
        assert_eq!(parsed.original_numbers, vec![4, 5]);
    }

    #[test]
    fn comment_lines_are_ignored_but_not_inside_quotes() {
        let bytes = b"id,note\n# a comment line\n1,\"#not a comment\"\n";
        let mut d = dialect();
        d.comment_prefix = Some("#".into());
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.records[0][1], "#not a comment");
    }

    #[test]
    fn multi_row_headers_combine_deterministically_and_report_duplicates() {
        let bytes = b"Amount,Amount,Name\nNet,Gross,\n1,2,ann\n";
        let mut d = dialect();
        d.header_row_count = 2;
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(
            parsed.headers.as_deref(),
            Some(
                &[
                    "Amount Net".to_string(),
                    "Amount Gross".into(),
                    "Name".into()
                ][..]
            )
        );
        assert!(parsed.duplicate_headers.is_empty());

        // Identical combined headers ARE reported.
        let bytes = b"A,A\nx,x\n1,2\n";
        let mut d = dialect();
        d.header_row_count = 2;
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(parsed.duplicate_headers, vec!["A x".to_string()]);
        // …and become unique when converted for opening.
        let (file, has_headers) = into_parsed_file(parsed, &d);
        assert!(has_headers);
        assert_eq!(file.records[0], vec!["A x", "A x (2)"]);
    }

    #[test]
    fn custom_escape_and_disabled_quoting_follow_the_dialect() {
        // Backslash escaping replaces quote doubling inside quoted fields.
        let bytes = b"a,b\n1,\"val\\\"ue\"\n";
        let mut d = dialect();
        d.escape_character = Some("\\".into());
        d.double_quote = false;
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(parsed.records[0][1], "val\"ue", "escaped quote is data");

        // Doubled quotes under the default RFC dialect.
        let bytes = b"a,b\n1,\"he said \"\"hi\"\"\"\n";
        let parsed = parse_with_dialect(bytes, &dialect()).unwrap();
        assert_eq!(parsed.records[0][1], "he said \"hi\"");

        // Quoting disabled: quotes are ordinary characters.
        let bytes = b"a,b\n\"x\",y\n";
        let mut d = dialect();
        d.quote_character = None;
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(parsed.records[0][0], "\"x\"");
    }

    #[test]
    fn null_tokens_are_counted_but_never_normalized() {
        let bytes = b"a,b\nNA,1\nx,NA\n";
        let mut d = dialect();
        d.null_tokens = vec!["NA".into()];
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(parsed.null_token_cells, 2);
        assert_eq!(parsed.records[0][0], "NA", "raw text retained");
    }

    #[test]
    fn preview_and_final_parse_agree_on_dimensions() {
        let bytes = b"# skip\nid,name\n1,ann\n2,bob\n";
        let mut d = dialect();
        d.skip_leading_records = 1;
        let p = preview(bytes, &d).unwrap();
        let parsed = parse_with_dialect(bytes, &d).unwrap();
        assert_eq!(p.total_rows, parsed.records.len());
        assert_eq!(p.n_cols, parsed.n_cols);
        assert_eq!(p.sample.len(), 2);
        assert_eq!(p.effective.skip_leading_records, 1);
    }

    #[test]
    fn invalid_dialects_are_rejected() {
        let mut d = dialect();
        d.delimiter = "ab".into();
        assert!(validate(&d).is_err());
        let mut d = dialect();
        d.header_row_count = 0;
        assert!(validate(&d).is_err());
        let mut d = dialect();
        d.header_row_count = 2;
        d.header_row_index = None;
        assert!(validate(&d).is_err());
        let mut d = dialect();
        d.header_row_index = Some(10);
        assert!(parse_with_dialect(b"a\n1\n", &d).is_err());
    }
}
