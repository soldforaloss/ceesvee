//! Parquet & Arrow interop (F32) — read side.
//!
//! Opens typed columnar datasets — Apache Parquet, Arrow IPC files and Arrow
//! IPC streams (Feather v2 IS the Arrow IPC file format; UI copy should say
//! so) — preserving types and nulls:
//!
//! - **Inspection** ([`inspect`]): row count, columns mapped to F31
//!   [`LogicalType`]s, row-group/batch count, compression codecs, nested
//!   fields, and a rough estimate of what the fully editable in-memory
//!   document would cost.
//! - **Indexed read-only backing** ([`open_indexed`] → [`ColumnarHandle`],
//!   wired into [`crate::document::Backing::Columnar`]): windowed reads over
//!   parquet row groups / Arrow record batches with a bounded LRU of decoded
//!   text blocks, so the grid, filters and export all work through the same
//!   `visit_rows` machinery as the F10 CSV index.
//! - **Typed-value → text conversion**: the text plane is CANONICAL under the
//!   generated F31 column schemas, so classified cells round-trip exactly —
//!   integers (i64/u64 well beyond JS number range) as plain decimal strings,
//!   decimals rendered exactly from mantissa+scale (`1.50` keeps its scale),
//!   floats via Rust's shortest round-trip display, booleans as
//!   `true`/`false`, dates as `%Y-%m-%d`, naive timestamps as ISO wall time
//!   (with an `inputFormats` pattern carrying fractional seconds), zoned
//!   timestamps as the UTC instant (`...Z`) with the original zone kept in
//!   [`ColumnSchema::time_zone`], and binary as lowercase hex.
//! - **Null vs empty string**: a columnar NULL is `None` in the
//!   [`TabularSource`] contract; an empty string is `Some("")`. The
//!   read-only text plane (`visit`/`visit_at`, what the grid shows) renders
//!   NULL as an empty cell; the `Option` plane keeps the distinction for
//!   export and conversion. [`plan_editable`] / [`open_editable_rows`]
//!   preserve it in editable documents by assigning each null-containing
//!   column a collision-free null token (`NULL`, escalating to `NULL#1`, …,
//!   never colliding with the column's actual trimmed values) recorded in the
//!   column's schema, so `is_null_token` recovers the null bit later.
//! - **Nested data**: structs are ALWAYS flattened to path-based names
//!   (segments escaped by [`crate::json_import::escape_key`], joined with
//!   `.`); lists and maps follow an explicit [`ComplexPolicy`] — preserve as
//!   JSON text (deterministic; note serde_json object keys serialize sorted),
//!   reject (drop) the field, or explode a SINGLE list column into rows
//!   (editable open only — an indexed backing cannot re-number rows).
//!   Null and empty lists both yield one row with a null cell under explode;
//!   under preserve-as-JSON they stay distinct (`None` vs `[]`).
//! - **Row-group statistics pruning** ([`ColumnarHandle::filter_scan_ranges`],
//!   used by [`crate::filter::matching_rows`]): equality and range conditions
//!   on numeric/date/datetime columns skip parquet row groups whose min/max
//!   statistics prove no row can match. Pruning is CONSERVATIVE: it only ever
//!   skips a group when the typed bounds make a match impossible under the
//!   column's CURRENT schema (which must still agree with the open-time
//!   schema on every parse-relevant field), so filtered and unfiltered reads
//!   return identical values for matching rows. Everything else falls back to
//!   the full scan.
//!
//! Deliberate deferrals (documented so later stages don't guess): write-side
//! export (backend part 2); stats pruning for unsigned 32/64-bit columns
//! (parquet stores them sign-reinterpreted, the old-format ordering is
//! unreliable) and Decimal256; exploding maps or more than one list column;
//! and JSON-policy object key order (serde_json sorts keys).
//!
//! Cancellation: open/inspect/convert observe [`JobCtx`] cooperatively. The
//! read side creates NO on-disk caches (the block LRU is in memory), so a
//! cancelled open or convert leaves nothing behind by construction.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use arrow::array::{
    Array, ArrayRef, BinaryArray, BinaryViewArray, BooleanArray, Date32Array, Date64Array,
    Decimal128Array, Decimal256Array, FixedSizeBinaryArray, FixedSizeListArray, Float16Array,
    Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array, LargeBinaryArray,
    LargeListArray, LargeStringArray, ListArray, MapArray, StringArray, StringViewArray,
    StructArray, Time32MillisecondArray, Time32SecondArray, Time64MicrosecondArray,
    Time64NanosecondArray, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array,
    UInt8Array,
};
use arrow::datatypes::{DataType, Schema as ArrowSchema, TimeUnit};
use arrow::ipc::reader::{FileReader as IpcFileReader, StreamReader as IpcStreamReader};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, NaiveDateTime, NaiveTime};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::metadata::ParquetMetaData;
use parquet::file::statistics::Statistics;
use serde::{Deserialize, Serialize};

use crate::dto::{
    Conjunction, FileFingerprint, FilterCondition, FilterGroup, FilterNode, FilterOp,
};
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::json_import::escape_key;
use crate::schema::{
    compare_typed, parse_typed, ColumnSchema, DecimalValue, LogicalType, TypedValue,
};
use crate::tabular::{ContentFingerprint, RowCountHint, TabularColumn, TabularRow, TabularSource};
use crate::{index, util};

/// Rows per decoded text block (matches the F10 index visit block, so one
/// grid window is at most two block decodes).
const BLOCK_ROWS: usize = 4096;

/// Default in-memory budget for the decoded-block LRU. Bounded regardless of
/// file size; a single oversized block is always retained (the cache never
/// thrashes itself below one block).
const DEFAULT_CACHE_BUDGET: usize = 32 * 1024 * 1024;

/// Per-cell / per-row overhead constants shared with the open-time memory
/// estimate (mirrors [`index::estimate`]'s deliberately rough model).
const CELL_OVERHEAD: u64 = 40;
const ROW_OVERHEAD: u64 = 32;

fn arrow_err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("columnar read error: {e}"))
}

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

/// The three supported columnar container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnarFormat {
    Parquet,
    /// Arrow IPC file (a.k.a. Feather v2 — same container, keep the alias in
    /// UI copy).
    ArrowFile,
    ArrowStream,
}

impl ColumnarFormat {
    pub fn wire_name(self) -> &'static str {
        match self {
            ColumnarFormat::Parquet => "parquet",
            ColumnarFormat::ArrowFile => "arrowFile",
            ColumnarFormat::ArrowStream => "arrowStream",
        }
    }
}

/// Sniff the container format: `PAR1` / `ARROW1` magic first, then an Arrow
/// IPC stream probe (streams have no magic; validity is the probe).
pub fn detect_format(path: &Path) -> AppResult<ColumnarFormat> {
    let mut head = [0u8; 8];
    let n = {
        let mut file = File::open(path)?;
        let mut read = 0usize;
        loop {
            let got = file.read(&mut head[read..])?;
            if got == 0 {
                break;
            }
            read += got;
            if read == head.len() {
                break;
            }
        }
        read
    };
    if n >= 4 && &head[..4] == b"PAR1" {
        return Ok(ColumnarFormat::Parquet);
    }
    if n >= 6 && &head[..6] == b"ARROW1" {
        return Ok(ColumnarFormat::ArrowFile);
    }
    match IpcStreamReader::try_new(BufReader::new(File::open(path)?), None) {
        Ok(_) => Ok(ColumnarFormat::ArrowStream),
        Err(_) => Err(AppError::invalid(
            "not a recognised columnar file (expected Parquet, an Arrow IPC file, or an Arrow IPC stream)",
        )),
    }
}

// ---------------------------------------------------------------------------
// Open options / nested-field policies
// ---------------------------------------------------------------------------

/// What to do with a field the text contract cannot carry directly (list,
/// map, and other non-flattenable nested types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComplexPolicy {
    /// Keep the field as one column of canonical JSON text (F31 `json` type).
    #[default]
    PreserveJson,
    /// Multiply the record into one row per list element (editable open
    /// only, one list column per open).
    Explode,
    /// Drop the field from the projected schema.
    Reject,
}

/// Options for [`inspect`] / [`open_indexed`] / [`open_editable_rows`]
/// (wire DTO, camelCase).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColumnarOpenOptions {
    /// Default policy for complex fields.
    pub complex_policy: ComplexPolicy,
    /// Per-field overrides, keyed by the flattened path-based column name.
    pub field_policies: BTreeMap<String, ComplexPolicy>,
    /// Override for the decoded-block LRU budget (bytes). `0` = default.
    pub cache_budget_bytes: usize,
}

// ---------------------------------------------------------------------------
// Projection: arrow schema -> output columns
// ---------------------------------------------------------------------------

/// How one output column reads its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutKind {
    /// A primitive leaf, rendered to canonical text.
    Primitive,
    /// A complex field kept as canonical JSON text.
    Json,
    /// A list column exploded into rows (editable open only).
    Explode,
}

/// One projected output column.
#[derive(Debug, Clone)]
struct OutCol {
    /// Flattened path-based name (struct segments escaped and dot-joined).
    name: String,
    kind: OutKind,
    /// Descent from the record batch: `steps[0]` is the top-level column,
    /// the rest are struct child indices.
    steps: Vec<usize>,
    /// The leaf's dictionary-resolved data type (rendering target).
    data_type: DataType,
    logical: LogicalType,
    time_zone: Option<String>,
    input_formats: Option<Vec<String>>,
    nullable: bool,
    /// Index of the column's FIRST parquet leaf, for row-group statistics.
    leaf_start: usize,
}

struct Projection {
    cols: Vec<OutCol>,
    /// Whether the parquet-leaf accounting is trustworthy (no exotic types
    /// encountered that could desynchronise leaf indices).
    stats_ok: bool,
    /// Total parquet leaves consumed by the FULL arrow schema (including
    /// rejected fields), for cross-checking against file metadata.
    total_leaves: usize,
}

/// Whether `dt` is text-representable as a single canonical cell.
fn is_primitive(dt: &DataType) -> bool {
    match dt {
        DataType::Null
        | DataType::Boolean
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float16
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _)
        | DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::FixedSizeBinary(_)
        | DataType::Date32
        | DataType::Date64
        | DataType::Time32(_)
        | DataType::Time64(_)
        | DataType::Timestamp(_, _)
        | DataType::Duration(_)
        | DataType::Interval(_) => true,
        DataType::Dictionary(_, value) => is_primitive(value),
        _ => false,
    }
}

/// Strip dictionary wrappers to the rendering target type.
fn resolved_type(dt: &DataType) -> DataType {
    match dt {
        DataType::Dictionary(_, value) => resolved_type(value),
        other => other.clone(),
    }
}

/// F31 logical type + schema metadata for a primitive leaf type.
fn logical_of(dt: &DataType) -> (LogicalType, Option<String>, Option<Vec<String>>) {
    match dt {
        DataType::Boolean => (LogicalType::Boolean, None, None),
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => (LogicalType::Integer, None, None),
        DataType::Float16 | DataType::Float32 | DataType::Float64 => {
            (LogicalType::Float, None, None)
        }
        DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => {
            (LogicalType::Decimal, None, None)
        }
        DataType::Date32 | DataType::Date64 => (LogicalType::Date, None, None),
        DataType::Timestamp(_, tz) => match tz {
            // Zoned: rendered as the UTC instant (RFC 3339 `...Z`, covered by
            // the built-in parse fallback); the original zone is metadata.
            Some(tz) => (LogicalType::Datetime, Some(tz.to_string()), None),
            // Naive wall time: the custom pattern accepts an optional
            // fractional part, so sub-second units round-trip.
            None => (
                LogicalType::Datetime,
                None,
                Some(vec!["%Y-%m-%dT%H:%M:%S%.f".to_string()]),
            ),
        },
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => {
            (LogicalType::Text, None, None)
        }
        // Times, durations, intervals, binary (hex) and Null all land as text.
        _ => (LogicalType::Text, None, None),
    }
}

/// Number of parquet leaf columns a field of this type occupies (depth-first,
/// matching the parquet schema layout arrow writers produce).
fn leaf_count(dt: &DataType) -> usize {
    match dt {
        DataType::Struct(fields) => fields.iter().map(|f| leaf_count(f.data_type())).sum(),
        DataType::List(f) | DataType::LargeList(f) | DataType::FixedSizeList(f, _) => {
            leaf_count(f.data_type())
        }
        DataType::Map(entries, _) => leaf_count(entries.data_type()),
        DataType::Dictionary(_, value) => leaf_count(value),
        DataType::RunEndEncoded(_, values) => leaf_count(values.data_type()),
        DataType::Union(fields, _) => fields.iter().map(|(_, f)| leaf_count(f.data_type())).sum(),
        _ => 1,
    }
}

/// Types whose presence anywhere in the schema makes the depth-first leaf
/// accounting unreliable; statistics pruning is disabled wholesale then.
fn leaf_accounting_fragile(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Null | DataType::Union(_, _) | DataType::RunEndEncoded(_, _)
    )
}

struct ProjectionBuilder<'a> {
    options: &'a ColumnarOpenOptions,
    allow_explode: bool,
    cols: Vec<OutCol>,
    stats_ok: bool,
    leaf_cursor: usize,
    explode_seen: bool,
}

