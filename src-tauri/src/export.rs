//! Serialize a [`Document`] to delimited output with explicit control over
//! delimiter, quoting, line endings, target encoding and BOM.
//!
//! Serialization STREAMS (F03): rows pass through the CSV writer into a small
//! buffer that is transcoded and drained to the sink at record boundaries, so
//! saving a 100 MB document never materialises a second 100 MB buffer.
//! Characters the target encoding cannot represent fail the export instead of
//! being silently substituted.

use std::io::Write;

use csv::{QuoteStyle, Terminator, WriterBuilder};
use encoding_rs::{Encoding, UTF_8};

use crate::document::{Document, LineEnding};
use crate::dto::ExportOptions;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::{encoding, util};

/// Drain the staging buffer to the sink once it grows past this.
pub(crate) const FLUSH_THRESHOLD: usize = 64 * 1024;

/// Stream the whole of `doc` into `out` using `opts`. Returns the total bytes
/// written. Progress (rows + bytes) and cancellation flow through `ctx`.
pub fn write_document<W: Write>(
    doc: &Document,
    opts: &ExportOptions,
    out: &mut W,
    ctx: Option<&JobCtx>,
) -> AppResult<u64> {
    let cols: Vec<usize> = (0..doc.n_cols()).collect();
    write_rows(
        doc,
        RowSelection::Range(0..doc.n_rows()),
        &cols,
        opts,
        out,
        ctx,
    )
}

/// Stream a row/column subset of `doc` (F04 scoped exports). `rows` are
/// absolute row indices and `cols` column indices, both in output order;
/// callers are responsible for range-validating them.
pub fn write_view<W: Write>(
    doc: &Document,
    rows: &[usize],
    cols: &[usize],
    opts: &ExportOptions,
    out: &mut W,
    ctx: Option<&JobCtx>,
) -> AppResult<u64> {
    write_rows(doc, RowSelection::Indices(rows), cols, opts, out, ctx)
}

/// Which rows to stream: a contiguous range (whole document) or explicit
/// absolute indices (scoped exports). Both stream through the document's
/// visit API, so exports work identically for editable and indexed backings.
enum RowSelection<'a> {
    Range(std::ops::Range<usize>),
    Indices(&'a [usize]),
}

/// The `csv` quote style an [`ExportOptions`] string selects.
pub(crate) fn quote_style_of(opts: &ExportOptions) -> QuoteStyle {
    match opts.quote_style.as_str() {
        "always" => QuoteStyle::Always,
        "never" => QuoteStyle::Never,
        // "minimal" / "necessary" / anything else
        _ => QuoteStyle::Necessary,
    }
}

/// The `csv` record terminator an [`ExportOptions`] string selects.
pub(crate) fn terminator_of(opts: &ExportOptions) -> Terminator {
    match LineEnding::parse(&opts.line_ending) {
        LineEnding::Crlf => Terminator::CRLF,
        LineEnding::Lf => Terminator::Any(b'\n'),
    }
}

fn write_rows<W: Write>(
    doc: &Document,
    rows: RowSelection<'_>,
    cols: &[usize],
    opts: &ExportOptions,
    out: &mut W,
    ctx: Option<&JobCtx>,
) -> AppResult<u64> {
    let delimiter = util::delimiter_to_byte(&opts.delimiter);
    let quote_style = quote_style_of(opts);
    let terminator = terminator_of(opts);
    let target = encoding::from_name(&opts.encoding);

    let make_writer = |buf: Vec<u8>| {
        WriterBuilder::new()
            .delimiter(delimiter)
            .quote_style(quote_style)
            .terminator(terminator)
            .from_writer(buf)
    };
    let drain = |writer: csv::Writer<Vec<u8>>| -> AppResult<Vec<u8>> {
        writer
            .into_inner()
            .map_err(|e| AppError::Other(e.to_string()))
    };

    let mut total: u64 = 0;

    if opts.bom {
        let bom = encoding::bom_for(target);
        out.write_all(bom)?;
        total += bom.len() as u64;
        if let Some(ctx) = ctx {
            ctx.add_bytes(bom.len() as u64);
        }
    }

    let mut writer = make_writer(Vec::with_capacity(FLUSH_THRESHOLD + 8 * 1024));
    if doc.has_header_row() && opts.include_headers {
        let headers = doc.headers();
        writer.write_record(cols.iter().map(|&c| headers[c].as_bytes()))?;
    }

    // Rough size of what's been fed to the writer since the last drain; used
    // to decide when to rotate (the csv writer hides its inner buffer).
    let mut estimated = 0usize;
    let mut pending_rows = 0u64;

    {
        let mut write_one = |row: &[String]| -> AppResult<bool> {
            writer.write_record(cols.iter().map(|&c| row[c].as_bytes()))?;
            pending_rows += 1;
            estimated += cols.iter().map(|&c| row[c].len()).sum::<usize>() + cols.len() + 2;

            if estimated >= FLUSH_THRESHOLD {
                writer.flush()?;
                let rotated = std::mem::replace(&mut writer, make_writer(Vec::new()));
                let mut buf = drain(rotated)?;
                total += transcode_chunk(&mut buf, target, out, ctx)?;
                writer = make_writer(buf); // reuse the (now cleared) allocation
                estimated = 0;
                if let Some(ctx) = ctx {
                    ctx.advance(pending_rows)?;
                }
                pending_rows = 0;
            }
            Ok(true)
        };
        match rows {
            RowSelection::Range(range) => doc.visit_rows(range, &mut |_, row| write_one(row))?,
            RowSelection::Indices(indices) => {
                doc.visit_rows_at(indices, &mut |_, row| write_one(row))?
            }
        }
    }

    writer.flush()?;
    let mut buf = drain(writer)?;
    total += transcode_chunk(&mut buf, target, out, ctx)?;
    if let Some(ctx) = ctx {
        ctx.advance(pending_rows)?;
        ctx.flush_progress();
    }

    Ok(total)
}

