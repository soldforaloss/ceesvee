//! Copy As (F14): serialize a row/column selection into structured clipboard
//! formats. All heavy work happens here in Rust — the front end receives one
//! finished string. Rows stream through the backing-aware visit API, so
//! copying off-screen or indexed rows never depends on the grid cache.

use serde::Deserialize;

use crate::document::Document;
use crate::error::{AppError, AppResult};

/// Wire format selector for `copy_as`.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CopyFormat {
    /// Tab-separated, quoted like Excel expects.
    Tsv,
    /// CSV using the document's own delimiter.
    CsvCurrent,
    /// CSV with explicit settings.
    CsvCustom {
        delimiter: String,
        quote_style: String,
        line_ending: String,
    },
    /// JSON array of `{header: value}` objects.
    JsonObjects,
    /// JSON array of arrays (first row = headers when included).
    JsonArrays,
    /// One JSON object per line.
    JsonLines,
    /// GitHub-flavoured Markdown table.
    Markdown,
    /// SQL `VALUES` rows; blanks become NULL.
    SqlValues,
}

/// Serialize `rows_abs` x `cols` of `doc` into `format`. `include_headers`
/// adds the header row where the format has a place for it.
pub fn serialize_selection(
    doc: &Document,
    rows_abs: &[usize],
    cols: &[usize],
    include_headers: bool,
    format: &CopyFormat,
) -> AppResult<String> {
    if cols.is_empty() {
        return Err(AppError::invalid("no columns are selected"));
    }
    for &c in cols {
        if c >= doc.n_cols() {
            return Err(AppError::invalid("column index out of range"));
        }
    }
    let headers: Vec<String> = cols
        .iter()
        .map(|&c| doc.headers().get(c).cloned().unwrap_or_default())
        .collect();

    match format {
        CopyFormat::Tsv => delimited(
            doc,
            rows_abs,
            cols,
            &headers,
            include_headers,
            b'\t',
            "necessary",
            "lf",
        ),
        CopyFormat::CsvCurrent => {
            let delim = doc.meta().delimiter;
            let byte = crate::util::delimiter_to_byte(&delim);
            delimited(
                doc,
                rows_abs,
                cols,
                &headers,
                include_headers,
                byte,
                "necessary",
                "lf",
            )
        }
        CopyFormat::CsvCustom {
            delimiter,
            quote_style,
            line_ending,
        } => {
            let byte = crate::util::delimiter_to_byte(delimiter);
            delimited(
                doc,
                rows_abs,
                cols,
                &headers,
                include_headers,
                byte,
                quote_style,
                line_ending,
            )
        }
        CopyFormat::JsonObjects => json_objects(doc, rows_abs, cols, &headers, false),
        CopyFormat::JsonLines => json_objects(doc, rows_abs, cols, &headers, true),
        CopyFormat::JsonArrays => json_arrays(doc, rows_abs, cols, &headers, include_headers),
        CopyFormat::Markdown => markdown(doc, rows_abs, cols, &headers),
        CopyFormat::SqlValues => sql_values(doc, rows_abs, cols),
    }
}

#[allow(clippy::too_many_arguments)] // internal serializer; each knob is a format setting
fn delimited(
    doc: &Document,
    rows_abs: &[usize],
    cols: &[usize],
    headers: &[String],
    include_headers: bool,
    delimiter: u8,
    quote_style: &str,
    line_ending: &str,
) -> AppResult<String> {
    use csv::{QuoteStyle, Terminator, WriterBuilder};
    let style = match quote_style {
        "always" => QuoteStyle::Always,
        "never" => QuoteStyle::Never,
        _ => QuoteStyle::Necessary,
    };
    let terminator = if line_ending.eq_ignore_ascii_case("crlf") {
        Terminator::CRLF
    } else {
        Terminator::Any(b'\n')
    };
    let mut writer = WriterBuilder::new()
        .delimiter(delimiter)
        .quote_style(style)
        .terminator(terminator)
        .from_writer(Vec::new());
    if include_headers {
        writer.write_record(headers.iter().map(String::as_bytes))?;
    }
    doc.visit_rows_at(rows_abs, &mut |_, row| {
        writer.write_record(cols.iter().map(|&c| row[c].as_bytes()))?;
        Ok(true)
    })?;
    let bytes = writer
        .into_inner()
        .map_err(|e| AppError::Other(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| AppError::Other(e.to_string()))
}

/// JSON string escaping via serde_json (quotes, backslashes, newlines,
/// control characters — all correct by construction).
fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn json_objects(
    doc: &Document,
    rows_abs: &[usize],
    cols: &[usize],
    headers: &[String],
    lines: bool,
) -> AppResult<String> {
    let mut out = String::new();
    if !lines {
        out.push_str("[\n");
    }
    let mut first = true;
    doc.visit_rows_at(rows_abs, &mut |_, row| {
        if !first {
            out.push_str(if lines { "\n" } else { ",\n" });
        }
        first = false;
        if !lines {
            out.push_str("  ");
        }
        out.push('{');
        for (i, &c) in cols.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&json_string(&headers[i]));
            out.push_str(": ");
            out.push_str(&json_string(&row[c]));
        }
        out.push('}');
        Ok(true)
    })?;
    if !lines {
        out.push_str("\n]");
    }
    Ok(out)
}

