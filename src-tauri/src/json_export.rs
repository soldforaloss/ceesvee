//! F33 JSON / JSON Lines export: serialize a document (or a scoped slice of
//! it) as an array of objects, an array of arrays, or JSON Lines, optionally
//! rebuilding nested objects from flattened path columns.
//!
//! Rules (the inverse of [`crate::json_import`], documented once, here):
//! - Output is ALWAYS UTF-8 with LF separators and no BOM; every successful
//!   export reparses as valid JSON (JSON Lines: per line).
//! - The per-export `missingToken` / `nullToken` cell texts are compared to
//!   the EXACT (untrimmed) cell text. A missing cell omits the property in
//!   the objects / JSON Lines formats. The arrays format has no property to
//!   omit, so a missing cell collapses to `null` there — positional rows
//!   cannot skip a slot; use the objects format when the missing-vs-null
//!   distinction must survive.
//! - With `typed` (the default), a column with a declared schema exports
//!   real JSON values: integer/decimal/float columns as numbers, boolean
//!   columns as booleans, json columns re-inflated (their key order
//!   preserved), and the schema's null tokens as `null`. Cells that do not
//!   parse fall back to the raw string, as do integers outside the i64/u64
//!   range and non-finite floats. Every other column stays untouched text.
//! - `rebuildNested` splits column names on unescaped `.` (via
//!   [`crate::json_import::split_path`], the exact inverse of the import's
//!   flattening) and nests values accordingly. Keys appear in scope column
//!   order; a rebuilt object opens where its first child column sits.
//! - Duplicate output paths — two columns mapping to the same path, or one
//!   column's value path doubling as another column's object prefix — are
//!   detected and rejected BEFORE anything is written.
//! - Every output streams through the atomic-save pipeline: failure or
//!   cancellation removes the staging file and never touches an existing
//!   destination.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use crate::document::Document;
use crate::dto::{ExportScope, JsonExportFormat, JsonExportOptions};
use crate::error::{AppError, AppResult};
use crate::export_scope::{self, ResolvedScope};
use crate::job::JobCtx;
use crate::json_import::{escape_key, split_path, JVal};
use crate::save;
use crate::schema::{self, CellState, ColumnSchema, LogicalType, TypedValue};

/// Progress/cancellation cadence (rows between [`JobCtx::advance`] calls).
const ADVANCE_EVERY: u64 = 512;

// ----- planning (everything that can fail before a byte is written) ---------------

/// A validated export: the resolved scope, the output column names, the
/// (possibly rebuilt) output path per column and the schema used for typed
/// emission. Producing one performs ALL up-front validation — option
/// consistency, scope resolution and duplicate output-path detection — so
/// commands can fail fast before spawning a job.
pub struct ExportPlan<'a> {
    resolved: ResolvedScope,
    names: Vec<String>,
    paths: Vec<Vec<String>>,
    schemas: Vec<Option<&'a ColumnSchema>>,
}

/// Validate `options` + `scope` against `doc` and build the emit plan.
/// Nothing is written; every rejected export fails here, before any I/O.
pub fn plan<'a>(
    doc: &'a Document,
    options: &JsonExportOptions,
    scope: &ExportScope,
) -> AppResult<ExportPlan<'a>> {
    if let (Some(null), Some(missing)) = (&options.null_token, &options.missing_token) {
        if null == missing {
            return Err(AppError::invalid(format!(
                "the null token and the missing-value text are both {null:?}; they must differ \
                 so explicit nulls stay distinguishable from missing fields"
            )));
        }
    }
    if options.rebuild_nested && options.format == JsonExportFormat::Arrays {
        return Err(AppError::invalid(
            "rebuildNested does not apply to the arrays format — positional rows have no keys \
             to nest under",
        ));
    }

    let resolved = export_scope::resolve_scope(doc, scope)?;
    let headers = doc.headers();
    let names: Vec<String> = resolved.cols.iter().map(|&c| headers[c].clone()).collect();
    let paths: Vec<Vec<String>> = if options.rebuild_nested {
        names.iter().map(|name| split_path(name)).collect()
    } else {
        names.iter().map(|name| vec![name.clone()]).collect()
    };
    // The positional arrays format has no keys, so paths cannot collide.
    if options.format != JsonExportFormat::Arrays {
        check_output_paths(&names, &paths)?;
    }
    let schemas: Vec<Option<&ColumnSchema>> = resolved
        .cols
        .iter()
        .map(|&c| {
            if options.typed {
                doc.column_schema_at(c)
            } else {
                None
            }
        })
        .collect();

    Ok(ExportPlan {
        resolved,
        names,
        paths,
        schemas,
    })
}

