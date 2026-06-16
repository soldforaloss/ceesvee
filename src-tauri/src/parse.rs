//! Turn a raw byte buffer into an in-memory grid of string cells, auto-detecting
//! (or honouring overrides for) encoding and delimiter along the way.

use encoding_rs::Encoding;

use crate::error::{AppError, AppResult};
use crate::{delimiter, encoding};

/// The result of parsing a file: a ragged-normalised grid plus the settings
/// that were actually used (so the UI can show and override them).
pub struct ParsedFile {
    pub records: Vec<Vec<String>>,
    pub n_cols: usize,
    pub delimiter: u8,
    pub encoding: &'static Encoding,
    pub had_bom: bool,
    pub uses_crlf: bool,
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

    let (text, _had_errors) = encoding::decode(bytes, encoding);

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
    for result in reader.records() {
        let record = result?;
        let row: Vec<String> = record.iter().map(|field| field.to_string()).collect();
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
    })
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
}