impl ProjectionBuilder<'_> {
    fn policy_for(&self, path: &str) -> ComplexPolicy {
        self.options
            .field_policies
            .get(path)
            .copied()
            .unwrap_or(self.options.complex_policy)
    }

    fn walk(
        &mut self,
        field: &arrow::datatypes::Field,
        prefix: Option<&str>,
        steps: &mut Vec<usize>,
        nullable_so_far: bool,
    ) -> AppResult<()> {
        let name = match prefix {
            Some(p) => format!("{p}.{}", escape_key(field.name())),
            None => escape_key(field.name()),
        };
        let nullable = nullable_so_far || field.is_nullable();
        let dt = field.data_type();
        if leaf_accounting_fragile(dt) {
            self.stats_ok = false;
        }
        if let DataType::Struct(children) = dt {
            // Structs are ALWAYS flattened: stable path-based names.
            for (i, child) in children.iter().enumerate() {
                steps.push(i);
                self.walk(child, Some(&name), steps, nullable)?;
                steps.pop();
            }
            return Ok(());
        }
        if is_primitive(dt) {
            let resolved = resolved_type(dt);
            if leaf_accounting_fragile(&resolved) {
                self.stats_ok = false;
            }
            let (logical, time_zone, input_formats) = logical_of(&resolved);
            self.cols.push(OutCol {
                name,
                kind: OutKind::Primitive,
                steps: steps.clone(),
                data_type: resolved,
                logical,
                time_zone,
                input_formats,
                nullable,
                leaf_start: self.leaf_cursor,
            });
            self.leaf_cursor += leaf_count(dt);
            return Ok(());
        }
        // Complex field: list / map / other — explicit policy.
        let policy = self.policy_for(&name);
        match policy {
            ComplexPolicy::Reject => {}
            ComplexPolicy::PreserveJson => {
                self.cols.push(OutCol {
                    name,
                    kind: OutKind::Json,
                    steps: steps.clone(),
                    data_type: resolved_type(dt),
                    logical: LogicalType::Json,
                    time_zone: None,
                    input_formats: None,
                    nullable,
                    leaf_start: self.leaf_cursor,
                });
            }
            ComplexPolicy::Explode => {
                if !self.allow_explode {
                    return Err(AppError::invalid(format!(
                        "field \"{name}\": explode changes the row count, which an indexed \
                         read-only document cannot represent — open it as editable instead, \
                         or choose preserveJson/reject"
                    )));
                }
                let element = match dt {
                    DataType::List(f) | DataType::LargeList(f) | DataType::FixedSizeList(f, _) => f,
                    _ => {
                        return Err(AppError::invalid(format!(
                            "field \"{name}\": only list columns can be exploded into rows \
                             (maps and other nested types support preserveJson or reject)"
                        )))
                    }
                };
                if self.explode_seen {
                    return Err(AppError::invalid(
                        "only one list column can be exploded per open; choose preserveJson or \
                         reject for the others",
                    ));
                }
                self.explode_seen = true;
                let elem_type = resolved_type(element.data_type());
                let (logical, time_zone, input_formats) = if is_primitive(&elem_type) {
                    logical_of(&elem_type)
                } else {
                    (LogicalType::Json, None, None)
                };
                self.cols.push(OutCol {
                    name,
                    kind: OutKind::Explode,
                    steps: steps.clone(),
                    data_type: resolved_type(dt),
                    logical,
                    time_zone,
                    input_formats,
                    nullable: true,
                    leaf_start: self.leaf_cursor,
                });
            }
        }
        self.leaf_cursor += leaf_count(dt);
        Ok(())
    }
}

fn build_projection(
    schema: &ArrowSchema,
    options: &ColumnarOpenOptions,
    allow_explode: bool,
) -> AppResult<Projection> {
    let mut builder = ProjectionBuilder {
        options,
        allow_explode,
        cols: Vec::new(),
        stats_ok: true,
        leaf_cursor: 0,
        explode_seen: false,
    };
    let mut steps = Vec::new();
    for (i, field) in schema.fields().iter().enumerate() {
        steps.push(i);
        builder.walk(field, None, &mut steps, false)?;
        steps.pop();
    }
    if builder.cols.is_empty() {
        return Err(AppError::invalid(
            "no readable columns (every field was rejected or the schema is empty)",
        ));
    }
    Ok(Projection {
        cols: builder.cols,
        stats_ok: builder.stats_ok,
        total_leaves: builder.leaf_cursor,
    })
}

/// The generated F31 schema for one output column. Column IDs are positional
/// placeholders (`c{i}`), matching what [`crate::document::Document`] assigns.
fn column_schemas(cols: &[OutCol]) -> Vec<ColumnSchema> {
    cols.iter()
        .enumerate()
        .map(|(i, col)| {
            let mut s = ColumnSchema::new(format!("c{i}"), col.name.clone(), col.logical);
            s.nullable = col.nullable;
            s.time_zone = col.time_zone.clone();
            s.input_formats = col.input_formats.clone();
            s
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Typed value -> canonical text
// ---------------------------------------------------------------------------

/// Exact decimal text from a mantissa's decimal digits and a scale.
fn decimal_text_from_digits(negative: bool, digits: &str, scale: i32) -> String {
    let digits = digits.trim_start_matches('0');
    let mut digits = if digits.is_empty() { "0" } else { digits }.to_string();
    let negative = negative && digits != "0";
    if scale <= 0 {
        if digits != "0" {
            digits.push_str(&"0".repeat((-scale) as usize));
        }
        DecimalValue {
            negative,
            digits,
            scale: 0,
        }
        .to_plain_string()
    } else {
        DecimalValue {
            negative,
            digits,
            scale: scale as u32,
        }
        .to_plain_string()
    }
}

fn decimal128_text(v: i128, scale: i32) -> String {
    let negative = v < 0;
    let digits = v.unsigned_abs().to_string();
    decimal_text_from_digits(negative, &digits, scale)
}

/// Exact [`DecimalValue`] from an i128 mantissa (for statistics pruning).
fn decimal128_value(v: i128, scale: i32) -> DecimalValue {
    let text = decimal128_text(v, scale);
    let negative = text.starts_with('-');
    let unsigned = text.trim_start_matches('-');
    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some((i, f)) => (i, f),
        None => (unsigned, ""),
    };
    let combined = format!("{int_part}{frac_part}");
    let trimmed = combined.trim_start_matches('0');
    DecimalValue {
        negative: negative && !trimmed.is_empty(),
        digits: if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed.to_string()
        },
        scale: frac_part.len() as u32,
    }
}

fn hex_text(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn timestamp_naive(v: i64, unit: &TimeUnit) -> Option<NaiveDateTime> {
    match unit {
        TimeUnit::Second => DateTime::from_timestamp(v, 0),
        TimeUnit::Millisecond => DateTime::from_timestamp_millis(v),
        TimeUnit::Microsecond => DateTime::from_timestamp_micros(v),
        TimeUnit::Nanosecond => Some(DateTime::from_timestamp_nanos(v)),
    }
    .map(|dt| dt.naive_utc())
}

/// ISO text for a timestamp; zoned columns carry the UTC instant with an
/// explicit `Z`. Values outside chrono's ±262143-year range fall back to the
/// raw tick count (reads stay total; such values are astronomically rare).
fn timestamp_text(v: i64, unit: &TimeUnit, zoned: bool) -> String {
    match timestamp_naive(v, unit) {
        Some(ndt) => {
            if zoned {
                ndt.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
            } else {
                ndt.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
            }
        }
        None => v.to_string(),
    }
}

fn date32_text(days: i32) -> String {
    match DateTime::from_timestamp(i64::from(days) * 86_400, 0) {
        Some(dt) => dt.date_naive().format("%Y-%m-%d").to_string(),
        None => days.to_string(),
    }
}

fn date64_text(ms: i64) -> String {
    match DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt.date_naive().format("%Y-%m-%d").to_string(),
        None => ms.to_string(),
    }
}

fn time_text(secs: i64, nanos: u32) -> String {
    match u32::try_from(secs)
        .ok()
        .and_then(|s| NaiveTime::from_num_seconds_from_midnight_opt(s, nanos))
    {
        Some(t) => t.format("%H:%M:%S%.f").to_string(),
        None => format!("{secs}.{nanos:09}"),
    }
}

macro_rules! int_arm {
    ($arr:expr, $i:expr, $ty:ty) => {{
        let a = $arr
            .as_any()
            .downcast_ref::<$ty>()
            .ok_or_else(|| arrow_err("array type mismatch"))?;
        a.value($i).to_string()
    }};
}

/// Render one primitive cell to canonical text (`None` = columnar NULL).
fn render_primitive(arr: &dyn Array, i: usize) -> AppResult<Option<String>> {
    if arr.is_null(i) {
        return Ok(None);
    }
    let text = match arr.data_type() {
        DataType::Null => return Ok(None),
        DataType::Boolean => {
            let a = arr
                .as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_string()
        }
        DataType::Int8 => int_arm!(arr, i, Int8Array),
        DataType::Int16 => int_arm!(arr, i, Int16Array),
        DataType::Int32 => int_arm!(arr, i, Int32Array),
        DataType::Int64 => int_arm!(arr, i, Int64Array),
        DataType::UInt8 => int_arm!(arr, i, UInt8Array),
        DataType::UInt16 => int_arm!(arr, i, UInt16Array),
        DataType::UInt32 => int_arm!(arr, i, UInt32Array),
        DataType::UInt64 => int_arm!(arr, i, UInt64Array),
        DataType::Float16 => {
            let a = arr
                .as_any()
                .downcast_ref::<Float16Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_f32().to_string()
        }
        DataType::Float32 => {
            let a = arr
                .as_any()
                .downcast_ref::<Float32Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_string()
        }
        DataType::Float64 => {
            let a = arr
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_string()
        }
        DataType::Decimal128(_, scale) => {
            let a = arr
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            decimal128_text(a.value(i), i32::from(*scale))
        }
        DataType::Decimal256(_, scale) => {
            let a = arr
                .as_any()
                .downcast_ref::<Decimal256Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            let s = a.value(i).to_string();
            let negative = s.starts_with('-');
            decimal_text_from_digits(negative, s.trim_start_matches('-'), i32::from(*scale))
        }
        DataType::Utf8 => {
            let a = arr
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_string()
        }
        DataType::LargeUtf8 => {
            let a = arr
                .as_any()
                .downcast_ref::<LargeStringArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_string()
        }
        DataType::Utf8View => {
            let a = arr
                .as_any()
                .downcast_ref::<StringViewArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            a.value(i).to_string()
        }
        DataType::Binary => {
            let a = arr
                .as_any()
                .downcast_ref::<BinaryArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            hex_text(a.value(i))
        }
        DataType::LargeBinary => {
            let a = arr
                .as_any()
                .downcast_ref::<LargeBinaryArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            hex_text(a.value(i))
        }
        DataType::BinaryView => {
            let a = arr
                .as_any()
                .downcast_ref::<BinaryViewArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            hex_text(a.value(i))
        }
        DataType::FixedSizeBinary(_) => {
            let a = arr
                .as_any()
                .downcast_ref::<FixedSizeBinaryArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            hex_text(a.value(i))
        }
        DataType::Date32 => {
            let a = arr
                .as_any()
                .downcast_ref::<Date32Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            date32_text(a.value(i))
        }
        DataType::Date64 => {
            let a = arr
                .as_any()
                .downcast_ref::<Date64Array>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            date64_text(a.value(i))
        }
        DataType::Time32(TimeUnit::Second) => {
            let a = arr
                .as_any()
                .downcast_ref::<Time32SecondArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            time_text(i64::from(a.value(i)), 0)
        }
        DataType::Time32(TimeUnit::Millisecond) => {
            let a = arr
                .as_any()
                .downcast_ref::<Time32MillisecondArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            let v = i64::from(a.value(i));
            time_text(
                v.div_euclid(1_000),
                (v.rem_euclid(1_000) * 1_000_000) as u32,
            )
        }
        DataType::Time64(TimeUnit::Microsecond) => {
            let a = arr
                .as_any()
                .downcast_ref::<Time64MicrosecondArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            let v = a.value(i);
            time_text(
                v.div_euclid(1_000_000),
                (v.rem_euclid(1_000_000) * 1_000) as u32,
            )
        }
        DataType::Time64(TimeUnit::Nanosecond) => {
            let a = arr
                .as_any()
                .downcast_ref::<Time64NanosecondArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            let v = a.value(i);
            time_text(
                v.div_euclid(1_000_000_000),
                v.rem_euclid(1_000_000_000) as u32,
            )
        }
        DataType::Timestamp(unit, tz) => {
            let v = timestamp_ticks(arr, i, unit)?;
            timestamp_text(v, unit, tz.is_some())
        }
        DataType::Dictionary(_, value) => {
            let cast = arrow::compute::cast(arr, value).map_err(arrow_err)?;
            return render_primitive(cast.as_ref(), i);
        }
        // Durations, intervals and anything else exotic: arrow's own display
        // (deterministic, documented as non-round-tripping text).
        _ => {
            let options = arrow::util::display::FormatOptions::default();
            let fmt =
                arrow::util::display::ArrayFormatter::try_new(arr, &options).map_err(arrow_err)?;
            fmt.value(i).try_to_string().map_err(arrow_err)?
        }
    };
    Ok(Some(text))
}

fn timestamp_ticks(arr: &dyn Array, i: usize, unit: &TimeUnit) -> AppResult<i64> {
    Ok(match unit {
        TimeUnit::Second => arr
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(i),
        TimeUnit::Millisecond => arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(i),
        TimeUnit::Microsecond => arr
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(i),
        TimeUnit::Nanosecond => arr
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(i),
    })
}

// ---------------------------------------------------------------------------
// Complex values -> canonical JSON
// ---------------------------------------------------------------------------

/// One value as a `serde_json::Value`. Exact where JSON is exact (i64/u64
/// carry full precision as JSON numbers; decimals, timestamps and binary
/// become strings so nothing is rounded).
fn json_at(arr: &dyn Array, i: usize) -> AppResult<serde_json::Value> {
    use serde_json::Value;
    if arr.is_null(i) {
        return Ok(Value::Null);
    }
    Ok(match arr.data_type() {
        DataType::Boolean => Value::Bool(
            arr.as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?
                .value(i),
        ),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            let text = render_primitive(arr, i)?.unwrap_or_default();
            let v: i64 = text.parse().map_err(arrow_err)?;
            Value::Number(v.into())
        }
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            let text = render_primitive(arr, i)?.unwrap_or_default();
            let v: u64 = text.parse().map_err(arrow_err)?;
            Value::Number(v.into())
        }
        DataType::Float16 | DataType::Float32 | DataType::Float64 => {
            let text = render_primitive(arr, i)?.unwrap_or_default();
            let v: f64 = text.parse().map_err(arrow_err)?;
            match serde_json::Number::from_f64(v) {
                Some(n) => Value::Number(n),
                None => Value::String(text),
            }
        }
        DataType::Struct(fields) => {
            let s = arr
                .as_any()
                .downcast_ref::<StructArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            let mut obj = serde_json::Map::new();
            for (field, column) in fields.iter().zip(s.columns()) {
                obj.insert(field.name().clone(), json_at(column.as_ref(), i)?);
            }
            Value::Object(obj)
        }
        DataType::List(_) => {
            let l = arr
                .as_any()
                .downcast_ref::<ListArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            json_list(&l.value(i))?
        }
        DataType::LargeList(_) => {
            let l = arr
                .as_any()
                .downcast_ref::<LargeListArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            json_list(&l.value(i))?
        }
        DataType::FixedSizeList(_, _) => {
            let l = arr
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            json_list(&l.value(i))?
        }
        DataType::Map(_, _) => {
            let m = arr
                .as_any()
                .downcast_ref::<MapArray>()
                .ok_or_else(|| arrow_err("array type mismatch"))?;
            let entries = m.value(i);
            let keys = entries.column(0);
            let values = entries.column(1);
            let mut obj = serde_json::Map::new();
            for e in 0..entries.len() {
                let key = render_primitive(keys.as_ref(), e)?.unwrap_or_else(|| "null".to_string());
                obj.insert(key, json_at(values.as_ref(), e)?);
            }
            Value::Object(obj)
        }
        DataType::Dictionary(_, value) => {
            let cast = arrow::compute::cast(arr, value).map_err(arrow_err)?;
            json_at(cast.as_ref(), i)?
        }
        // Every remaining primitive renders through its canonical text
        // (decimals, dates, timestamps, times, binary-as-hex, strings).
        _ => match render_primitive(arr, i)? {
            Some(text) => Value::String(text),
            None => Value::Null,
        },
    })
}