/// Display form of an output path (segments re-escaped, joined with `.`).
fn dotted(path: &[String]) -> String {
    path.iter()
        .map(|segment| escape_key(segment))
        .collect::<Vec<_>>()
        .join(".")
}

/// Reject duplicate output paths and leaf-vs-object conflicts. Both checks
/// run over the FULL column set before anything is written.
fn check_output_paths(names: &[String], paths: &[Vec<String>]) -> AppResult<()> {
    let mut seen: HashMap<&[String], &str> = HashMap::new();
    for (name, path) in names.iter().zip(paths) {
        if let Some(first) = seen.insert(path.as_slice(), name) {
            return Err(AppError::invalid(format!(
                "duplicate JSON output path {:?}: columns {first:?} and {name:?} both write it — \
                 rename one of them",
                dotted(path)
            )));
        }
    }
    // Every PROPER prefix of every path names an object the rebuild will
    // create; a full path landing on one would be a value and an object at
    // the same key. Checking full paths against the prefix set catches both
    // directions ("a" vs "a.b" and "a.b" vs "a").
    let mut prefixes: HashMap<&[String], &str> = HashMap::new();
    for (name, path) in names.iter().zip(paths) {
        for len in 1..path.len() {
            prefixes.entry(&path[..len]).or_insert(name.as_str());
        }
    }
    for (name, path) in names.iter().zip(paths) {
        if let Some(other) = prefixes.get(path.as_slice()) {
            return Err(AppError::invalid(format!(
                "conflicting JSON output paths: column {name:?} writes a value at {:?}, but \
                 column {other:?} nests an object under it — rename one of them",
                dotted(path)
            )));
        }
    }
    Ok(())
}

// ----- cell mapping ----------------------------------------------------------------

/// What one cell contributes to its record.
enum Cell {
    /// Omit the property (objects / JSON Lines); `null` in the arrays format.
    Missing,
    Value(JVal),
}

fn cell_value(cell: &str, schema: Option<&ColumnSchema>, options: &JsonExportOptions) -> Cell {
    if options.missing_token.as_deref() == Some(cell) {
        return Cell::Missing;
    }
    if options.null_token.as_deref() == Some(cell) {
        return Cell::Value(JVal::Null);
    }
    match schema {
        Some(schema) => Cell::Value(typed_value(cell, schema)),
        None => Cell::Value(JVal::Str(cell.to_string())),
    }
}

/// Typed emission for a column with a declared schema (module docs). Never
/// fails: anything unparseable falls back to the raw cell string.
fn typed_value(cell: &str, schema: &ColumnSchema) -> JVal {
    if schema::is_null_token(schema, cell) {
        return JVal::Null;
    }
    match schema.logical_type {
        // Parsed straight to the order-preserving value so object key order
        // inside preserved-JSON cells survives the round-trip.
        LogicalType::Json => {
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                return JVal::Str(cell.to_string());
            }
            serde_json::from_str::<JVal>(trimmed).unwrap_or_else(|_| JVal::Str(cell.to_string()))
        }
        LogicalType::Integer | LogicalType::Decimal | LogicalType::Float => {
            match schema::classify(Some(cell), schema) {
                CellState::Valid(TypedValue::Integer(v)) => int_value(v, cell),
                CellState::Valid(TypedValue::Decimal(d)) => {
                    decimal_value(&d.to_plain_string(), cell)
                }
                CellState::Valid(TypedValue::Float(f)) => float_value(f, cell),
                _ => JVal::Str(cell.to_string()),
            }
        }
        LogicalType::Boolean => match schema::classify(Some(cell), schema) {
            CellState::Valid(TypedValue::Boolean(b)) => JVal::Bool(b),
            _ => JVal::Str(cell.to_string()),
        },
        _ => JVal::Str(cell.to_string()),
    }
}

fn int_value(v: i128, raw: &str) -> JVal {
    if let Ok(i) = i64::try_from(v) {
        JVal::Num(serde_json::Number::from(i))
    } else if let Ok(u) = u64::try_from(v) {
        JVal::Num(serde_json::Number::from(u))
    } else {
        // Beyond what a JSON number can carry losslessly here.
        JVal::Str(raw.to_string())
    }
}

fn decimal_value(plain: &str, raw: &str) -> JVal {
    if let Ok(i) = plain.parse::<i64>() {
        return JVal::Num(serde_json::Number::from(i));
    }
    match plain
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
    {
        Some(n) => JVal::Num(n),
        None => JVal::Str(raw.to_string()),
    }
}