/// Transcode one UTF-8 chunk to the target encoding and write it out,
/// clearing the buffer. Record-aligned chunks keep multi-byte characters
/// intact. Fails on unmappable characters instead of substituting.
/// Shared with the [`crate::tabular`] CSV sink.
pub(crate) fn transcode_chunk<W: Write>(
    buf: &mut Vec<u8>,
    target: &'static Encoding,
    out: &mut W,
    ctx: Option<&JobCtx>,
) -> AppResult<u64> {
    if buf.is_empty() {
        return Ok(0);
    }
    let written: u64 = if target == UTF_8 {
        out.write_all(buf)?;
        buf.len() as u64
    } else {
        // The CSV writer only emits ASCII structure plus the (UTF-8) field
        // bytes, so the chunk is always valid UTF-8.
        let text = std::str::from_utf8(buf).map_err(|e| AppError::Other(e.to_string()))?;
        let (encoded, lossy) = encoding::encode_checked(text, target);
        if lossy {
            return Err(AppError::invalid(format!(
                "some characters cannot be represented in {} — export with UTF-8 instead, \
                 or fix the affected cells",
                target.name()
            )));
        }
        out.write_all(&encoded)?;
        encoded.len() as u64
    };
    if let Some(ctx) = ctx {
        ctx.add_bytes(written);
    }
    buf.clear();
    Ok(written)
}

/// Serialize `doc` into one in-memory buffer. Test helper: production writes
/// stream through [`write_document`] into a file sink instead of building the
/// whole output in memory.
#[cfg(test)]
pub fn serialize(doc: &Document, opts: &ExportOptions) -> AppResult<Vec<u8>> {
    let mut out = Vec::new();
    write_document(doc, opts, &mut out, None)?;
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
            backup: Default::default(),
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

    #[test]
    fn windows_1252_round_trips_when_representable() {
        let d = doc("name\ncafé", true);
        let mut opts = export_options();
        opts.encoding = "windows-1252".into();
        let bytes = serialize(&d, &opts).unwrap();
        let settings = ParseSettings {
            delimiter: Some(b','),
            encoding: Some(encoding_rs::WINDOWS_1252),
        };
        let reparsed = parse(&bytes, &settings).unwrap();
        assert_eq!(reparsed.records[1], vec!["café"]);
    }

    #[test]
    fn unmappable_characters_fail_instead_of_substituting() {
        let d = doc("name\na → b", true);
        let mut opts = export_options();
        opts.encoding = "windows-1252".into();
        let err = serialize(&d, &opts).unwrap_err();
        assert!(
            err.to_string().contains("cannot be represented"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn streaming_crosses_chunk_boundaries_losslessly() {
        // Enough multi-byte content to force several 64 KiB chunk rotations;
        // record-aligned draining must never split a UTF-8 sequence.
        let mut src = String::from("col\n");
        for i in 0..20_000 {
            src.push_str(&format!("värde-⚡-{i}\n"));
        }
        let d = doc(&src, true);
        let bytes = serialize(&d, &export_options()).unwrap();
        let reparsed = parse(&bytes, &ParseSettings::default()).unwrap();
        assert_eq!(reparsed.records.len(), 20_001);
        assert_eq!(reparsed.records[1][0], "värde-⚡-0");
        assert_eq!(reparsed.records[20_000][0], "värde-⚡-19999");
    }

    #[test]
    fn utf16_output_streams_correctly() {
        let d = doc("a,b\nx,ü", true);
        let mut opts = export_options();
        opts.encoding = "UTF-16LE".into();
        opts.bom = true;
        let bytes = serialize(&d, &opts).unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xFE]);
        let settings = ParseSettings {
            delimiter: Some(b','),
            encoding: Some(encoding_rs::UTF_16LE),
        };
        let reparsed = parse(&bytes, &settings).unwrap();
        assert_eq!(reparsed.records[1], vec!["x", "ü"]);
    }
}