fn json_list(elems: &ArrayRef) -> AppResult<serde_json::Value> {
    let mut out = Vec::with_capacity(elems.len());
    for e in 0..elems.len() {
        out.push(json_at(elems.as_ref(), e)?);
    }
    Ok(serde_json::Value::Array(out))
}

// ---------------------------------------------------------------------------
// Batch -> text rows
// ---------------------------------------------------------------------------

/// Per-batch reader state for one output column: the structs along the
/// descent (for ancestor-null checks) and the leaf array.
struct LeafCursor {
    parents: Vec<ArrayRef>,
    leaf: ArrayRef,
}

impl LeafCursor {
    fn new(col: &OutCol, batch: &RecordBatch) -> AppResult<LeafCursor> {
        let mut parents = Vec::new();
        let mut current: ArrayRef = batch
            .columns()
            .get(col.steps[0])
            .ok_or_else(|| arrow_err("record batch is narrower than the schema"))?
            .clone();
        for &step in &col.steps[1..] {
            let s = current
                .as_any()
                .downcast_ref::<StructArray>()
                .ok_or_else(|| arrow_err("schema mismatch: expected a struct"))?;
            let child = s
                .columns()
                .get(step)
                .ok_or_else(|| arrow_err("schema mismatch: struct child out of range"))?
                .clone();
            parents.push(current);
            current = child;
        }
        // Dictionary-encoded primitives decode once per batch, not per cell.
        if col.kind == OutKind::Primitive
            && matches!(current.data_type(), DataType::Dictionary(_, _))
        {
            current = arrow::compute::cast(current.as_ref(), &col.data_type).map_err(arrow_err)?;
        }
        Ok(LeafCursor {
            parents,
            leaf: current,
        })
    }

    /// A cell is null when the leaf OR any ancestor struct is null.
    fn cell(&self, col: &OutCol, row: usize) -> AppResult<Option<String>> {
        for parent in &self.parents {
            if parent.is_null(row) {
                return Ok(None);
            }
        }
        match col.kind {
            OutKind::Primitive => render_primitive(self.leaf.as_ref(), row),
            OutKind::Json => {
                if self.leaf.is_null(row) {
                    Ok(None)
                } else {
                    let value = json_at(self.leaf.as_ref(), row)?;
                    Ok(Some(serde_json::to_string(&value).map_err(|e| {
                        arrow_err(format!("JSON encoding failed: {e}"))
                    })?))
                }
            }
            OutKind::Explode => Err(AppError::Other(
                "internal: explode columns are handled by the editable open".into(),
            )),
        }
    }
}