fn float_value(f: f64, raw: &str) -> JVal {
    match serde_json::Number::from_f64(f) {
        Some(n) => JVal::Num(n),
        None => JVal::Str(raw.to_string()),
    }
}

// ----- record building -------------------------------------------------------------

/// Build the JSON element for one row (an object or a positional array).
fn build_element(
    row: &[String],
    plan: &ExportPlan<'_>,
    options: &JsonExportOptions,
) -> AppResult<JVal> {
    match options.format {
        JsonExportFormat::Arrays => {
            let mut cells = Vec::with_capacity(plan.resolved.cols.len());
            for (i, &c) in plan.resolved.cols.iter().enumerate() {
                cells.push(match cell_value(&row[c], plan.schemas[i], options) {
                    Cell::Missing => JVal::Null,
                    Cell::Value(v) => v,
                });
            }
            Ok(JVal::Arr(cells))
        }
        JsonExportFormat::Objects | JsonExportFormat::JsonLines => {
            let mut root: Vec<(String, JVal)> = Vec::new();
            for (i, &c) in plan.resolved.cols.iter().enumerate() {
                match cell_value(&row[c], plan.schemas[i], options) {
                    Cell::Missing => {}
                    Cell::Value(v) => insert_path(&mut root, &plan.paths[i], v)?,
                }
            }
            Ok(JVal::Obj(root))
        }
    }
}

/// Insert `value` at `path`, creating intermediate objects in first-child
/// order. Conflicts were rejected at plan time; the error arm is defensive.
fn insert_path(obj: &mut Vec<(String, JVal)>, path: &[String], value: JVal) -> AppResult<()> {
    let Some((key, rest)) = path.split_first() else {
        return Err(AppError::Other(
            "internal JSON export error: empty output path".into(),
        ));
    };
    if rest.is_empty() {
        obj.push((key.clone(), value));
        return Ok(());
    }
    let pos = match obj.iter().position(|(k, _)| k == key) {
        Some(pos) => pos,
        None => {
            obj.push((key.clone(), JVal::Obj(Vec::new())));
            obj.len() - 1
        }
    };
    match &mut obj[pos].1 {
        JVal::Obj(entries) => insert_path(entries, rest, value),
        _ => Err(AppError::Other(format!(
            "internal JSON export error: {key:?} is both a value and an object"
        ))),
    }
}

// ----- writing ---------------------------------------------------------------------

/// Byte-counting writer that also feeds the job's bytes-written progress.
struct Sink<'a, W: Write> {
    out: W,
    bytes: u64,
    ctx: &'a JobCtx,
}

impl<W: Write> Sink<'_, W> {
    fn write(&mut self, chunk: &[u8]) -> AppResult<()> {
        self.out.write_all(chunk)?;
        self.bytes += chunk.len() as u64;
        self.ctx.add_bytes(chunk.len() as u64);
        Ok(())
    }

    fn finish(mut self) -> AppResult<u64> {
        self.out.flush()?;
        Ok(self.bytes)
    }
}

/// Serialize one element into the reusable buffer.
fn to_json(buf: &mut Vec<u8>, element: &JVal) -> AppResult<()> {
    buf.clear();
    serde_json::to_writer(&mut *buf, element)
        .map_err(|e| AppError::Other(format!("JSON serialization failed: {e}")))
}

/// Run a validated, revision-guarded, cancellable JSON export through the
/// atomic-save pipeline. Returns the bytes written. Any failure — stale
/// revision, path conflict, I/O, cancellation — leaves the destination
/// byte-for-byte untouched and removes the staging file.
pub fn run(
    doc: &Document,
    dest: &Path,
    options: &JsonExportOptions,
    scope: &ExportScope,
    expected_revision: u64,
    ctx: &JobCtx,
) -> AppResult<u64> {
    doc.check_revision(expected_revision)?;
    let plan = plan(doc, options, scope)?;
    ctx.set_total(plan.resolved.rows.len() as u64);

    let bytes = save::atomic_write(dest, options.backup, |file| {
        let mut sink = Sink {
            out: std::io::BufWriter::new(file),
            bytes: 0,
            ctx,
        };
        match options.format {
            JsonExportFormat::JsonLines => write_json_lines(doc, &plan, options, &mut sink)?,
            JsonExportFormat::Objects | JsonExportFormat::Arrays => {
                write_array_document(doc, &plan, options, &mut sink)?
            }
        }
        sink.finish()
    })?;
    ctx.flush_progress();
    Ok(bytes)
}

