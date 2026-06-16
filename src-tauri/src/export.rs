//! Serialize a [`Document`] back to delimited bytes with explicit control over
//! delimiter, quoting, line endings, target encoding and BOM.

use csv::{QuoteStyle, Terminator, WriterBuilder};

use crate::document::{Document, LineEnding};
use crate::dto::ExportOptions;
use crate::error::{AppError, AppResult};
use crate::{encoding, util};

/// Serialize `doc` using `opts`, returning the exact bytes to write to disk.
pub fn serialize(doc: &Document, opts: &ExportOptions) -> AppResult<Vec<u8>> {
    let delimiter = util::delimiter_to_byte(&opts.delimiter);
    let quote_style = match opts.quote_style.as_str() {
        "always" => QuoteStyle::Always,
        "never" => QuoteStyle::Never,
        // "minimal" / "necessary" / anything else
        _ => QuoteStyle::Necessary,
    };
    let terminator = match LineEnding::parse(&opts.line_ending) {
        LineEnding::Crlf => Terminator::CRLF,
        LineEnding::Lf => Terminator::Any(b'\n'),
    };

    let mut writer = WriterBuilder::new()
        .delimiter(delimiter)
        .quote_style(quote_style)
        .terminator(terminator)
        .from_writer(Vec::<u8>::new());

    if doc.has_header_row() && opts.include_headers {
        writer.write_record(doc.headers())?;
    }
    for row in doc.rows() {
        writer.write_record(row)?;
    }
    writer.flush()?;

    let utf8 = writer
        .into_inner()
        .map_err(|e| AppError::Other(e.to_string()))?;
    // The CSV writer only emits ASCII structure plus the (UTF-8) field bytes, so
    // the buffer is always valid UTF-8.
    let text = String::from_utf8(utf8).map_err(|e| AppError::Other(e.to_string()))?;

    let target = encoding::from_name(&opts.encoding);
    let mut out = Vec::with_capacity(text.len() + 3);
    if opts.bom {
        out.extend_from_slice(encoding::bom_for(target));
    }
    out.extend_from_slice(&encoding::encode(&text, target));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn export_options() -> ExportOptions {
        ExportOptions {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            quote_style: "minimal".into(),
            line_ending: "lf".into(),
            bom: false,
            include_headers: true,
        }
    }

    fn doc(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    #[test]
    fn round_trip_preserves_data() {
        let original = "name,note\nAda,\"hello, world\"\nBob,plain";
        let d = doc(original, true);
        let bytes = serialize(&d, &export_options()).unwrap();
        let reparsed = parse(&bytes, &ParseSettings::default()).unwrap();
        assert_eq!(reparsed.records[0], vec!["name", "note"]);
        assert_eq!(reparsed.records[1], vec!["Ada", "hello, world"]);
        assert_eq!(reparsed.records[2], vec!["Bob", "plain"]);
    }

    #[test]
    fn always_quote_style() {
        let d = doc("a,b\n1,2", true);
        let mut opts = export_options();
        opts.quote_style = "always".into();
        let bytes = serialize(&d, &opts).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "\"a\",\"b\"\n\"1\",\"2\"\n");
    }

    #[test]
    fn crlf_line_endings() {
        let d = doc("a,b\n1,2", true);
        let mut opts = export_options();
        opts.line_ending = "crlf".into();
        let bytes = serialize(&d, &opts).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\r\n"));
    }

    #[test]
    fn utf8_bom_is_prepended() {
        let d = doc("a\nx", true);
        let mut opts = export_options();
        opts.bom = true;
        let bytes = serialize(&d, &opts).unwrap();
        assert_eq!(&bytes[0..3], &[0xEF, 0xBB, 0xBF]);
    }

    #[test]
    fn custom_delimiter_and_no_header() {
        let d = doc("1;2\n3;4", false);
        let mut opts = export_options();
        opts.delimiter = ";".into();
        opts.include_headers = true; // ignored: no header row exists
        let bytes = serialize(&d, &opts).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "1;2\n3;4\n");
    }
}