/// Append every row of `batch` to `out` as optional text cells.
fn render_batch(cols: &[OutCol], batch: &RecordBatch, out: &mut Vec<TabularRow>) -> AppResult<()> {
    let cursors: Vec<LeafCursor> = cols
        .iter()
        .map(|c| LeafCursor::new(c, batch))
        .collect::<AppResult<_>>()?;
    for row in 0..batch.num_rows() {
        let mut cells = Vec::with_capacity(cols.len());
        for (col, cursor) in cols.iter().zip(&cursors) {
            cells.push(cursor.cell(col, row)?);
        }
        out.push(cells);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row-group statistics (parquet pruning)
// ---------------------------------------------------------------------------

/// Typed min/max of one output column within one row group.
#[derive(Debug, Clone)]
struct StatsRange {
    min: TypedValue,
    max: TypedValue,
}

/// Sign-extend big-endian two's-complement bytes into an i128 (decimal
/// statistics are stored this way for byte-array physical types).
fn i128_from_be(bytes: &[u8]) -> Option<i128> {
    if bytes.is_empty() || bytes.len() > 16 {
        return None;
    }
    let fill = if bytes[0] & 0x80 != 0 { 0xFF } else { 0x00 };
    let mut buf = [fill; 16];
    buf[16 - bytes.len()..].copy_from_slice(bytes);
    Some(i128::from_be_bytes(buf))
}

/// Widen an f32-derived bound so text-plane parsing (shortest decimal of the
/// f32, re-parsed as f64) can never fall outside the pruning interval.
fn widen_down(v: f64) -> f64 {
    v - v.abs() * 1e-6 - f64::MIN_POSITIVE
}
fn widen_up(v: f64) -> f64 {
    v + v.abs() * 1e-6 + f64::MIN_POSITIVE
}

/// Extract the typed min/max for `col` from one row group, when the
/// statistics exist and can be trusted for the column's logical type.
fn stat_range(col: &OutCol, stats: &Statistics) -> Option<StatsRange> {
    let range = |min: TypedValue, max: TypedValue| Some(StatsRange { min, max });
    match (&col.data_type, stats) {
        // Signed integers (and the two small unsigned widths whose values are
        // non-negative in a signed physical column, so ordering agrees).
        (
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::UInt8 | DataType::UInt16,
            Statistics::Int32(s),
        ) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            range(
                TypedValue::Integer(i128::from(min)),
                TypedValue::Integer(i128::from(max)),
            )
        }
        (DataType::Int64, Statistics::Int64(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            range(
                TypedValue::Integer(i128::from(min)),
                TypedValue::Integer(i128::from(max)),
            )
        }
        // UInt32/UInt64 are stored sign-reinterpreted with unsigned sort
        // order; older writers ordered them signed. Deliberately skipped.
        (DataType::Float32, Statistics::Float(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            if min.is_nan() || max.is_nan() {
                return None;
            }
            range(
                TypedValue::Float(widen_down(f64::from(min))),
                TypedValue::Float(widen_up(f64::from(max))),
            )
        }
        (DataType::Float64, Statistics::Double(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            if min.is_nan() || max.is_nan() {
                return None;
            }
            range(TypedValue::Float(min), TypedValue::Float(max))
        }
        (DataType::Decimal128(_, scale), Statistics::Int32(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            range(
                TypedValue::Decimal(decimal128_value(i128::from(min), i32::from(*scale))),
                TypedValue::Decimal(decimal128_value(i128::from(max), i32::from(*scale))),
            )
        }
        (DataType::Decimal128(_, scale), Statistics::Int64(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            range(
                TypedValue::Decimal(decimal128_value(i128::from(min), i32::from(*scale))),
                TypedValue::Decimal(decimal128_value(i128::from(max), i32::from(*scale))),
            )
        }
        (DataType::Decimal128(_, scale), Statistics::FixedLenByteArray(s)) => {
            // Byte-array decimal ordering was wrong in the deprecated stats
            // fields; only trust the modern min_value/max_value pair.
            if stats.is_min_max_deprecated() {
                return None;
            }
            let min = i128_from_be(s.min_opt()?.data())?;
            let max = i128_from_be(s.max_opt()?.data())?;
            range(
                TypedValue::Decimal(decimal128_value(min, i32::from(*scale))),
                TypedValue::Decimal(decimal128_value(max, i32::from(*scale))),
            )
        }
        (DataType::Decimal128(_, scale), Statistics::ByteArray(s)) => {
            if stats.is_min_max_deprecated() {
                return None;
            }
            let min = i128_from_be(s.min_opt()?.data())?;
            let max = i128_from_be(s.max_opt()?.data())?;
            range(
                TypedValue::Decimal(decimal128_value(min, i32::from(*scale))),
                TypedValue::Decimal(decimal128_value(max, i32::from(*scale))),
            )
        }
        (DataType::Date32, Statistics::Int32(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            let to_date = |days: i32| {
                DateTime::from_timestamp(i64::from(days) * 86_400, 0).map(|d| d.date_naive())
            };
            range(
                TypedValue::Date(to_date(min)?),
                TypedValue::Date(to_date(max)?),
            )
        }
        (DataType::Timestamp(unit, _), Statistics::Int64(s)) => {
            let (min, max) = (*s.min_opt()?, *s.max_opt()?);
            range(
                TypedValue::DateTime(timestamp_naive(min, unit)?),
                TypedValue::DateTime(timestamp_naive(max, unit)?),
            )
        }
        _ => None,
    }
}

/// Per-row-group, per-output-column stats, when the leaf accounting checks
/// out against the file's actual parquet schema.
fn extract_stats(
    meta: &ParquetMetaData,
    cols: &[OutCol],
    projection: &Projection,
) -> Option<Vec<Vec<Option<StatsRange>>>> {
    if !projection.stats_ok {
        return None;
    }
    if meta.file_metadata().schema_descr().num_columns() != projection.total_leaves {
        return None;
    }
    let mut out = Vec::with_capacity(meta.num_row_groups());
    for rg in meta.row_groups() {
        let mut per_col = Vec::with_capacity(cols.len());
        for col in cols {
            let entry = if col.kind == OutKind::Primitive {
                rg.columns()
                    .get(col.leaf_start)
                    .and_then(|c| c.statistics())
                    .and_then(|s| stat_range(col, s))
            } else {
                None
            };
            per_col.push(entry);
        }
        out.push(per_col);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// The columnar handle (indexed read-only backing)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CachedBlock {
    rows: Vec<TabularRow>,
    bytes: usize,
    stamp: u64,
}

#[derive(Debug)]
struct BlockCache {
    blocks: HashMap<usize, CachedBlock>,
    bytes: usize,
    budget: usize,
    next_stamp: u64,
}

impl BlockCache {
    fn new(budget: usize) -> BlockCache {
        BlockCache {
            blocks: HashMap::new(),
            bytes: 0,
            budget,
            next_stamp: 0,
        }
    }

    fn touch(&mut self, block: usize) -> bool {
        self.next_stamp += 1;
        let stamp = self.next_stamp;
        match self.blocks.get_mut(&block) {
            Some(b) => {
                b.stamp = stamp;
                true
            }
            None => false,
        }
    }

    fn insert(&mut self, block: usize, rows: Vec<TabularRow>) {
        let bytes = block_bytes(&rows);
        self.next_stamp += 1;
        let stamp = self.next_stamp;
        if let Some(old) = self
            .blocks
            .insert(block, CachedBlock { rows, bytes, stamp })
        {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        // Evict least-recently-used blocks past the budget; the newest block
        // always survives so an oversized block still works.
        while self.bytes > self.budget && self.blocks.len() > 1 {
            let Some((&victim, _)) = self.blocks.iter().min_by_key(|(_, b)| b.stamp) else {
                break;
            };
            if victim == block {
                break;
            }
            if let Some(old) = self.blocks.remove(&victim) {
                self.bytes -= old.bytes;
            }
        }
    }
}

fn block_bytes(rows: &[TabularRow]) -> usize {
    let mut total = 0usize;
    for row in rows {
        total += ROW_OVERHEAD as usize;
        for cell in row {
            total += CELL_OVERHEAD as usize + cell.as_ref().map_or(0, |s| s.len());
        }
    }
    total
}

/// Read-only handle over a columnar file: windowed text reads through a
/// bounded LRU of decoded blocks. The document integration mirrors
/// [`index::IndexHandle`] (`visit` / `visit_at` over `&[String]`); the
/// `Option` plane ([`ColumnarHandle::read_optional`]) additionally keeps
/// columnar NULL (`None`) distinct from the empty string (`Some("")`).
#[derive(Debug)]
pub struct ColumnarHandle {
    path: PathBuf,
    format: ColumnarFormat,
    cols: Vec<OutCol>,
    schemas: Vec<ColumnSchema>,
    /// Prefix sums of the per-chunk (row group / record batch) row counts
    /// (len = chunks + 1; last = row count).
    chunk_starts: Vec<usize>,
    n_rows: usize,
    /// Identity of the source file as of the open; every read re-validates
    /// it so an in-place rewrite errors instead of decoding stale bytes.
    source_check: Option<FileFingerprint>,
    editable_estimate: u64,
    /// Parquet only: per-row-group typed min/max per output column.
    stats: Option<Vec<Vec<Option<StatsRange>>>>,
    cache: Mutex<BlockCache>,
}

impl ColumnarHandle {
    pub fn n_rows(&self) -> usize {
        self.n_rows
    }

    pub fn n_cols(&self) -> usize {
        self.cols.len()
    }

    pub fn format(&self) -> ColumnarFormat {
        self.format
    }

    /// Flattened output column names, in read order.
    pub fn headers(&self) -> Vec<String> {
        self.cols.iter().map(|c| c.name.clone()).collect()
    }

    /// Generated F31 schemas, parallel to [`ColumnarHandle::headers`]
    /// (column IDs are positional `c{i}` placeholders).
    pub fn schemas(&self) -> &[ColumnSchema] {
        &self.schemas
    }

    /// Rough bytes a fully editable in-memory conversion would need.
    pub fn editable_estimate(&self) -> u64 {
        self.editable_estimate
    }

    /// Whether converting to editable should require explicit confirmation.
    pub fn convert_needs_decision(&self) -> bool {
        self.editable_estimate > index::MEMORY_DECISION_THRESHOLD
    }

    #[cfg(test)]
    fn cached_bytes(&self) -> usize {
        self.cache.lock().map(|c| c.bytes).unwrap_or(0)
    }

    #[cfg(test)]
    fn cached_blocks(&self) -> usize {
        self.cache.lock().map(|c| c.blocks.len()).unwrap_or(0)
    }

    /// Tabular columns for the [`TabularSource`] adapter and export stages.
    pub fn tabular_columns(&self) -> Vec<TabularColumn> {
        self.cols
            .iter()
            .zip(&self.schemas)
            .enumerate()
            .map(|(i, (col, schema))| TabularColumn {
                name: col.name.clone(),
                id: Some(format!("c{i}")),
                schema: Some(schema.clone()),
            })
            .collect()
    }

    // ----- reads -----------------------------------------------------------

    fn check_source(&self) -> AppResult<()> {
        if let Some(expected) = self.source_check {
            if util::stat_fingerprint(&self.path) != Some(expected) {
                return Err(AppError::Other(
                    "the source file changed on disk; reload the document".into(),
                ));
            }
        }
        Ok(())
    }

    /// Run `f` over the cached rows of `block`, decoding it on a miss. The
    /// cache lock is held for the duration of `f`; callers must NOT invoke
    /// user callbacks inside it (they copy what they need out instead), so a
    /// callback that re-enters this handle cannot deadlock.
    fn with_block<T>(&self, block: usize, f: impl FnOnce(&[TabularRow]) -> T) -> AppResult<T> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| AppError::Other("columnar cache lock poisoned".into()))?;
        if !cache.touch(block) {
            let rows = self.decode_block(block)?;
            cache.insert(block, rows);
        }
        let cached = cache
            .blocks
            .get(&block)
            .expect("block inserted or touched above");
        Ok(f(&cached.rows))
    }

    /// Decode text rows for block `block` (rows `[block*BLOCK_ROWS, ...)`).
    fn decode_block(&self, block: usize) -> AppResult<Vec<TabularRow>> {
        self.check_source()?;
        let start = block * BLOCK_ROWS;
        let end = (start + BLOCK_ROWS).min(self.n_rows);
        if start >= end {
            return Ok(Vec::new());
        }
        let mut rows = Vec::with_capacity(end - start);
        match self.format {
            ColumnarFormat::Parquet => {
                let file = File::open(&self.path)?;
                let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(arrow_err)?;
                let reader = builder
                    .with_batch_size(BLOCK_ROWS.min(end - start))
                    .with_offset(start)
                    .with_limit(end - start)
                    .build()
                    .map_err(arrow_err)?;
                for batch in reader {
                    let batch = batch.map_err(arrow_err)?;
                    render_batch(&self.cols, &batch, &mut rows)?;
                }
            }
            ColumnarFormat::ArrowFile => {
                let mut reader =
                    IpcFileReader::try_new(BufReader::new(File::open(&self.path)?), None)
                        .map_err(arrow_err)?;
                let first = self.chunk_of(start);
                let last = self.chunk_of(end - 1);
                for chunk in first..=last {
                    reader.set_index(chunk).map_err(arrow_err)?;
                    let batch = reader
                        .next()
                        .ok_or_else(|| arrow_err("record batch missing (file truncated?)"))?
                        .map_err(arrow_err)?;
                    self.render_chunk_slice(&batch, chunk, start, end, &mut rows)?;
                }
            }
            ColumnarFormat::ArrowStream => {
                let reader =
                    IpcStreamReader::try_new(BufReader::new(File::open(&self.path)?), None)
                        .map_err(arrow_err)?;
                let first = self.chunk_of(start);
                let last = self.chunk_of(end - 1);
                for (chunk, batch) in reader.enumerate() {
                    if chunk > last {
                        break;
                    }
                    let batch = batch.map_err(arrow_err)?;
                    if chunk < first {
                        continue;
                    }
                    self.render_chunk_slice(&batch, chunk, start, end, &mut rows)?;
                }
            }
        }
        if rows.len() != end - start {
            return Err(AppError::Other(
                "the source file changed on disk; reload the document".into(),
            ));
        }
        Ok(rows)
    }

    fn render_chunk_slice(
        &self,
        batch: &RecordBatch,
        chunk: usize,
        start: usize,
        end: usize,
        rows: &mut Vec<TabularRow>,
    ) -> AppResult<()> {
        let chunk_start = self.chunk_starts[chunk];
        let chunk_end = self.chunk_starts[chunk + 1];
        if batch.num_rows() != chunk_end - chunk_start {
            return Err(AppError::Other(
                "the source file changed on disk; reload the document".into(),
            ));
        }
        let lo = start.max(chunk_start) - chunk_start;
        let hi = end.min(chunk_end) - chunk_start;
        let sliced = batch.slice(lo, hi - lo);
        render_batch(&self.cols, &sliced, rows)
    }

    /// Which chunk (row group / batch) contains absolute row `row`.
    fn chunk_of(&self, row: usize) -> usize {
        debug_assert!(row < self.n_rows);
        match self.chunk_starts.binary_search(&row) {
            Ok(c) => c,
            Err(insert) => insert - 1,
        }
    }

    /// Owned `Option`-plane rows `[offset, offset+limit)` — columnar NULL is
    /// `None`, an empty string is `Some("")`. Window semantics mirror
    /// [`TabularSource::read_rows`].
    pub fn read_optional(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>> {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.n_rows);
        let end = start.saturating_add(limit).min(self.n_rows);
        let mut out = Vec::with_capacity(end - start);
        let mut at = start;
        while at < end {
            if let Some(ctx) = ctx {
                ctx.check()?;
            }
            let block = at / BLOCK_ROWS;
            let block_start = block * BLOCK_ROWS;
            let hi = end.min(block_start + BLOCK_ROWS);
            let mut copied = self.with_block(block, |rows| {
                rows[at - block_start..hi - block_start].to_vec()
            })?;
            out.append(&mut copied);
            at = hi;
        }
        Ok(out)
    }

    /// Owned TEXT-plane rows for `[lo, hi)` within one block (NULL renders as
    /// an empty cell). Copies out under the cache lock so callbacks never run
    /// while it is held.
    fn text_rows(&self, block: usize, lo: usize, hi: usize) -> AppResult<Vec<Vec<String>>> {
        self.with_block(block, |rows| {
            rows[lo..hi]
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|c| c.clone().unwrap_or_default())
                        .collect::<Vec<String>>()
                })
                .collect()
        })
    }

    /// Visit data rows `[range)` in order over the TEXT plane (NULL renders
    /// as an empty cell). Mirrors [`index::IndexHandle::visit`].
    pub fn visit(
        &self,
        range: Range<usize>,
        f: &mut dyn FnMut(usize, &[String]) -> AppResult<bool>,
    ) -> AppResult<()> {
        let end = range.end.min(self.n_rows);
        let mut start = range.start.min(end);
        while start < end {
            let block = start / BLOCK_ROWS;
            let block_start = block * BLOCK_ROWS;
            let hi = end.min(block_start + BLOCK_ROWS);
            let rows = self.text_rows(block, start - block_start, hi - block_start)?;
            for (i, row) in rows.iter().enumerate() {
                if !f(start + i, row)? {
                    return Ok(());
                }
            }
            start = hi;
        }
        Ok(())
    }

    /// Visit specific rows in CALLER order (text plane). Mirrors
    /// [`index::IndexHandle::visit_at`].
    pub fn visit_at(
        &self,
        indices: &[usize],
        f: &mut dyn FnMut(usize, &[String]) -> AppResult<bool>,
    ) -> AppResult<()> {
        if let Some(&bad) = indices.iter().find(|&&i| i >= self.n_rows) {
            return Err(AppError::invalid(format!("row {bad} is out of range")));
        }
        for &i in indices {
            let block = i / BLOCK_ROWS;
            let block_start = block * BLOCK_ROWS;
            let row = self
                .text_rows(block, i - block_start, i - block_start + 1)?
                .pop()
                .expect("one row requested");
            if !f(i, &row)? {
                return Ok(());
            }
        }
        Ok(())
    }

    // ----- statistics pruning ------------------------------------------------

    /// Absolute row ranges a filter scan must visit, when row-group
    /// statistics prove the remaining groups cannot match. `None` = no
    /// pruning possible (scan everything); pruning is conservative, so rows
    /// outside the returned ranges are GUARANTEED not to match `spec`.
    ///
    /// `schema_of` must resolve the document's CURRENT per-column schemas —
    /// the same ones [`crate::filter`] compiles conditions against.
    pub fn filter_scan_ranges(
        &self,
        spec: &FilterGroup,
        schema_of: &dyn Fn(usize) -> Option<ColumnSchema>,
    ) -> Option<Vec<Range<usize>>> {
        let stats = self.stats.as_ref()?;
        let mut conds = Vec::new();
        required_conditions(spec, &mut conds);
        if conds.is_empty() {
            return None;
        }
        let mut tests = Vec::new();
        for c in conds {
            if let Some(test) = self.prune_test(c, schema_of) {
                tests.push(test);
            }
        }
        if tests.is_empty() {
            return None;
        }
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (group, per_col) in stats.iter().enumerate() {
            let skip = tests.iter().any(|t| {
                per_col
                    .get(t.column)
                    .and_then(|r| r.as_ref())
                    .is_some_and(|r| t.impossible(r))
            });
            if skip {
                continue;
            }
            let start = self.chunk_starts[group];
            let end = self.chunk_starts[group + 1];
            match ranges.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => ranges.push(start..end),
            }
        }
        Some(ranges)
    }

    /// Compile one condition into a pruning test, when it is eligible: an
    /// equality or range comparison, on a numeric/date/datetime column whose
    /// current schema still parses exactly like the open-time one.
    fn prune_test(
        &self,
        c: &FilterCondition,
        schema_of: &dyn Fn(usize) -> Option<ColumnSchema>,
    ) -> Option<PruneTest> {
        let op = match c.op {
            FilterOp::Equals => PruneOp::Eq,
            FilterOp::Gt => PruneOp::Gt,
            FilterOp::Gte => PruneOp::Gte,
            FilterOp::Lt => PruneOp::Lt,
            FilterOp::Lte => PruneOp::Lte,
            _ => return None,
        };
        let opened = self.schemas.get(c.column)?;
        let current = schema_of(c.column)?;
        if !(current.logical_type.is_numeric() || current.logical_type.is_temporal()) {
            return None;
        }
        // The statistics describe text produced under the OPEN-TIME schema;
        // if any parse-relevant field moved since, classification could
        // disagree with the stats domain — fall back to the full scan.
        if current.logical_type != opened.logical_type
            || current.locale != opened.locale
            || current.time_zone != opened.time_zone
            || current.input_formats != opened.input_formats
        {
            return None;
        }
        let trimmed = c.value.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Text equality compares the FULL cell text; canonical numeric text
        // never carries surrounding whitespace, so a padded value would need
        // the full scan (it simply never matches — but stay conservative).
        if op == PruneOp::Eq && trimmed != c.value {
            return None;
        }
        let target = parse_typed(trimmed, &current).ok()?;
        Some(PruneTest {
            column: c.column,
            op,
            target,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PruneOp {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
}

struct PruneTest {
    column: usize,
    op: PruneOp,
    target: TypedValue,
}

impl PruneTest {
    /// Whether the row group's [min, max] makes any match impossible.
    fn impossible(&self, range: &StatsRange) -> bool {
        use std::cmp::Ordering;
        // compare_typed falls back to Equal across variants, which would
        // corrupt range pruning — require the exact same variant.
        if std::mem::discriminant(&self.target) != std::mem::discriminant(&range.min) {
            return false;
        }
        let min = compare_typed(&range.min, &self.target);
        let max = compare_typed(&range.max, &self.target);
        match self.op {
            PruneOp::Eq => min == Ordering::Greater || max == Ordering::Less,
            PruneOp::Gt => max != Ordering::Greater,
            PruneOp::Gte => max == Ordering::Less,
            PruneOp::Lt => min != Ordering::Less,
            PruneOp::Lte => min == Ordering::Greater,
        }
    }
}

/// Conditions that are REQUIRED for a row to match: every condition reachable
/// from the root through `And` groups (an `Or` group with a single node is
/// equivalent to `And`). OR branches contribute nothing (conservative).
fn required_conditions<'a>(group: &'a FilterGroup, out: &mut Vec<&'a FilterCondition>) {
    if group.conjunction == Conjunction::Or && group.nodes.len() != 1 {
        return;
    }
    for node in &group.nodes {
        match node {
            FilterNode::Condition(c) => out.push(c),
            FilterNode::Group(sub) => required_conditions(sub, out),
        }
    }
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// One column as reported by [`inspect`] (wire DTO, camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedColumn {
    /// Flattened path-based name.
    pub name: String,
    /// The arrow type, for display (`Int64`, `Timestamp(µs, Europe/Berlin)`…).
    pub arrow_type: String,
    /// The F31 logical type the column maps to.
    pub logical_type: LogicalType,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Whether this column came out of a nested field (struct flattening or
    /// a complex-field policy).
    pub nested: bool,
}

/// Everything the open dialog needs BEFORE opening (wire DTO, camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnarInspection {
    pub format: String,
    pub row_count: u64,
    /// Parquet row groups / Arrow record batches.
    pub chunk_count: u64,
    /// Distinct parquet compression codecs, `None` for Arrow IPC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,
    /// Columns under the DEFAULT policies (complex fields preserved as JSON).
    pub columns: Vec<InspectedColumn>,
    /// Flattened paths of complex fields that take a [`ComplexPolicy`].
    pub complex_fields: Vec<String>,
    /// Rough bytes the fully editable in-memory document would need.
    pub estimated_memory: u64,
    /// Whether opening editable should require an explicit choice.
    pub needs_decision: bool,
    pub file_size: u64,
}

fn arrow_type_label(field_type: &DataType) -> String {
    format!("{field_type}")
}

fn estimate_memory(n_rows: u64, n_cols: u64, data_bytes: u64) -> u64 {
    data_bytes + n_rows * n_cols * CELL_OVERHEAD + n_rows * ROW_OVERHEAD
}

/// Shared open-time scan: schema + per-chunk row counts (+ parquet metadata).
struct Scanned {
    schema: ArrowSchema,
    chunk_rows: Vec<usize>,
    data_bytes: u64,
    compression: Option<String>,
    parquet_meta: Option<std::sync::Arc<ParquetMetaData>>,
}

fn scan_source(path: &Path, format: ColumnarFormat, ctx: Option<&JobCtx>) -> AppResult<Scanned> {
    if let Some(ctx) = ctx {
        ctx.check()?;
    }
    match format {
        ColumnarFormat::Parquet => {
            let file = File::open(path)?;
            let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(arrow_err)?;
            let schema = builder.schema().as_ref().clone();
            let meta = builder.metadata().clone();
            let mut chunk_rows = Vec::with_capacity(meta.num_row_groups());
            let mut data_bytes = 0u64;
            let mut codecs: Vec<String> = Vec::new();
            for rg in meta.row_groups() {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
                chunk_rows.push(usize::try_from(rg.num_rows()).unwrap_or(0));
                data_bytes = data_bytes.saturating_add(rg.total_byte_size().max(0) as u64);
                for col in rg.columns() {
                    let name = format!("{}", col.compression());
                    if !codecs.contains(&name) {
                        codecs.push(name);
                    }
                }
            }
            Ok(Scanned {
                schema,
                chunk_rows,
                data_bytes,
                compression: if codecs.is_empty() {
                    None
                } else {
                    Some(codecs.join(", "))
                },
                parquet_meta: Some(meta),
            })
        }
        ColumnarFormat::ArrowFile | ColumnarFormat::ArrowStream => {
            // One pass over the batches for per-chunk row counts, decoding a
            // single projected column to keep it cheap.
            let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let (schema, chunk_rows) = if format == ColumnarFormat::ArrowFile {
                let full = IpcFileReader::try_new(BufReader::new(File::open(path)?), None)
                    .map_err(arrow_err)?;
                let schema = full.schema().as_ref().clone();
                drop(full);
                let reader =
                    IpcFileReader::try_new(BufReader::new(File::open(path)?), Some(vec![0]))
                        .map_err(arrow_err)?;
                let mut rows = Vec::new();
                for batch in reader {
                    if let Some(ctx) = ctx {
                        ctx.check()?;
                    }
                    rows.push(batch.map_err(arrow_err)?.num_rows());
                }
                (schema, rows)
            } else {
                let reader =
                    IpcStreamReader::try_new(BufReader::new(File::open(path)?), Some(vec![0]))
                        .map_err(arrow_err)?;
                let schema = reader.schema().as_ref().clone();
                let mut rows = Vec::new();
                for batch in reader {
                    if let Some(ctx) = ctx {
                        ctx.check()?;
                    }
                    rows.push(batch.map_err(arrow_err)?.num_rows());
                }
                (schema, rows)
            };
            if schema.fields().is_empty() {
                return Err(AppError::invalid("the file has no columns"));
            }
            Ok(Scanned {
                schema,
                chunk_rows,
                data_bytes: file_size,
                compression: None,
                parquet_meta: None,
            })
        }
    }
}

/// Inspect a columnar file: format, row/chunk counts, columns mapped to F31
/// logical types, compression, nested fields and the editable-memory
/// estimate — everything the open dialog shows BEFORE any open.
pub fn inspect(path: &Path, ctx: Option<&JobCtx>) -> AppResult<ColumnarInspection> {
    let format = detect_format(path)?;
    let scanned = scan_source(path, format, ctx)?;
    let options = ColumnarOpenOptions::default();
    let projection = build_projection(&scanned.schema, &options, false)?;
    let n_rows: usize = scanned.chunk_rows.iter().sum();
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let estimated_memory = estimate_memory(
        n_rows as u64,
        projection.cols.len() as u64,
        scanned.data_bytes,
    );

    let columns = projection
        .cols
        .iter()
        .map(|col| InspectedColumn {
            name: col.name.clone(),
            arrow_type: arrow_type_label(&col.data_type),
            logical_type: col.logical,
            nullable: col.nullable,
            time_zone: col.time_zone.clone(),
            nested: col.steps.len() > 1 || col.kind == OutKind::Json,
        })
        .collect();
    let complex_fields = projection
        .cols
        .iter()
        .filter(|c| c.kind == OutKind::Json)
        .map(|c| c.name.clone())
        .collect();

    Ok(ColumnarInspection {
        format: format.wire_name().to_string(),
        row_count: n_rows as u64,
        chunk_count: scanned.chunk_rows.len() as u64,
        compression: scanned.compression,
        columns,
        complex_fields,
        estimated_memory,
        needs_decision: estimated_memory > index::MEMORY_DECISION_THRESHOLD
            || file_size > index::SIZE_DECISION_THRESHOLD,
        file_size,
    })
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

/// Everything [`crate::document::Document::from_columnar`] needs.
#[derive(Debug)]
pub struct ColumnarFile {
    pub handle: ColumnarHandle,
    pub headers: Vec<String>,
    /// Parallel to `headers`; column IDs are positional `c{i}` placeholders
    /// that match the document's initial ID assignment.
    pub schemas: Vec<ColumnSchema>,
}

/// Open a columnar file as an indexed READ-ONLY backing. Explode policies are
/// rejected here (they change the row count); use [`open_editable_rows`].
/// Creates no on-disk caches — cancellation simply abandons the handle.
pub fn open_indexed(
    path: &Path,
    options: &ColumnarOpenOptions,
    ctx: Option<&JobCtx>,
) -> AppResult<ColumnarFile> {
    let format = detect_format(path)?;
    let scanned = scan_source(path, format, ctx)?;
    let projection = build_projection(&scanned.schema, options, false)?;
    let schemas = column_schemas(&projection.cols);
    let headers: Vec<String> = projection.cols.iter().map(|c| c.name.clone()).collect();

    let mut chunk_starts = Vec::with_capacity(scanned.chunk_rows.len() + 1);
    let mut total = 0usize;
    chunk_starts.push(0);
    for &rows in &scanned.chunk_rows {
        total += rows;
        chunk_starts.push(total);
    }
    let stats = scanned
        .parquet_meta
        .as_deref()
        .and_then(|meta| extract_stats(meta, &projection.cols, &projection));
    if let Some(ctx) = ctx {
        ctx.check()?;
    }

    let editable_estimate = estimate_memory(
        total as u64,
        projection.cols.len() as u64,
        scanned.data_bytes,
    );
    let budget = if options.cache_budget_bytes == 0 {
        DEFAULT_CACHE_BUDGET
    } else {
        options.cache_budget_bytes
    };
    let handle = ColumnarHandle {
        path: path.to_path_buf(),
        format,
        cols: projection.cols,
        schemas: schemas.clone(),
        chunk_starts,
        n_rows: total,
        source_check: util::stat_fingerprint(path),
        editable_estimate,
        stats,
        cache: Mutex::new(BlockCache::new(budget)),
    };
    Ok(ColumnarFile {
        handle,
        headers,
        schemas,
    })
}

// ---------------------------------------------------------------------------
// Editable materialisation (convert-to-editable + explode opens)
// ---------------------------------------------------------------------------

/// A fully materialised editable table: text rows plus the schemas whose
/// null tokens carry the null-vs-empty distinction into the text plane.
#[derive(Debug)]
pub struct EditableTable {
    pub headers: Vec<String>,
    /// Parallel to `headers`; positional `c{i}` column IDs. Columns that
    /// contain at least one NULL get a collision-free null token.
    pub schemas: Vec<ColumnSchema>,
    pub rows: Vec<Vec<String>>,
}

/// Deterministic per-column null token: the first of `NULL`, `NULL#1`,
/// `NULL#2`, … whose trimmed form never appears among the column's actual
/// (trimmed) values, so [`crate::schema::is_null_token`] can never
/// misclassify a real value.
fn pick_null_token(present: &dyn Fn(&str) -> bool) -> String {
    if !present("NULL") {
        return "NULL".to_string();
    }
    let mut n = 1u64;
    loop {
        let candidate = format!("NULL#{n}");
        if !present(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Turn `Option` rows into token-rendered text rows, assigning null tokens
/// to the schemas of columns that actually contain nulls.
fn tokenize_rows(
    rows_opt: Vec<Vec<Option<String>>>,
    schemas: &mut [ColumnSchema],
) -> Vec<Vec<String>> {
    let n_cols = schemas.len();
    let mut has_null = vec![false; n_cols];
    for row in &rows_opt {
        for (c, cell) in row.iter().enumerate() {
            if cell.is_none() {
                has_null[c] = true;
            }
        }
    }
    let mut tokens: Vec<Option<String>> = vec![None; n_cols];
    for c in 0..n_cols {
        if !has_null[c] {
            continue;
        }
        let present = |candidate: &str| {
            rows_opt.iter().any(|row| {
                row.get(c)
                    .and_then(|cell| cell.as_deref())
                    .is_some_and(|v| v.trim() == candidate)
            })
        };
        let token = pick_null_token(&present);
        schemas[c].null_tokens = vec![token.clone()];
        tokens[c] = Some(token);
    }
    rows_opt
        .into_iter()
        .map(|row| {
            row.into_iter()
                .enumerate()
                .map(|(c, cell)| match cell {
                    Some(v) => v,
                    None => tokens[c].clone().unwrap_or_default(),
                })
                .collect()
        })
        .collect()
}

/// Materialise an open indexed handle into editable rows + schemas (the
/// convert-to-editable payload). The caller is responsible for the explicit
/// memory check ([`ColumnarHandle::convert_needs_decision`]) and for applying
/// the result under a revision guard.
pub fn plan_editable(handle: &ColumnarHandle, ctx: Option<&JobCtx>) -> AppResult<EditableTable> {
    if let Some(ctx) = ctx {
        ctx.set_total(handle.n_rows() as u64);
    }
    let mut rows_opt: Vec<Vec<Option<String>>> = Vec::with_capacity(handle.n_rows());
    let mut offset = 0u64;
    loop {
        let batch = handle.read_optional(offset, crate::tabular::DEFAULT_WINDOW, ctx)?;
        if batch.is_empty() {
            break;
        }
        let n = batch.len() as u64;
        offset += n;
        rows_opt.extend(batch);
        if let Some(ctx) = ctx {
            ctx.advance(n)?;
        }
    }
    let mut schemas = handle.schemas().to_vec();
    let rows = tokenize_rows(rows_opt, &mut schemas);
    Ok(EditableTable {
        headers: handle.headers(),
        schemas,
        rows,
    })
}

/// Open a columnar file straight to editable rows, honouring ALL policies
/// including exploding one list column into rows. The caller does the
/// explicit memory check first (via [`inspect`]).
pub fn open_editable_rows(
    path: &Path,
    options: &ColumnarOpenOptions,
    ctx: Option<&JobCtx>,
) -> AppResult<EditableTable> {
    let format = detect_format(path)?;
    let scanned = scan_source(path, format, ctx)?;
    let projection = build_projection(&scanned.schema, options, true)?;
    let mut schemas = column_schemas(&projection.cols);
    let headers: Vec<String> = projection.cols.iter().map(|c| c.name.clone()).collect();
    let n_rows: usize = scanned.chunk_rows.iter().sum();
    if let Some(ctx) = ctx {
        ctx.set_total(n_rows as u64);
    }

    let mut rows_opt: Vec<Vec<Option<String>>> = Vec::with_capacity(n_rows);
    let mut push_batch = |batch: &RecordBatch| -> AppResult<()> {
        render_batch_exploded(&projection.cols, batch, &mut rows_opt)
    };
    match format {
        ColumnarFormat::Parquet => {
            let file = File::open(path)?;
            let reader = ParquetRecordBatchReaderBuilder::try_new(file)
                .map_err(arrow_err)?
                .with_batch_size(BLOCK_ROWS)
                .build()
                .map_err(arrow_err)?;
            for batch in reader {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
                let batch = batch.map_err(arrow_err)?;
                let rows = batch.num_rows() as u64;
                push_batch(&batch)?;
                if let Some(ctx) = ctx {
                    ctx.advance(rows)?;
                }
            }
        }
        ColumnarFormat::ArrowFile => {
            let reader = IpcFileReader::try_new(BufReader::new(File::open(path)?), None)
                .map_err(arrow_err)?;
            for batch in reader {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
                let batch = batch.map_err(arrow_err)?;
                let rows = batch.num_rows() as u64;
                push_batch(&batch)?;
                if let Some(ctx) = ctx {
                    ctx.advance(rows)?;
                }
            }
        }
        ColumnarFormat::ArrowStream => {
            let reader = IpcStreamReader::try_new(BufReader::new(File::open(path)?), None)
                .map_err(arrow_err)?;
            for batch in reader {
                if let Some(ctx) = ctx {
                    ctx.check()?;
                }
                let batch = batch.map_err(arrow_err)?;
                let rows = batch.num_rows() as u64;
                push_batch(&batch)?;
                if let Some(ctx) = ctx {
                    ctx.advance(rows)?;
                }
            }
        }
    }

    let rows = tokenize_rows(rows_opt, &mut schemas);
    Ok(EditableTable {
        headers,
        schemas,
        rows,
    })
}

/// Like [`render_batch`], but honouring an Explode column: each source
/// record becomes one output row PER LIST ELEMENT (deterministic order);
/// null and empty lists both yield a single row with a null cell.
fn render_batch_exploded(
    cols: &[OutCol],
    batch: &RecordBatch,
    out: &mut Vec<TabularRow>,
) -> AppResult<()> {
    let explode_at = cols.iter().position(|c| c.kind == OutKind::Explode);
    let Some(explode_at) = explode_at else {
        return render_batch(cols, batch, out);
    };
    let cursors: Vec<LeafCursor> = cols
        .iter()
        .map(|c| LeafCursor::new(c, batch))
        .collect::<AppResult<_>>()?;
    for row in 0..batch.num_rows() {
        let mut base = Vec::with_capacity(cols.len());
        for (c, (col, cursor)) in cols.iter().zip(&cursors).enumerate() {
            if c == explode_at {
                base.push(None); // placeholder
            } else {
                base.push(cursor.cell(col, row)?);
            }
        }
        let elements = explode_elements(&cursors[explode_at], row)?;
        match elements {
            None => out.push(base),
            Some(elems) if elems.is_empty() => out.push(base),
            Some(elems) => {
                for element in elems {
                    let mut cloned = base.clone();
                    cloned[explode_at] = element;
                    out.push(cloned);
                }
            }
        }
    }
    Ok(())
}

/// The exploded cells for one record: `None` for a null list (or a null
/// ancestor), `Some(vec![...])` with one rendered cell per element.
fn explode_elements(cursor: &LeafCursor, row: usize) -> AppResult<Option<Vec<Option<String>>>> {
    for parent in &cursor.parents {
        if parent.is_null(row) {
            return Ok(None);
        }
    }
    let leaf = cursor.leaf.as_ref();
    if leaf.is_null(row) {
        return Ok(None);
    }
    let elems: ArrayRef = match leaf.data_type() {
        DataType::List(_) => leaf
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(row),
        DataType::LargeList(_) => leaf
            .as_any()
            .downcast_ref::<LargeListArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(row),
        DataType::FixedSizeList(_, _) => leaf
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .ok_or_else(|| arrow_err("array type mismatch"))?
            .value(row),
        _ => return Err(arrow_err("explode target is not a list")),
    };
    let mut out = Vec::with_capacity(elems.len());
    for e in 0..elems.len() {
        if is_primitive(elems.data_type()) {
            out.push(render_primitive(elems.as_ref(), e)?);
        } else if elems.is_null(e) {
            out.push(None);
        } else {
            let value = json_at(elems.as_ref(), e)?;
            out.push(Some(serde_json::to_string(&value).map_err(|err| {
                arrow_err(format!("JSON encoding failed: {err}"))
            })?));
        }
    }
    Ok(Some(out))
}

// ---------------------------------------------------------------------------
// TabularSource adapter
// ---------------------------------------------------------------------------

/// [`TabularSource`] over an open [`ColumnarHandle`]: the `Option` plane —
/// columnar NULL is `None`, an empty string is `Some("")` — so export and
/// derived operations keep the distinction end to end.
pub struct ColumnarSource<'a> {
    handle: &'a ColumnarHandle,
}

impl<'a> ColumnarSource<'a> {
    pub fn new(handle: &'a ColumnarHandle) -> ColumnarSource<'a> {
        ColumnarSource { handle }
    }
}

impl TabularSource for ColumnarSource<'_> {
    fn columns(&self) -> Vec<TabularColumn> {
        self.handle.tabular_columns()
    }

    fn row_count(&self) -> RowCountHint {
        RowCountHint::Exact(self.handle.n_rows() as u64)
    }

    fn read_rows(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>> {
        self.handle.read_optional(offset, limit, ctx)
    }

    fn fingerprint(&self) -> ContentFingerprint {
        match self.handle.source_check {
            Some(fp) => ContentFingerprint::File(fp),
            None => ContentFingerprint::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow::array::{
        ArrayRef, BooleanArray, Date32Array, Decimal128Array, DictionaryArray, Int64Array,
        Int64Builder, ListArray, MapBuilder, StringArray, StringBuilder, TimestampMicrosecondArray,
        UInt64Array,
    };
    use arrow::buffer::NullBuffer;
    use arrow::datatypes::{Field, Fields, Int64Type, Int8Type, Schema};
    use arrow::ipc::writer::{FileWriter, StreamWriter};
    use chrono::NaiveDate;
    use parquet::arrow::ArrowWriter;
    use parquet::basic::Compression;
    use parquet::file::properties::WriterProperties;

    use crate::document::Document;
    use crate::dto::{FilterCondition, FilterGroup, FilterNode, FilterOp};
    use crate::job::JobRegistry;
    use crate::schema::{classify, CellState};
    use crate::tabular::DocumentSource;

    // ----- builders --------------------------------------------------------

    fn batch_of(cols: Vec<(&str, ArrayRef)>) -> RecordBatch {
        let fields: Vec<Field> = cols
            .iter()
            .map(|(n, a)| Field::new(*n, a.data_type().clone(), true))
            .collect();
        let schema = Arc::new(Schema::new(fields));
        RecordBatch::try_new(schema, cols.into_iter().map(|(_, a)| a).collect()).unwrap()
    }

    fn write_parquet_with(
        path: &Path,
        batches: &[RecordBatch],
        row_group_size: usize,
        compression: Compression,
    ) {
        let props = WriterProperties::builder()
            .set_max_row_group_row_count(Some(row_group_size))
            .set_compression(compression)
            .build();
        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, batches[0].schema(), Some(props)).unwrap();
        for batch in batches {
            writer.write(batch).unwrap();
        }
        writer.close().unwrap();
    }

    fn write_parquet(path: &Path, batch: &RecordBatch) {
        write_parquet_with(
            path,
            std::slice::from_ref(batch),
            1024 * 1024,
            Compression::SNAPPY,
        );
    }

    fn write_ipc_file(path: &Path, batches: &[RecordBatch]) {
        let file = File::create(path).unwrap();
        let mut writer = FileWriter::try_new(file, batches[0].schema().as_ref()).unwrap();
        for batch in batches {
            writer.write(batch).unwrap();
        }
        writer.finish().unwrap();
    }

    fn write_ipc_stream(path: &Path, batches: &[RecordBatch]) {
        let file = File::create(path).unwrap();
        let mut writer = StreamWriter::try_new(file, batches[0].schema().as_ref()).unwrap();
        for batch in batches {
            writer.write(batch).unwrap();
        }
        writer.finish().unwrap();
    }

    fn open(path: &Path) -> ColumnarFile {
        open_indexed(path, &ColumnarOpenOptions::default(), None).unwrap()
    }

    fn all_optional(handle: &ColumnarHandle) -> Vec<TabularRow> {
        handle.read_optional(0, handle.n_rows(), None).unwrap()
    }

    fn all_text(handle: &ColumnarHandle) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        handle
            .visit(0..handle.n_rows(), &mut |_, row| {
                out.push(row.to_vec());
                Ok(true)
            })
            .unwrap();
        out
    }

    fn cancelled_ctx(registry: &JobRegistry) -> JobCtx {
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        ctx
    }

    fn days(y: i32, m: u32, d: u32) -> i32 {
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        (NaiveDate::from_ymd_opt(y, m, d).unwrap() - epoch).num_days() as i32
    }

    fn cond(column: usize, op: FilterOp, value: &str) -> FilterNode {
        FilterNode::Condition(FilterCondition {
            column,
            op,
            value: value.to_string(),
            case_sensitive: false,
        })
    }

    fn and_group(nodes: Vec<FilterNode>) -> FilterGroup {
        FilterGroup {
            conjunction: Conjunction::And,
            nodes,
        }
    }

    // ----- type preservation matrix ----------------------------------------

    #[test]
    fn type_matrix_round_trips_extremes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.parquet");
        let ts = NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_micro_opt(3, 4, 5, 123_456)
            .unwrap();
        let batch = batch_of(vec![
            ("i", Arc::new(Int64Array::from(vec![i64::MAX, i64::MIN, 0]))),
            ("u", Arc::new(UInt64Array::from(vec![u64::MAX, 0, 7]))),
            ("f", Arc::new(Float64Array::from(vec![0.1, -1.5e300, 2.0]))),
            (
                "dec",
                Arc::new(
                    Decimal128Array::from(vec![150i128, -5, 0])
                        .with_precision_and_scale(12, 2)
                        .unwrap(),
                ),
            ),
            ("b", Arc::new(BooleanArray::from(vec![true, false, true]))),
            (
                "d",
                Arc::new(Date32Array::from(vec![
                    days(2024, 1, 1),
                    0,
                    days(1969, 12, 31),
                ])),
            ),
            (
                "ts",
                Arc::new(TimestampMicrosecondArray::from(vec![
                    ts.and_utc().timestamp_micros(),
                    0,
                    -1,
                ])),
            ),
            (
                "s",
                Arc::new(StringArray::from(vec!["x", "", "long text ünïcode"])),
            ),
            (
                "bin",
                Arc::new(BinaryArray::from_opt_vec(vec![
                    Some(b"\x0a\xff".as_ref()),
                    Some(b"".as_ref()),
                    None,
                ])),
            ),
        ]);
        write_parquet(&path, &batch);

        let file = open(&path);
        assert_eq!(
            file.headers,
            ["i", "u", "f", "dec", "b", "d", "ts", "s", "bin"]
        );
        let logicals: Vec<LogicalType> = file.schemas.iter().map(|s| s.logical_type).collect();
        assert_eq!(
            logicals,
            [
                LogicalType::Integer,
                LogicalType::Integer,
                LogicalType::Float,
                LogicalType::Decimal,
                LogicalType::Boolean,
                LogicalType::Date,
                LogicalType::Datetime,
                LogicalType::Text,
                LogicalType::Text,
            ]
        );

        let text = all_text(&file.handle);
        assert_eq!(text[0][0], i64::MAX.to_string());
        assert_eq!(text[1][0], i64::MIN.to_string());
        assert_eq!(text[0][1], u64::MAX.to_string(), "u64::MAX lossless");
        assert_eq!(text[0][2], "0.1");
        assert_eq!(text[0][3], "1.50", "decimal keeps its scale");
        assert_eq!(text[1][3], "-0.05");
        assert_eq!(text[2][3], "0.00");
        assert_eq!(text[0][4], "true");
        assert_eq!(text[0][5], "2024-01-01");
        assert_eq!(text[2][5], "1969-12-31");
        assert_eq!(text[0][6], "2024-01-02T03:04:05.123456");
        assert_eq!(text[1][6], "1970-01-01T00:00:00");
        assert_eq!(text[2][6], "1969-12-31T23:59:59.999999");
        assert_eq!(text[2][7], "long text ünïcode");
        assert_eq!(text[0][8], "0aff", "binary renders as lowercase hex");

        // Every rendered value classifies as VALID under its generated
        // schema — the text plane is canonical, so it round-trips.
        for row in &text {
            for (c, cell) in row.iter().enumerate() {
                if cell.is_empty() {
                    continue;
                }
                let state = classify(Some(cell), &file.schemas[c]);
                assert!(
                    matches!(state, CellState::Valid(_)),
                    "column {c} cell {cell:?} classified {state:?}"
                );
            }
        }

        // The integer extremes survive typed classification exactly.
        let CellState::Valid(TypedValue::Integer(v)) =
            classify(Some(&text[0][1]), &file.schemas[1])
        else {
            panic!("expected valid integer");
        };
        assert_eq!(v, i128::from(u64::MAX));
    }

    // ----- null vs empty ----------------------------------------------------

    #[test]
    fn null_vs_empty_string_round_trip_distinctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.parquet");
        let batch = batch_of(vec![
            (
                "s",
                Arc::new(StringArray::from(vec![Some(""), None, Some("x")])),
            ),
            (
                "i",
                Arc::new(Int64Array::from(vec![Some(5), None, Some(6)])),
            ),
        ]);
        write_parquet(&path, &batch);
        let file = open(&path);

        // Option plane: NULL is None, empty string is Some("").
        let rows = all_optional(&file.handle);
        assert_eq!(rows[0][0], Some(String::new()));
        assert_eq!(rows[1][0], None);
        assert_eq!(rows[2][0], Some("x".to_string()));
        assert_eq!(rows[1][1], None);

        // Text plane: both render as an empty cell (grid display).
        let text = all_text(&file.handle);
        assert_eq!(text[0][0], "");
        assert_eq!(text[1][0], "");

        // The document-level tabular source keeps the distinction, so the
        // export path sees the real null bit.
        let doc = Document::from_columnar(1, Some(path.clone()), file);
        let source = DocumentSource::new(&doc);
        let rows = source.read_rows(0, 10, None).unwrap();
        assert_eq!(rows[0][0], Some(String::new()));
        assert_eq!(rows[1][0], None);
        assert!(source.has_header_row());
    }

    // ----- timestamps & time zones ------------------------------------------

    #[test]
    fn timestamp_time_zone_metadata_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tz.parquet");
        let ts = NaiveDate::from_ymd_opt(2024, 6, 1)
            .unwrap()
            .and_hms_micro_opt(12, 30, 0, 250_000)
            .unwrap();
        let micros = ts.and_utc().timestamp_micros();
        let batch = batch_of(vec![
            (
                "zoned",
                Arc::new(
                    TimestampMicrosecondArray::from(vec![micros]).with_timezone("Europe/Berlin"),
                ),
            ),
            (
                "naive",
                Arc::new(TimestampMicrosecondArray::from(vec![micros])),
            ),
        ]);
        write_parquet(&path, &batch);
        let file = open(&path);

        assert_eq!(
            file.schemas[0].time_zone.as_deref(),
            Some("Europe/Berlin"),
            "tz metadata maps to ColumnSchema.timeZone"
        );
        assert_eq!(file.schemas[1].time_zone, None);
        assert_eq!(
            file.schemas[1].input_formats,
            Some(vec!["%Y-%m-%dT%H:%M:%S%.f".to_string()])
        );

        let text = all_text(&file.handle);
        // Zoned: the UTC instant with an explicit Z (RFC 3339).
        assert_eq!(text[0][0], "2024-06-01T12:30:00.250Z");
        // Naive: plain ISO wall time.
        assert_eq!(text[0][1], "2024-06-01T12:30:00.250");

        // Both classify back to the exact stored instant.
        for (c, (cell, schema)) in text[0].iter().zip(&file.schemas).enumerate() {
            let CellState::Valid(TypedValue::DateTime(parsed)) = classify(Some(cell), schema)
            else {
                panic!("column {c} did not classify as a datetime");
            };
            assert_eq!(parsed, ts, "column {c} round-trips the instant");
        }
    }

    // ----- nested structs ----------------------------------------------------

    #[test]
    fn struct_flattening_is_deterministic_and_escapes_dots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.parquet");
        let a: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), Some(2), Some(3)]));
        let bc: ArrayRef = Arc::new(StringArray::from(vec![Some("u"), Some("v"), Some("w")]));
        let child_fields = Fields::from(vec![
            Field::new("a", DataType::Int64, true),
            Field::new("b.c", DataType::Utf8, true),
        ]);
        // Row 1 has a NULL STRUCT: both flattened cells must be null even
        // though the child arrays hold values there.
        let validity = NullBuffer::from(vec![true, false, true]);
        let s: ArrayRef = Arc::new(StructArray::new(child_fields, vec![a, bc], Some(validity)));
        let batch = batch_of(vec![
            ("s", s),
            ("d", Arc::new(Int64Array::from(vec![9, 8, 7]))),
        ]);
        write_parquet(&path, &batch);

        let first = open(&path);
        let second = open(&path);
        assert_eq!(
            first.headers,
            ["s.a", "s.b\\.c", "d"],
            "stable escaped paths"
        );
        assert_eq!(first.headers, second.headers, "deterministic across opens");
        assert_eq!(
            first
                .schemas
                .iter()
                .map(|s| s.logical_type)
                .collect::<Vec<_>>(),
            second
                .schemas
                .iter()
                .map(|s| s.logical_type)
                .collect::<Vec<_>>(),
        );

        let rows = all_optional(&first.handle);
        assert_eq!(rows[0][0], Some("1".to_string()));
        assert_eq!(rows[1][0], None, "null struct nulls its children");
        assert_eq!(rows[1][1], None);
        assert_eq!(rows[1][2], Some("8".to_string()));
        assert_eq!(rows[2][1], Some("w".to_string()));
    }

    // ----- list/map policy matrix --------------------------------------------

    fn list_map_batch() -> RecordBatch {
        let list = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![
            Some(vec![Some(1), Some(2)]),
            Some(vec![]),
            None,
        ]);
        let mut map = MapBuilder::new(None, StringBuilder::new(), Int64Builder::new());
        map.keys().append_value("k");
        map.values().append_value(1);
        map.append(true).unwrap();
        map.append(true).unwrap(); // empty map
        map.append(false).unwrap(); // null map
        let map = map.finish();
        batch_of(vec![
            ("id", Arc::new(Int64Array::from(vec![10, 20, 30]))),
            ("l", Arc::new(list)),
            ("m", Arc::new(map)),
        ])
    }

    #[test]
    fn list_and_map_policies_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.parquet");
        write_parquet(&path, &list_map_batch());

        // Default: preserve as JSON. Null and empty stay distinct.
        let file = open(&path);
        assert_eq!(file.headers, ["id", "l", "m"]);
        assert_eq!(file.schemas[1].logical_type, LogicalType::Json);
        let rows = all_optional(&file.handle);
        assert_eq!(rows[0][1], Some("[1,2]".to_string()));
        assert_eq!(rows[1][1], Some("[]".to_string()));
        assert_eq!(rows[2][1], None, "null list is a null cell");
        assert_eq!(rows[0][2], Some("{\"k\":1}".to_string()));
        assert_eq!(rows[1][2], Some("{}".to_string()));
        assert_eq!(rows[2][2], None);

        // Reject drops the field from the schema deterministically.
        let mut options = ColumnarOpenOptions::default();
        options
            .field_policies
            .insert("l".to_string(), ComplexPolicy::Reject);
        let rejected = open_indexed(&path, &options, None).unwrap();
        assert_eq!(rejected.headers, ["id", "m"]);

        // Explode cannot re-number rows in an indexed backing.
        let mut options = ColumnarOpenOptions::default();
        options
            .field_policies
            .insert("l".to_string(), ComplexPolicy::Explode);
        let err = open_indexed(&path, &options, None).unwrap_err();
        assert!(err.to_string().contains("explode"), "{err}");

        // Exploding a MAP is rejected outright.
        let mut options = ColumnarOpenOptions::default();
        options
            .field_policies
            .insert("m".to_string(), ComplexPolicy::Explode);
        let err = open_editable_rows(&path, &options, None).unwrap_err();
        assert!(err.to_string().contains("list"), "{err}");
    }

    #[test]
    fn explode_multiplies_rows_in_the_editable_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.parquet");
        write_parquet(&path, &list_map_batch());

        let mut options = ColumnarOpenOptions::default();
        options
            .field_policies
            .insert("l".to_string(), ComplexPolicy::Explode);
        options
            .field_policies
            .insert("m".to_string(), ComplexPolicy::Reject);
        let table = open_editable_rows(&path, &options, None).unwrap();
        assert_eq!(table.headers, ["id", "l"]);
        // [1,2] explodes to two rows; the empty and null lists each yield one
        // row whose exploded cell is null (rendered as the column's token).
        let token = table.schemas[1].null_tokens.first().cloned().unwrap();
        assert_eq!(
            table.rows,
            vec![
                vec!["10".to_string(), "1".to_string()],
                vec!["10".to_string(), "2".to_string()],
                vec!["20".to_string(), token.clone()],
                vec!["30".to_string(), token],
            ]
        );

        // Two exploded lists are rejected (deterministic schemas only).
        let list2 = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![
            Some(vec![Some(1)]),
            Some(vec![Some(2)]),
            Some(vec![Some(3)]),
        ]);
        let path2 = dir.path().join("e2.parquet");
        let batch = list_map_batch();
        let batch2 = batch_of(vec![
            ("id", batch.column(0).clone()),
            ("l", batch.column(1).clone()),
            ("l2", Arc::new(list2)),
        ]);
        write_parquet(&path2, &batch2);
        let options = ColumnarOpenOptions {
            complex_policy: ComplexPolicy::Explode,
            ..Default::default()
        };
        let err = open_editable_rows(&path2, &options, None).unwrap_err();
        assert!(err.to_string().contains("one list column"), "{err}");
    }

    // ----- convert-to-editable ------------------------------------------------

    #[test]
    fn convert_to_editable_assigns_collision_free_null_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.parquet");
        let batch = batch_of(vec![
            (
                "s",
                Arc::new(StringArray::from(vec![
                    Some("NULL"),
                    None,
                    Some("x"),
                    Some(""),
                ])),
            ),
            (
                "i",
                Arc::new(Int64Array::from(vec![Some(1), Some(2), None, Some(4)])),
            ),
            ("clean", Arc::new(Int64Array::from(vec![1, 2, 3, 4]))),
        ]);
        write_parquet(&path, &batch);
        let file = open(&path);
        let plan = plan_editable(&file.handle, None).unwrap();

        // The literal string "NULL" exists in column s: the token escalates.
        assert_eq!(plan.schemas[0].null_tokens, vec!["NULL#1".to_string()]);
        assert_eq!(plan.rows[0][0], "NULL", "real value stays verbatim");
        assert_eq!(plan.rows[1][0], "NULL#1", "null renders as the token");
        assert_eq!(plan.rows[3][0], "", "empty string stays empty");
        // No collision in column i: the default token.
        assert_eq!(plan.schemas[1].null_tokens, vec!["NULL".to_string()]);
        assert_eq!(plan.rows[2][1], "NULL");
        // A column without nulls gets no token.
        assert!(plan.schemas[2].null_tokens.is_empty());

        // Applying the plan to the document keeps everything distinguishable.
        let mut doc = Document::from_columnar(1, Some(path.clone()), file);
        let ids: Vec<String> = doc.column_ids().to_vec();
        doc.make_editable(plan.rows).unwrap();
        for (i, mut schema) in plan.schemas.into_iter().enumerate() {
            schema.column_id = ids[i].clone();
            doc.set_column_schema(schema);
        }
        assert!(doc.is_editable());
        let s0 = doc.column_schema_at(0).unwrap();
        assert!(matches!(classify(Some("NULL#1"), s0), CellState::NullToken));
        assert!(matches!(classify(Some("NULL"), s0), CellState::Valid(_)));
        assert!(matches!(classify(Some(""), s0), CellState::Empty));
    }

    #[test]
    fn convert_memory_check_uses_the_columnar_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.parquet");
        let batch = batch_of(vec![("i", Arc::new(Int64Array::from_iter_values(0..100)))]);
        write_parquet(&path, &batch);
        let file = open(&path);
        assert!(file.handle.editable_estimate() > 0);
        assert!(
            !file.handle.convert_needs_decision(),
            "tiny file needs no gate"
        );
    }

    // ----- arrow IPC file & stream ---------------------------------------------

    #[test]
    fn arrow_ipc_file_and_stream_read_identically() {
        let dir = tempfile::tempdir().unwrap();
        let b1 = batch_of(vec![
            (
                "i",
                Arc::new(Int64Array::from(vec![Some(1), None, Some(3)])),
            ),
            (
                "s",
                Arc::new(StringArray::from(vec![Some("a"), Some(""), None])),
            ),
        ]);
        let b2 = batch_of(vec![
            (
                "i",
                Arc::new(Int64Array::from(vec![Some(4), Some(5), Some(6)])),
            ),
            (
                "s",
                Arc::new(StringArray::from(vec![Some("d"), Some("e"), Some("f")])),
            ),
        ]);
        let file_path = dir.path().join("t.arrow");
        let stream_path = dir.path().join("t.arrows");
        write_ipc_file(&file_path, &[b1.clone(), b2.clone()]);
        write_ipc_stream(&stream_path, &[b1, b2]);

        assert_eq!(
            detect_format(&file_path).unwrap(),
            ColumnarFormat::ArrowFile
        );
        assert_eq!(
            detect_format(&stream_path).unwrap(),
            ColumnarFormat::ArrowStream
        );

        let f = open(&file_path);
        let s = open(&stream_path);
        assert_eq!(f.handle.n_rows(), 6);
        assert_eq!(all_optional(&f.handle), all_optional(&s.handle));

        // A window crossing the batch boundary reads contiguously.
        let window = f.handle.read_optional(2, 3, None).unwrap();
        assert_eq!(window.len(), 3);
        assert_eq!(window[0][0], Some("3".to_string()));
        assert_eq!(window[1][0], Some("4".to_string()));
        // Offset past the end yields an empty batch (tabular contract).
        assert!(f.handle.read_optional(100, 5, None).unwrap().is_empty());
        // Null vs empty survives both containers.
        let rows = all_optional(&s.handle);
        assert_eq!(rows[1][0], None);
        assert_eq!(rows[1][1], Some(String::new()));
        assert_eq!(rows[2][1], None);
    }

    #[test]
    fn dictionary_columns_render_their_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.arrow");
        let dict: DictionaryArray<Int8Type> = vec![Some("a"), Some("b"), Some("a"), None]
            .into_iter()
            .collect();
        let batch = batch_of(vec![("k", Arc::new(dict))]);
        write_ipc_file(&path, &[batch]);
        let file = open(&path);
        assert_eq!(file.schemas[0].logical_type, LogicalType::Text);
        let rows = all_optional(&file.handle);
        assert_eq!(rows[0][0], Some("a".to_string()));
        assert_eq!(rows[1][0], Some("b".to_string()));
        assert_eq!(rows[3][0], None);
    }

    // ----- statistics pruning -----------------------------------------------

    /// 20 ordered rows in row groups of 4: groups [0..4), [4..8), … [16..20).
    fn stats_doc(dir: &tempfile::TempDir) -> Document {
        let path = dir.path().join("stats.parquet");
        let batch = batch_of(vec![
            ("n", Arc::new(Int64Array::from_iter_values(0..20))),
            (
                "d",
                Arc::new(Date32Array::from_iter_values(
                    (0..20).map(|i| days(2024, 1, 1) + i),
                )),
            ),
        ]);
        write_parquet_with(
            &path,
            std::slice::from_ref(&batch),
            4,
            Compression::UNCOMPRESSED,
        );
        let file = open(&path);
        Document::from_columnar(1, Some(path), file)
    }

    #[test]
    // The expected values genuinely ARE one-element range lists here.
    #[allow(clippy::single_range_in_vec_init)]
    fn row_group_stats_skip_groups_and_match_the_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        let doc = stats_doc(&dir);

        // Range on the int column: only the last group can match n > 15.
        let spec = and_group(vec![cond(0, FilterOp::Gt, "15")]);
        let ranges = doc.filter_scan_ranges(&spec).expect("pruning applies");
        assert_eq!(ranges, [16..20], "first four groups proven impossible");

        let matched = crate::filter::matching_rows(&doc, &spec).unwrap();
        assert_eq!(matched, vec![16, 17, 18, 19]);

        // Equality: only the group containing 6.
        let spec = and_group(vec![cond(0, FilterOp::Equals, "6")]);
        assert_eq!(doc.filter_scan_ranges(&spec).as_deref(), Some(&[4..8][..]));
        assert_eq!(crate::filter::matching_rows(&doc, &spec).unwrap(), vec![6]);

        // Date range prunes chronologically.
        let spec = and_group(vec![cond(1, FilterOp::Lt, "2024-01-03")]);
        assert_eq!(doc.filter_scan_ranges(&spec).as_deref(), Some(&[0..4][..]));
        assert_eq!(
            crate::filter::matching_rows(&doc, &spec).unwrap(),
            vec![0, 1]
        );

        // ACCEPTANCE: the filtered read returns exactly the same values as
        // an unfiltered full scan for the matching rows.
        let spec = and_group(vec![cond(0, FilterOp::Gte, "13")]);
        let matched = crate::filter::matching_rows(&doc, &spec).unwrap();
        let mut brute = Vec::new();
        let mut brute_values = Vec::new();
        doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
            if row[0].parse::<i64>().is_ok_and(|v| v >= 13) {
                brute.push(i);
                brute_values.push(row.to_vec());
            }
            Ok(true)
        })
        .unwrap();
        assert_eq!(matched, brute, "pruned scan finds identical rows");
        assert_eq!(
            doc.fetch_rows(&matched).unwrap(),
            brute_values,
            "identical values for matching rows"
        );
    }

    #[test]
    fn stats_pruning_falls_back_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = stats_doc(&dir);

        // OR specs contribute no required conditions.
        let spec = FilterGroup {
            conjunction: Conjunction::Or,
            nodes: vec![cond(0, FilterOp::Gt, "15"), cond(0, FilterOp::Lt, "2")],
        };
        assert_eq!(doc.filter_scan_ranges(&spec), None);
        assert_eq!(
            crate::filter::matching_rows(&doc, &spec).unwrap(),
            vec![0, 1, 16, 17, 18, 19]
        );

        // Text-op conditions are ineligible.
        let spec = and_group(vec![cond(0, FilterOp::Contains, "1")]);
        assert_eq!(doc.filter_scan_ranges(&spec), None);

        // A schema whose parsing moved since the open (locale change) makes
        // the stats domain untrustworthy: full scan, still correct.
        let mut changed = doc.column_schema_at(0).unwrap().clone();
        changed.locale = Some("de-DE".to_string());
        doc.set_column_schema(changed);
        let spec = and_group(vec![cond(0, FilterOp::Gt, "15")]);
        assert_eq!(doc.filter_scan_ranges(&spec), None);
        assert_eq!(
            crate::filter::matching_rows(&doc, &spec).unwrap(),
            vec![16, 17, 18, 19]
        );
    }

    // ----- document integration ------------------------------------------------

    #[test]
    fn columnar_documents_behave_like_indexed_read_only_documents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.parquet");
        let batch = batch_of(vec![
            ("a", Arc::new(Int64Array::from(vec![1, 2, 3, 4]))),
            ("b", Arc::new(StringArray::from(vec!["w", "x", "y", "z"]))),
        ]);
        write_parquet(&path, &batch);
        let file = open(&path);
        let mut doc = Document::from_columnar(7, Some(path), file);

        assert_eq!(doc.backing_name(), "indexedReadOnly");
        assert!(!doc.is_editable());
        assert!(matches!(doc.ensure_editable(), Err(AppError::ReadOnly)));
        assert_eq!(doc.n_rows(), 4);
        assert_eq!(doc.n_cols(), 2);
        assert_eq!(doc.headers(), ["a", "b"]);
        assert!(
            doc.has_header_row(),
            "real flattened names, never synthetic"
        );
        assert_eq!(
            doc.column_schema_at(0).map(|s| s.logical_type),
            Some(LogicalType::Integer)
        );

        // Random-access fetch in caller order (view sorts use this).
        let rows = doc.fetch_rows(&[2, 0, 2]).unwrap();
        assert_eq!(rows[0][1], "y");
        assert_eq!(rows[1][1], "w");
        assert_eq!(rows[2][1], "y");
        assert!(doc.fetch_rows(&[99]).is_err(), "out of range errors");

        // The filter view composes over the columnar backing.
        doc.set_filter(vec![1, 3]).unwrap();
        assert_eq!(doc.visible_len(), 2);
        let resp = doc.get_rows(0, 10).unwrap();
        assert_eq!(resp.rows.len(), 2);
        assert_eq!(resp.rows[0][1], "x");
        assert_eq!(resp.rows[1][1], "z");
    }

    // ----- cache bounds -----------------------------------------------------

    #[test]
    fn block_cache_stays_within_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.parquet");
        let n = (BLOCK_ROWS * 2 + 10) as i64;
        let batch = batch_of(vec![("i", Arc::new(Int64Array::from_iter_values(0..n)))]);
        write_parquet(&path, &batch);

        // A one-byte budget: every newly decoded block evicts the previous.
        let options = ColumnarOpenOptions {
            cache_budget_bytes: 1,
            ..Default::default()
        };
        let file = open_indexed(&path, &options, None).unwrap();
        let handle = &file.handle;
        for block in 0..3 {
            handle
                .read_optional((block * BLOCK_ROWS) as u64, 1, None)
                .unwrap();
            assert_eq!(handle.cached_blocks(), 1, "budget keeps exactly one block");
        }

        // The default budget comfortably holds all three blocks.
        let file = open(&path);
        let handle = &file.handle;
        let all = all_optional(handle);
        assert_eq!(all.len(), n as usize);
        assert_eq!(all[BLOCK_ROWS * 2 + 9][0], Some((n - 1).to_string()));
        assert_eq!(handle.cached_blocks(), 3);
        assert!(handle.cached_bytes() <= DEFAULT_CACHE_BUDGET);
    }

    // ----- cancellation -----------------------------------------------------

    #[test]
    fn cancel_stops_open_inspect_and_convert_without_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.parquet");
        let batch = batch_of(vec![("i", Arc::new(Int64Array::from_iter_values(0..64)))]);
        write_parquet(&path, &batch);
        let registry = JobRegistry::default();

        let before: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();

        let ctx = cancelled_ctx(&registry);
        assert!(matches!(
            open_indexed(&path, &ColumnarOpenOptions::default(), Some(&ctx)),
            Err(AppError::Cancelled)
        ));
        let ctx = cancelled_ctx(&registry);
        assert!(matches!(
            inspect(&path, Some(&ctx)),
            Err(AppError::Cancelled)
        ));
        let ctx = cancelled_ctx(&registry);
        assert!(matches!(
            open_editable_rows(&path, &ColumnarOpenOptions::default(), Some(&ctx)),
            Err(AppError::Cancelled)
        ));

        let file = open(&path);
        let ctx = cancelled_ctx(&registry);
        assert!(matches!(
            plan_editable(&file.handle, Some(&ctx)),
            Err(AppError::Cancelled)
        ));

        // The read side never writes: no caches or partial outputs appear.
        let after: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before, after, "cancellation leaves the directory untouched");
    }

    // ----- inspection ---------------------------------------------------------

    #[test]
    fn inspect_reports_shape_types_and_compression() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.parquet");
        let batch = batch_of(vec![
            ("n", Arc::new(Int64Array::from_iter_values(0..10))),
            (
                "l",
                Arc::new(ListArray::from_iter_primitive::<Int64Type, _, _>(vec![
                    Some(
                        vec![Some(1)]
                    );
                    10
                ])),
            ),
        ]);
        write_parquet_with(&path, std::slice::from_ref(&batch), 4, Compression::SNAPPY);

        let report = inspect(&path, None).unwrap();
        assert_eq!(report.format, "parquet");
        assert_eq!(report.row_count, 10);
        assert_eq!(report.chunk_count, 3, "10 rows in groups of 4");
        assert!(
            report
                .compression
                .as_deref()
                .unwrap_or("")
                .contains("SNAPPY"),
            "codec surfaces: {:?}",
            report.compression
        );
        assert_eq!(report.columns.len(), 2);
        assert_eq!(report.columns[0].logical_type, LogicalType::Integer);
        assert_eq!(report.columns[1].logical_type, LogicalType::Json);
        assert!(report.columns[1].nested);
        assert_eq!(report.complex_fields, vec!["l".to_string()]);
        assert!(report.estimated_memory > 0);
        assert!(!report.needs_decision);

        // Arrow IPC file: same surface, no compression, batch chunks.
        let arrow_path = dir.path().join("i.arrow");
        write_ipc_file(&arrow_path, &[batch.clone(), batch]);
        let report = inspect(&arrow_path, None).unwrap();
        assert_eq!(report.format, "arrowFile");
        assert_eq!(report.row_count, 20);
        assert_eq!(report.chunk_count, 2);
        assert_eq!(report.compression, None);
    }
}