/// `[` … one element per line … `]` (objects and arrays formats).
fn write_array_document<W: Write>(
    doc: &Document,
    plan: &ExportPlan<'_>,
    options: &JsonExportOptions,
    sink: &mut Sink<'_, W>,
) -> AppResult<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut any = false;
    sink.write(b"[")?;

    if options.format == JsonExportFormat::Arrays && options.include_headers && doc.has_header_row()
    {
        let header = JVal::Arr(plan.names.iter().map(|n| JVal::Str(n.clone())).collect());
        to_json(&mut buf, &header)?;
        sink.write(if any { b",\n  " } else { b"\n  " })?;
        any = true;
        sink.write(&buf)?;
    }

    let mut pending = 0u64;
    doc.visit_rows_at(&plan.resolved.rows, &mut |_, row| {
        let element = build_element(row, plan, options)?;
        to_json(&mut buf, &element)?;
        sink.write(if any { b",\n  " } else { b"\n  " })?;
        any = true;
        sink.write(&buf)?;
        pending += 1;
        if pending >= ADVANCE_EVERY {
            sink.ctx.advance(pending)?;
            pending = 0;
        }
        Ok(true)
    })?;
    sink.ctx.advance(pending)?;

    if any {
        sink.write(b"\n]\n")
    } else {
        sink.write(b"]\n")
    }
}

/// One compact object per LF-terminated line (JSON Lines / NDJSON).
fn write_json_lines<W: Write>(
    doc: &Document,
    plan: &ExportPlan<'_>,
    options: &JsonExportOptions,
    sink: &mut Sink<'_, W>,
) -> AppResult<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut pending = 0u64;
    doc.visit_rows_at(&plan.resolved.rows, &mut |_, row| {
        let element = build_element(row, plan, options)?;
        to_json(&mut buf, &element)?;
        sink.write(&buf)?;
        sink.write(b"\n")?;
        pending += 1;
        if pending >= ADVANCE_EVERY {
            sink.ctx.advance(pending)?;
            pending = 0;
        }
        Ok(true)
    })?;
    sink.ctx.advance(pending)
}