fn json_arrays(
    doc: &Document,
    rows_abs: &[usize],
    cols: &[usize],
    headers: &[String],
    include_headers: bool,
) -> AppResult<String> {
    let mut out = String::from("[\n");
    let mut first = true;
    let push_row = |cells: Vec<String>, out: &mut String, first: &mut bool| {
        if !*first {
            out.push_str(",\n");
        }
        *first = false;
        out.push_str("  [");
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(cell);
        }
        out.push(']');
    };
    if include_headers {
        let cells: Vec<String> = headers.iter().map(|h| json_string(h)).collect();
        push_row(cells, &mut out, &mut first);
    }
    doc.visit_rows_at(rows_abs, &mut |_, row| {
        let cells: Vec<String> = cols.iter().map(|&c| json_string(&row[c])).collect();
        push_row(cells, &mut out, &mut first);
        Ok(true)
    })?;
    out.push_str("\n]");
    Ok(out)
}

/// Markdown cell escaping: pipes escape, newlines become `<br>` so the table
/// structure survives.
fn markdown_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\r', '\n'], "<br>")
}

fn markdown(
    doc: &Document,
    rows_abs: &[usize],
    cols: &[usize],
    headers: &[String],
) -> AppResult<String> {
    let mut out = String::from("|");
    for header in headers {
        out.push_str(&format!(" {} |", markdown_cell(header)));
    }
    out.push_str("\n|");
    for _ in headers {
        out.push_str(" --- |");
    }
    doc.visit_rows_at(rows_abs, &mut |_, row| {
        out.push_str("\n|");
        for &c in cols {
            out.push_str(&format!(" {} |", markdown_cell(&row[c])));
        }
        Ok(true)
    })?;
    Ok(out)
}

/// ANSI SQL string literal: single quotes double; everything else (newlines,
/// backslashes) is preserved verbatim inside the quotes. Blank cells emit
/// NULL so numeric columns stay loadable.
fn sql_literal(value: &str) -> String {
    if value.is_empty() {
        "NULL".to_string()
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

fn sql_values(doc: &Document, rows_abs: &[usize], cols: &[usize]) -> AppResult<String> {
    let mut out = String::new();
    let mut first = true;
    doc.visit_rows_at(rows_abs, &mut |_, row| {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        out.push('(');
        for (i, &c) in cols.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&sql_literal(&row[c]));
        }
        out.push(')');
        Ok(true)
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn fixture() -> Document {
        doc("name,note,qty\nAda,\"line1\nline2\",3\nBob's,\"say \"\"hi\"\"\",\nC\\D,plain|pipe,7")
    }

    #[test]
    fn tsv_and_custom_csv_round_trip() {
        let d = fixture();
        let tsv = serialize_selection(&d, &[0, 1, 2], &[0, 1, 2], true, &CopyFormat::Tsv).unwrap();
        let reparsed = parse(
            tsv.as_bytes(),
            &ParseSettings {
                delimiter: Some(b'\t'),
                encoding: Some(encoding_rs::UTF_8),
            },
        )
        .unwrap();
        assert_eq!(reparsed.records.len(), 4);
        assert_eq!(reparsed.records[1][1], "line1\nline2");
        assert_eq!(reparsed.records[2][0], "Bob's");

        let custom = serialize_selection(
            &d,
            &[0],
            &[0, 2],
            false,
            &CopyFormat::CsvCustom {
                delimiter: ";".into(),
                quote_style: "always".into(),
                line_ending: "crlf".into(),
            },
        )
        .unwrap();
        assert_eq!(custom, "\"Ada\";\"3\"\r\n");
    }

    #[test]
    fn json_formats_escape_correctly() {
        let d = fixture();
        let objects =
            serialize_selection(&d, &[0, 1], &[0, 1], true, &CopyFormat::JsonObjects).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&objects).unwrap();
        assert_eq!(parsed[0]["note"], "line1\nline2");
        assert_eq!(parsed[1]["note"], "say \"hi\"");

        let arrays = serialize_selection(&d, &[2], &[0, 1], true, &CopyFormat::JsonArrays).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&arrays).unwrap();
        assert_eq!(parsed[0][0], "name"); // header row included
        assert_eq!(parsed[1][0], "C\\D"); // backslash survives

        let lines = serialize_selection(&d, &[0, 2], &[2], false, &CopyFormat::JsonLines).unwrap();
        let rows: Vec<&str> = lines.lines().collect();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert!(serde_json::from_str::<serde_json::Value>(row).is_ok());
        }
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines() {
        let d = fixture();
        let md = serialize_selection(&d, &[2], &[1], true, &CopyFormat::Markdown).unwrap();
        assert!(md.contains("plain\\|pipe"));
        let multiline = serialize_selection(&d, &[0], &[1], true, &CopyFormat::Markdown).unwrap();
        assert!(multiline.contains("line1<br>line2"));
        assert!(multiline.starts_with("| note |"));
    }

    #[test]
    fn sql_values_escape_quotes_and_emit_null_for_blank() {
        let d = fixture();
        let sql = serialize_selection(&d, &[1, 2], &[0, 2], false, &CopyFormat::SqlValues).unwrap();
        assert_eq!(sql, "('Bob''s', NULL),\n('C\\D', '7')");
    }

    #[test]
    fn selection_subsets_and_header_toggle_apply() {
        let d = fixture();
        let no_headers = serialize_selection(&d, &[0], &[2, 0], false, &CopyFormat::Tsv).unwrap();
        // Column order follows the request (qty before name).
        assert_eq!(no_headers, "3\tAda\n");
        assert!(serialize_selection(&d, &[0], &[], true, &CopyFormat::Tsv).is_err());
        assert!(serialize_selection(&d, &[0], &[9], true, &CopyFormat::Tsv).is_err());
    }
}