// ----- tests -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::json_import::{self, JsonImportOptions};
    use crate::parse::{parse, ParseSettings};
    use serde_json::{json, Value};

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    fn options(format: JsonExportFormat) -> JsonExportOptions {
        JsonExportOptions {
            format,
            ..JsonExportOptions::default()
        }
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("export", Some(1), |_| {});
        (registry, ctx)
    }

    fn export_to_string(
        doc: &Document,
        options: &JsonExportOptions,
        scope: &ExportScope,
    ) -> AppResult<String> {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.json");
        let (_r, ctx) = ctx();
        run(doc, &dest, options, scope, doc.revision(), &ctx)?;
        Ok(std::fs::read_to_string(&dest).unwrap())
    }

    /// Import JSON text through the real F33 import engine (typed schemas,
    /// null tokens and flattening attached exactly as production does).
    fn import_json(contents: &str) -> (tempfile::TempDir, Document) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.json");
        std::fs::write(&path, contents.as_bytes()).unwrap();
        let cache = dir.path().join("cache");
        let doc =
            json_import::import(&path, &JsonImportOptions::default(), &cache, 1, None).unwrap();
        (dir, doc)
    }

    #[test]
    fn objects_export_is_valid_utf8_and_reparses() {
        let d = doc_from("name,note\nAda,\"x,\ny\"\nBob,käse ⚡", true);
        let text =
            export_to_string(&d, &options(JsonExportFormat::Objects), &ExportScope::All).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!([
                {"name": "Ada", "note": "x,\ny"},
                {"name": "Bob", "note": "käse ⚡"}
            ])
        );
    }

    #[test]
    fn null_and_missing_tokens_map_to_null_and_omitted() {
        let d = doc_from("a,b\n1,null\n2,", true);
        let text =
            export_to_string(&d, &options(JsonExportFormat::Objects), &ExportScope::All).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed[0]["b"],
            Value::Null,
            "the null token becomes JSON null"
        );
        assert!(
            parsed[1].as_object().unwrap().get("b").is_none(),
            "the missing token omits the property entirely"
        );

        // The positional arrays format cannot omit a slot; missing collapses
        // to null there (module docs).
        let mut opts = options(JsonExportFormat::Arrays);
        opts.include_headers = false;
        let text = export_to_string(&d, &opts, &ExportScope::All).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, json!([["1", null], ["2", null]]));
    }

    #[test]
    fn import_export_round_trip_distinguishes_missing_from_null() {
        let (_dir, doc) = import_json(r#"[{"a":1,"b":null},{"a":2}]"#);
        let text =
            export_to_string(&doc, &options(JsonExportFormat::Objects), &ExportScope::All).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed[0],
            json!({"a": 1, "b": null}),
            "explicit null survives"
        );
        assert_eq!(
            parsed[1],
            json!({"a": 2}),
            "a missing field re-exports as missing, not as null"
        );
    }

    #[test]
    fn flatten_then_rebuild_round_trips_nested_objects() {
        let source = r#"[
            {"id":1,"user":{"name":"Ada","meta":{"active":true}},"tags":[1,2]},
            {"id":2,"user":{"name":"Bob","meta":{"active":false}},"tags":[]}
        ]"#;
        let (_dir, doc) = import_json(source);
        let mut opts = options(JsonExportFormat::Objects);
        opts.rebuild_nested = true;
        let text = export_to_string(&doc, &opts, &ExportScope::All).unwrap();
        let exported: Value = serde_json::from_str(&text).unwrap();
        let original: Value = serde_json::from_str(source).unwrap();
        assert_eq!(
            exported, original,
            "flatten → rebuild is semantically lossless"
        );
    }

    #[test]
    fn escaped_dot_columns_stay_distinct_from_nested_paths() {
        // A literal "a.b" key flattens to the escaped column "a\.b", which
        // must neither collide with nor nest into the real a→b path.
        let source = r#"[{"a.b":1,"a":{"b":2}}]"#;
        let (_dir, doc) = import_json(source);
        let mut opts = options(JsonExportFormat::Objects);
        opts.rebuild_nested = true;
        let text = export_to_string(&doc, &opts, &ExportScope::All).unwrap();
        let exported: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(exported, serde_json::from_str::<Value>(source).unwrap());
    }

    #[test]
    fn json_lines_export_writes_one_valid_object_per_line() {
        let d = doc_from("a,b\n1,x\n2,\n3,z", true);
        let text =
            export_to_string(&d, &options(JsonExportFormat::JsonLines), &ExportScope::All).unwrap();
        assert!(text.ends_with('\n'), "every record line is LF-terminated");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let _: Value = serde_json::from_str(line).expect("each line reparses");
        }
        let row2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            row2,
            json!({"a": "2"}),
            "missing fields are omitted per line"
        );
    }

    #[test]
    fn arrays_format_writes_header_first_and_respects_schema_types() {
        let mut d = doc_from("n,t\n7,x\nabc,y", true);
        let id = d.column_ids()[0].clone();
        d.set_column_schema(ColumnSchema::new(id, "n", LogicalType::Integer));
        let text =
            export_to_string(&d, &options(JsonExportFormat::Arrays), &ExportScope::All).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!([["n", "t"], [7, "x"], ["abc", "y"]]),
            "header first; parseable integers become numbers; invalid cells stay strings"
        );
    }

    #[test]
    fn scoped_exports_respect_filter_and_column_order() {
        let mut d = doc_from("a,b,c\n1,2,3\n4,5,6\n7,8,9", true);
        d.set_filter(vec![0, 2]).unwrap();
        let text = export_to_string(
            &d,
            &options(JsonExportFormat::Objects),
            &ExportScope::VisibleRows,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!([
                {"a": "1", "b": "2", "c": "3"},
                {"a": "7", "b": "8", "c": "9"}
            ])
        );

        let scope = ExportScope::SelectedColumns {
            columns: vec![2, 0],
        };
        let text = export_to_string(&d, &options(JsonExportFormat::Arrays), &scope).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!([["c", "a"], ["3", "1"], ["9", "7"]]),
            "selected-column order is preserved and the filter still applies"
        );
    }

    #[test]
    fn duplicate_output_paths_are_rejected_before_writing() {
        let d = doc_from("a,a\n1,2", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.json");
        let (_r, ctx) = ctx();
        let err = run(
            &d,
            &dest,
            &options(JsonExportFormat::Objects),
            &ExportScope::All,
            d.revision(),
            &ctx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
        assert!(!dest.exists(), "nothing may be written");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no temp litter"
        );

        // Selecting the same column twice collides the same way…
        let d2 = doc_from("a,b\n1,2", true);
        let scope = ExportScope::SelectedColumns {
            columns: vec![0, 0],
        };
        let err = run(
            &d2,
            &dest,
            &options(JsonExportFormat::JsonLines),
            &scope,
            d2.revision(),
            &ctx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
        // …but the keyless positional arrays format allows it.
        let mut opts = options(JsonExportFormat::Arrays);
        opts.include_headers = false;
        let text = export_to_string(&d2, &opts, &scope).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!([["1", "1"]])
        );
    }

    #[test]
    fn nested_prefix_conflicts_are_rejected_before_writing() {
        // "a" is a value while "a.b" needs an object at "a".
        let d = doc_from("a,a.b\n1,2", true);
        let mut opts = options(JsonExportFormat::Objects);
        opts.rebuild_nested = true;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.json");
        let (_r, ctx) = ctx();
        let err = run(&d, &dest, &opts, &ExportScope::All, d.revision(), &ctx).unwrap_err();
        assert!(err.to_string().contains("conflicting"), "{err}");
        assert!(!dest.exists());

        // Two columns rebuilding to the identical path are duplicates.
        let d2 = doc_from("x.y,x.y\n1,2", true);
        let err = run(&d2, &dest, &opts, &ExportScope::All, d2.revision(), &ctx).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
        assert!(!dest.exists());

        // An ESCAPED dot is a literal key — no conflict with the real path.
        let d3 = doc_from("a\\.b,a.b\n1,2", true);
        let text = export_to_string(&d3, &opts, &ExportScope::All).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!([{"a.b": "1", "a": {"b": "2"}}])
        );
    }

    #[test]
    fn cancelled_export_cleans_up_and_leaves_no_file() {
        let d = doc_from("a\n1\n2\n3", true);
        let registry = JobRegistry::default();
        let ctx = registry.begin("export", Some(1), |_| {});
        registry.cancel(ctx.id);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cancel.json");
        let result = run(
            &d,
            &dest,
            &options(JsonExportFormat::Objects),
            &ExportScope::All,
            d.revision(),
            &ctx,
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(
            !dest.exists(),
            "a cancelled export must not create the file"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no temp litter"
        );
    }

    #[test]
    fn stale_revision_is_rejected_before_writing() {
        let mut d = doc_from("a\n1", true);
        let stale = d.revision();
        d.set_cell(0, 0, "changed".into()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.json");
        let (_r, ctx) = ctx();
        let err = run(
            &d,
            &dest,
            &options(JsonExportFormat::Objects),
            &ExportScope::All,
            stale,
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::StaleRevision { .. }), "{err}");
        assert!(!dest.exists());
    }

    #[test]
    fn invalid_option_combinations_are_rejected() {
        let d = doc_from("a\n1", true);
        let mut opts = options(JsonExportFormat::Objects);
        opts.null_token = Some("x".into());
        opts.missing_token = Some("x".into());
        let err = export_to_string(&d, &opts, &ExportScope::All).unwrap_err();
        assert!(err.to_string().contains("must differ"), "{err}");

        let mut opts = options(JsonExportFormat::Arrays);
        opts.rebuild_nested = true;
        let err = export_to_string(&d, &opts, &ExportScope::All).unwrap_err();
        assert!(err.to_string().contains("arrays format"), "{err}");
    }

    #[test]
    fn typed_false_exports_everything_as_strings() {
        let mut d = doc_from("n\n7", true);
        let id = d.column_ids()[0].clone();
        d.set_column_schema(ColumnSchema::new(id, "n", LogicalType::Integer));
        let mut opts = options(JsonExportFormat::Objects);
        opts.typed = false;
        let text = export_to_string(&d, &opts, &ExportScope::All).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!([{"n": "7"}])
        );
    }

    #[test]
    fn schema_null_tokens_export_as_null_when_typed() {
        let mut d = doc_from("n\nN/A\n8", true);
        let id = d.column_ids()[0].clone();
        let mut schema = ColumnSchema::new(id, "n", LogicalType::Integer);
        schema.null_tokens = vec!["N/A".into()];
        d.set_column_schema(schema);
        let text =
            export_to_string(&d, &options(JsonExportFormat::Objects), &ExportScope::All).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!([{"n": null}, {"n": 8}])
        );
    }

    #[test]
    fn empty_scope_exports_an_empty_array() {
        let mut d = doc_from("a\n1", true);
        d.set_filter(vec![]).unwrap();
        let text = export_to_string(
            &d,
            &options(JsonExportFormat::Objects),
            &ExportScope::VisibleRows,
        )
        .unwrap();
        assert_eq!(text, "[]\n");
    }
}
