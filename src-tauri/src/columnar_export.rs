//! Parquet & Arrow interop (F32) — write side.
//!
//! Exports a document — ANY export scope from [`crate::export_scope`] (all
//! rows, the filtered view, selected rows/columns/range) — to Apache Parquet
//! (uncompressed / Snappy / Zstd), an Arrow IPC file (= Feather v2; keep the
//! alias in UI copy) or an Arrow IPC stream, through the F03 atomic-save
//! pipeline as a cancellable job.
//!
//! Typing rules (`typed`, the default):
//! - A column WITH a declared F31 schema exports as the arrow type its
//!   logical type maps to: integer → Int64 (or UInt64 when the scoped values
//!   include one beyond `i64::MAX` and none negative — a typing pre-pass over
//!   the scoped rows decides); decimal → Decimal128 with the smallest
//!   precision/scale covering every valid scoped cell (per-cell scales unify
//!   to the widest EXACTLY — `1.5` in a scale-2 column exports as mantissa
//!   `150`; a column needing more than 38 fractional digits falls back to
//!   Utf8); float → Float64; boolean → Boolean; date → Date32; datetime →
//!   Timestamp in microseconds — or nanoseconds when any scoped value
//!   carries sub-microsecond digits — with [`ColumnSchema::time_zone`]
//!   preserved as the arrow timezone (values are the UTC instants the schema
//!   parse already produces); text/uuid/json → Utf8, cell text verbatim.
//! - Schema null tokens export as columnar NULL. An empty cell in a
//!   non-text typed column is a null WITHOUT a warning (it means "no value",
//!   mirroring [`crate::schema::classify`]'s `Empty` state).
//! - A cell that fails to parse under its declared schema — or whose value
//!   the chosen arrow type cannot represent (`u64::MAX` in a column that
//!   also holds negatives, a decimal wider than the unified precision, a
//!   sub-microsecond timestamp outside the nanosecond range) — exports as
//!   NULL and counts into the per-column totals on the returned
//!   [`ColumnarExportReport`]. Nothing is ever substituted silently.
//! - Without a schema (or with `typed: false`) every column exports as Utf8
//!   text exactly as stored, so `007` stays `007`.
//!
//! Null vs empty string: scoped rows are read through the `Option` plane
//! ([`crate::tabular`] semantics — a columnar-backed document's NULL arrives
//! as `None`, an empty string as `Some("")`), so both survive distinctly:
//! `None` → columnar NULL, `Some("")` → an empty Utf8 string. Editable and
//! F10-indexed documents never produce `None`; their nulls are schema null
//! tokens.
//!
//! Compression applies to Parquet only (Arrow IPC always writes
//! uncompressed; the option is ignored there), and `row_group_rows` bounds
//! parquet row-group sizes so the read side's statistics pruning has groups
//! to skip.
//!
//! Cancellation/atomicity: the typing pre-pass and every batch write observe
//! the [`JobCtx`]; all bytes stream into the F03 staging file, so failure or
//! cancellation at any point removes the staging file and never touches an
//! existing destination.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use arrow::array::{
    ArrayRef, BooleanBuilder, Date32Builder, Decimal128Builder, Float64Builder, Int64Builder,
    StringBuilder, TimestampMicrosecondBuilder, TimestampNanosecondBuilder, UInt64Builder,
};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema, SchemaRef, TimeUnit};
use arrow::ipc::writer::{FileWriter as IpcFileWriter, StreamWriter as IpcStreamWriter};
use arrow::record_batch::RecordBatch;
use chrono::{NaiveDate, Timelike};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::dto::{BackupPolicy, ExportScope};
use crate::error::{AppError, AppResult};
use crate::export_scope::{self, ResolvedScope};
use crate::job::JobCtx;
use crate::parquet_arrow::ColumnarFormat;
use crate::save;
use crate::schema::{
    self, classify, CellState, ColumnSchema, DecimalValue, LogicalType, TypedValue,
};
use crate::tabular::TabularRow;

/// Rows per written record batch (matches the read side's decode block, so
/// an export → re-open round trip stays block-aligned).
const BATCH_ROWS: usize = 4096;

fn write_err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("columnar write error: {e}"))
}

// ---------------------------------------------------------------------------
// Options / report DTOs
// ---------------------------------------------------------------------------

/// Parquet compression codec choice (wire DTO, camelCase). Applies to the
/// Parquet format only; Arrow IPC output is always uncompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnarCompression {
    Uncompressed,
    #[default]
    Snappy,
    Zstd,
}

/// Options for [`run`] (wire DTO, camelCase).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColumnarExportOptions {
    /// Output container: Parquet, Arrow IPC file (Feather v2) or stream.
    pub format: ColumnarFormat,
    /// Parquet compression codec (ignored for the Arrow IPC formats).
    pub compression: ColumnarCompression,
    /// Emit typed arrow columns for columns with a declared F31 schema
    /// (module docs). `false` = every column as Utf8 text.
    pub typed: bool,
    /// Parquet only: maximum rows per row group (`0` = writer default).
    /// Smaller groups give the read side's statistics pruning more to skip.
    pub row_group_rows: usize,
    /// Backup policy for the previous destination file.
    pub backup: BackupPolicy,
}

impl Default for ColumnarExportOptions {
    fn default() -> ColumnarExportOptions {
        ColumnarExportOptions {
            format: ColumnarFormat::Parquet,
            compression: ColumnarCompression::default(),
            typed: true,
            row_group_rows: 0,
            backup: BackupPolicy::default(),
        }
    }
}

/// Per-column invalid-cell total on a finished export (wire DTO, camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnWarning {
    /// Output column name.
    pub name: String,
    /// Cells that exported as NULL because they could not be represented
    /// under the declared schema / chosen arrow type.
    pub invalid_cells: u64,
}

/// What a finished export produced (wire DTO, camelCase). Fetched by job id
/// after the `job-finished` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnarExportReport {
    /// Wire name of the container written (`parquet` / `arrowFile` /
    /// `arrowStream`).
    pub format: String,
    /// Data rows written.
    pub rows: u64,
    /// Columns written.
    pub columns: usize,
    /// Bytes written to the destination.
    pub bytes: u64,
    /// Total cells exported as NULL with a warning (module docs).
    pub invalid_cells: u64,
    /// Per-column breakdown (only columns with at least one warning).
    pub column_warnings: Vec<ColumnWarning>,
}

/// Finished export reports keyed by the job id that produced them (mirrors
/// the JSON-import preview cache).
#[derive(Default)]
pub struct ColumnarExportReportCache(Arc<Mutex<HashMap<u64, ColumnarExportReport>>>);

impl ColumnarExportReportCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, ColumnarExportReport>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, job_id: u64) -> Option<ColumnarExportReport> {
        self.0.lock().ok()?.get(&job_id).cloned()
    }
}

// ---------------------------------------------------------------------------
// Planning: scope + per-column arrow targets
// ---------------------------------------------------------------------------

/// The arrow type one output column writes.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Utf8,
    Int64,
    UInt64,
    Float64,
    Decimal { precision: u8, scale: u32 },
    Boolean,
    Date32,
    Timestamp { nanos: bool, tz: Option<String> },
}

/// One planned output column (parallel to `resolved.cols`, which carries
/// the absolute document column index).
struct PlannedColumn {
    name: String,
    /// The declared schema driving typed emission (`None` = plain Utf8).
    schema: Option<ColumnSchema>,
    target: Target,
}

struct ExportPlan {
    resolved: ResolvedScope,
    cols: Vec<PlannedColumn>,
}

/// Typing pre-pass aggregates for one column.
#[derive(Debug, Default)]
struct Probe {
    any_negative: bool,
    any_over_i64: bool,
    max_scale: u32,
    max_int_digits: usize,
    subsec: bool,
}

/// Resolve the scope and decide every column's arrow target. Columns whose
/// target depends on the data (integer width, decimal precision/scale,
/// timestamp unit) are decided by ONE typing pre-pass over the scoped rows;
/// everything else is decided from the schema alone. Sets the job total
/// (rows × passes) as a side effect.
fn plan(
    doc: &Document,
    options: &ColumnarExportOptions,
    scope: &ExportScope,
    ctx: &JobCtx,
) -> AppResult<ExportPlan> {
    let resolved = export_scope::resolve_scope(doc, scope)?;
    let headers = doc.headers();
    let mut cols: Vec<PlannedColumn> = Vec::with_capacity(resolved.cols.len());
    let mut probes: Vec<Option<Probe>> = Vec::with_capacity(resolved.cols.len());
    for &c in &resolved.cols {
        let schema = if options.typed {
            doc.column_schema_at(c).cloned()
        } else {
            None
        };
        let (target, probe) = match schema.as_ref().map(|s| s.logical_type) {
            Some(LogicalType::Integer) => (Target::Int64, Some(Probe::default())),
            Some(LogicalType::Decimal) => (
                Target::Decimal {
                    precision: 1,
                    scale: 0,
                },
                Some(Probe::default()),
            ),
            Some(LogicalType::Float) => (Target::Float64, None),
            Some(LogicalType::Boolean) => (Target::Boolean, None),
            Some(LogicalType::Date) => (Target::Date32, None),
            Some(LogicalType::Datetime) => (
                Target::Timestamp {
                    nanos: false,
                    tz: schema.as_ref().and_then(|s| s.time_zone.clone()),
                },
                Some(Probe::default()),
            ),
            // Text, Uuid and Json stay text, verbatim; no schema = text.
            _ => (Target::Utf8, None),
        };
        cols.push(PlannedColumn {
            name: headers[c].clone(),
            schema,
            target,
        });
        probes.push(probe);
    }

    let needs_prepass = probes.iter().any(Option::is_some);
    let passes = 1 + u64::from(needs_prepass);
    ctx.set_total(resolved.rows.len() as u64 * passes);

    if needs_prepass {
        stream_scoped(doc, &resolved, ctx, |chunk| {
            for row in chunk {
                for (i, probe) in probes.iter_mut().enumerate() {
                    let Some(probe) = probe.as_mut() else {
                        continue;
                    };
                    let Some(text) = row[i].as_deref() else {
                        continue;
                    };
                    let Some(schema) = cols[i].schema.as_ref() else {
                        continue;
                    };
                    let CellState::Valid(value) = classify(Some(text), schema) else {
                        continue;
                    };
                    match value {
                        TypedValue::Integer(v) => {
                            if v < 0 {
                                probe.any_negative = true;
                            }
                            if v > i128::from(i64::MAX) {
                                probe.any_over_i64 = true;
                            }
                        }
                        TypedValue::Decimal(d) => {
                            probe.max_scale = probe.max_scale.max(d.scale);
                            let int_digits = d.digits.len().saturating_sub(d.scale as usize);
                            probe.max_int_digits = probe.max_int_digits.max(int_digits);
                        }
                        TypedValue::DateTime(ndt) if ndt.nanosecond() % 1000 != 0 => {
                            probe.subsec = true;
                        }
                        _ => {}
                    }
                }
            }
            Ok(())
        })?;
        for (planned, probe) in cols.iter_mut().zip(&probes) {
            let Some(probe) = probe else { continue };
            planned.target = match &planned.target {
                Target::Int64 => {
                    if probe.any_over_i64 && !probe.any_negative {
                        Target::UInt64
                    } else {
                        Target::Int64
                    }
                }
                Target::Decimal { .. } => {
                    let scale = probe.max_scale;
                    if scale > 38 {
                        // Decimal128 cannot carry the fractional width; keep
                        // the exact text instead of rounding (module docs).
                        Target::Utf8
                    } else {
                        let precision = (probe.max_int_digits as u32 + scale).clamp(1, 38) as u8;
                        Target::Decimal { precision, scale }
                    }
                }
                Target::Timestamp { tz, .. } => Target::Timestamp {
                    nanos: probe.subsec,
                    tz: tz.clone(),
                },
                other => other.clone(),
            };
        }
    }

    Ok(ExportPlan { resolved, cols })
}

fn field_of(planned: &PlannedColumn) -> Field {
    let data_type = match &planned.target {
        Target::Utf8 => DataType::Utf8,
        Target::Int64 => DataType::Int64,
        Target::UInt64 => DataType::UInt64,
        Target::Float64 => DataType::Float64,
        Target::Decimal { precision, scale } => DataType::Decimal128(*precision, *scale as i8),
        Target::Boolean => DataType::Boolean,
        Target::Date32 => DataType::Date32,
        Target::Timestamp { nanos, tz } => DataType::Timestamp(
            if *nanos {
                TimeUnit::Nanosecond
            } else {
                TimeUnit::Microsecond
            },
            tz.clone().map(Arc::from),
        ),
    };
    Field::new(planned.name.clone(), data_type, true)
}

// ---------------------------------------------------------------------------
// Scoped Option-plane streaming
// ---------------------------------------------------------------------------

/// Stream the resolved rows, projected to the resolved columns, as bounded
/// chunks of `Option` cells. Columnar-backed documents read the handle's
/// `Option` plane (NULL = `None`) with consecutive scoped rows coalesced
/// into shared windowed reads; the text backings never produce `None`.
/// Advances the job by every row delivered (cancellation is observed there).
fn stream_scoped(
    doc: &Document,
    resolved: &ResolvedScope,
    ctx: &JobCtx,
    mut per_chunk: impl FnMut(Vec<TabularRow>) -> AppResult<()>,
) -> AppResult<()> {
    let cols = &resolved.cols;
    let mut buffer: Vec<TabularRow> = Vec::with_capacity(BATCH_ROWS.min(resolved.rows.len()));
    if let Some(handle) = doc.columnar_handle() {
        let rows = &resolved.rows;
        let mut i = 0usize;
        while i < rows.len() {
            // Coalesce a run of consecutive absolute rows into one read.
            let start = rows[i];
            let mut len = 1usize;
            while i + len < rows.len() && rows[i + len] == start + len && len < BATCH_ROWS {
                len += 1;
            }
            let batch = handle.read_optional(start as u64, len, Some(ctx))?;
            if batch.len() != len {
                return Err(AppError::Other(
                    "the source file changed on disk; reload the document".into(),
                ));
            }
            for row in batch {
                buffer.push(
                    cols.iter()
                        .map(|&c| row.get(c).cloned().flatten())
                        .collect(),
                );
                if buffer.len() >= BATCH_ROWS {
                    ctx.advance(buffer.len() as u64)?;
                    per_chunk(std::mem::take(&mut buffer))?;
                }
            }
            i += len;
        }
    } else {
        doc.visit_rows_at(&resolved.rows, &mut |_, row| {
            buffer.push(cols.iter().map(|&c| Some(row[c].clone())).collect());
            if buffer.len() >= BATCH_ROWS {
                ctx.advance(buffer.len() as u64)?;
                per_chunk(std::mem::take(&mut buffer))?;
            }
            Ok(true)
        })?;
    }
    if !buffer.is_empty() {
        ctx.advance(buffer.len() as u64)?;
        per_chunk(buffer)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cell → arrow value conversion
// ---------------------------------------------------------------------------

enum ColBuilder {
    Utf8(StringBuilder),
    Int64(Int64Builder),
    UInt64(UInt64Builder),
    Float64(Float64Builder),
    Decimal(Decimal128Builder),
    Boolean(BooleanBuilder),
    Date32(Date32Builder),
    TsMicro(TimestampMicrosecondBuilder),
    TsNano(TimestampNanosecondBuilder),
}

fn builder_for(target: &Target) -> ColBuilder {
    match target {
        Target::Utf8 => ColBuilder::Utf8(StringBuilder::new()),
        Target::Int64 => ColBuilder::Int64(Int64Builder::new()),
        Target::UInt64 => ColBuilder::UInt64(UInt64Builder::new()),
        Target::Float64 => ColBuilder::Float64(Float64Builder::new()),
        Target::Decimal { .. } => ColBuilder::Decimal(Decimal128Builder::new()),
        Target::Boolean => ColBuilder::Boolean(BooleanBuilder::new()),
        Target::Date32 => ColBuilder::Date32(Date32Builder::new()),
        Target::Timestamp { nanos: false, .. } => {
            ColBuilder::TsMicro(TimestampMicrosecondBuilder::new())
        }
        Target::Timestamp { nanos: true, .. } => {
            ColBuilder::TsNano(TimestampNanosecondBuilder::new())
        }
    }
}

impl ColBuilder {
    fn append_null(&mut self) {
        match self {
            ColBuilder::Utf8(b) => b.append_null(),
            ColBuilder::Int64(b) => b.append_null(),
            ColBuilder::UInt64(b) => b.append_null(),
            ColBuilder::Float64(b) => b.append_null(),
            ColBuilder::Decimal(b) => b.append_null(),
            ColBuilder::Boolean(b) => b.append_null(),
            ColBuilder::Date32(b) => b.append_null(),
            ColBuilder::TsMicro(b) => b.append_null(),
            ColBuilder::TsNano(b) => b.append_null(),
        }
    }
}

/// The exact Decimal128 mantissa of `d` rescaled to `scale` fraction digits,
/// when it fits (`None` = wider than i128, defensively also a cell whose own
/// scale exceeds the unified one).
fn decimal_mantissa(d: &DecimalValue, scale: u32) -> Option<i128> {
    if d.scale > scale {
        return None;
    }
    let mut mantissa: i128 = d.digits.parse().ok()?;
    for _ in 0..(scale - d.scale) {
        mantissa = mantissa.checked_mul(10)?;
    }
    Some(if d.negative { -mantissa } else { mantissa })
}

/// Append one typed value to its builder. `Err(())` = the chosen arrow type
/// cannot represent it (the caller writes null + counts a warning).
fn append_typed(builder: &mut ColBuilder, target: &Target, value: TypedValue) -> Result<(), ()> {
    match (builder, value) {
        (ColBuilder::Int64(b), TypedValue::Integer(v)) => match i64::try_from(v) {
            Ok(v) => {
                b.append_value(v);
                Ok(())
            }
            Err(_) => Err(()),
        },
        (ColBuilder::UInt64(b), TypedValue::Integer(v)) => match u64::try_from(v) {
            Ok(v) => {
                b.append_value(v);
                Ok(())
            }
            Err(_) => Err(()),
        },
        (ColBuilder::Float64(b), TypedValue::Float(v)) => {
            b.append_value(v);
            Ok(())
        }
        (ColBuilder::Decimal(b), TypedValue::Decimal(d)) => {
            let Target::Decimal { precision, scale } = target else {
                return Err(());
            };
            let mantissa = decimal_mantissa(&d, *scale).ok_or(())?;
            // 10^precision fits i128 for every legal precision (≤ 38).
            if mantissa.abs() >= 10i128.pow(u32::from(*precision)) {
                return Err(());
            }
            b.append_value(mantissa);
            Ok(())
        }
        (ColBuilder::Boolean(b), TypedValue::Boolean(v)) => {
            b.append_value(v);
            Ok(())
        }
        (ColBuilder::Date32(b), TypedValue::Date(d)) => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch is valid");
            match i32::try_from((d - epoch).num_days()) {
                Ok(days) => {
                    b.append_value(days);
                    Ok(())
                }
                Err(_) => Err(()),
            }
        }
        // Parse produced a UTC instant for zoned columns and a naive wall
        // time otherwise; arrow stores epoch ticks either way. The full
        // NaiveDateTime range fits i64 microseconds, so the µs arm is total.
        (ColBuilder::TsMicro(b), TypedValue::DateTime(ndt)) => {
            b.append_value(ndt.and_utc().timestamp_micros());
            Ok(())
        }
        (ColBuilder::TsNano(b), TypedValue::DateTime(ndt)) => {
            match ndt.and_utc().timestamp_nanos_opt() {
                Some(nanos) => {
                    b.append_value(nanos);
                    Ok(())
                }
                None => Err(()),
            }
        }
        // classify() always yields the variant matching the logical type;
        // this arm is defensive.
        _ => Err(()),
    }
}

/// Append one `Option` cell. Returns the number of warnings (0 or 1).
fn append_cell(builder: &mut ColBuilder, planned: &PlannedColumn, cell: Option<&str>) -> u64 {
    let Some(text) = cell else {
        // Columnar NULL from the Option plane.
        builder.append_null();
        return 0;
    };
    if let ColBuilder::Utf8(b) = builder {
        // Text stays verbatim; only a declared null token becomes NULL, so
        // `Some("")` survives as an empty string, distinct from null.
        match &planned.schema {
            Some(schema) if schema::is_null_token(schema, text) => b.append_null(),
            _ => b.append_value(text),
        }
        return 0;
    }
    let Some(schema) = planned.schema.as_ref() else {
        // Non-text targets are only ever chosen from a declared schema.
        builder.append_null();
        return 1;
    };
    match classify(Some(text), schema) {
        CellState::NullToken | CellState::Empty | CellState::Missing => {
            builder.append_null();
            0
        }
        CellState::Invalid(_) => {
            builder.append_null();
            1
        }
        CellState::Valid(value) => {
            if append_typed(builder, &planned.target, value).is_ok() {
                0
            } else {
                builder.append_null();
                1
            }
        }
    }
}

fn finish_builder(builder: ColBuilder, target: &Target) -> AppResult<ArrayRef> {
    Ok(match (builder, target) {
        (ColBuilder::Utf8(mut b), _) => Arc::new(b.finish()),
        (ColBuilder::Int64(mut b), _) => Arc::new(b.finish()),
        (ColBuilder::UInt64(mut b), _) => Arc::new(b.finish()),
        (ColBuilder::Float64(mut b), _) => Arc::new(b.finish()),
        (ColBuilder::Decimal(mut b), Target::Decimal { precision, scale }) => Arc::new(
            b.finish()
                .with_precision_and_scale(*precision, *scale as i8)
                .map_err(write_err)?,
        ),
        (ColBuilder::Boolean(mut b), _) => Arc::new(b.finish()),
        (ColBuilder::Date32(mut b), _) => Arc::new(b.finish()),
        (ColBuilder::TsMicro(mut b), Target::Timestamp { tz, .. }) => {
            Arc::new(b.finish().with_timezone_opt(tz.clone()))
        }
        (ColBuilder::TsNano(mut b), Target::Timestamp { tz, .. }) => {
            Arc::new(b.finish().with_timezone_opt(tz.clone()))
        }
        _ => {
            return Err(AppError::Other(
                "internal columnar export error: builder/target mismatch".into(),
            ))
        }
    })
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Byte-counting writer feeding the job's bytes-written progress.
struct CountingWriter<'a> {
    inner: &'a mut File,
    bytes: u64,
    ctx: &'a JobCtx,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        self.ctx.add_bytes(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn compression_of(c: ColumnarCompression) -> Compression {
    match c {
        ColumnarCompression::Uncompressed => Compression::UNCOMPRESSED,
        ColumnarCompression::Snappy => Compression::SNAPPY,
        ColumnarCompression::Zstd => Compression::ZSTD(ZstdLevel::default()),
    }
}

/// Stream the scoped rows as record batches into `write`.
fn write_batches(
    doc: &Document,
    plan: &ExportPlan,
    schema: &SchemaRef,
    warnings: &mut [u64],
    ctx: &JobCtx,
    mut write: impl FnMut(&RecordBatch) -> AppResult<()>,
) -> AppResult<()> {
    stream_scoped(doc, &plan.resolved, ctx, |chunk| {
        let mut builders: Vec<ColBuilder> =
            plan.cols.iter().map(|c| builder_for(&c.target)).collect();
        for row in &chunk {
            for (i, planned) in plan.cols.iter().enumerate() {
                warnings[i] += append_cell(&mut builders[i], planned, row[i].as_deref());
            }
        }
        let arrays: Vec<ArrayRef> = builders
            .into_iter()
            .zip(&plan.cols)
            .map(|(b, c)| finish_builder(b, &c.target))
            .collect::<AppResult<_>>()?;
        let batch = RecordBatch::try_new(schema.clone(), arrays).map_err(write_err)?;
        write(&batch)
    })
}

/// Run a revision-guarded, cancellable scoped export to Parquet / Arrow IPC
/// through the atomic-save pipeline. Any failure — stale revision, I/O,
/// cancellation — leaves the destination byte-for-byte untouched and removes
/// the staging file.
pub fn run(
    doc: &Document,
    dest: &Path,
    options: &ColumnarExportOptions,
    scope: &ExportScope,
    expected_revision: u64,
    ctx: &JobCtx,
) -> AppResult<ColumnarExportReport> {
    doc.check_revision(expected_revision)?;
    let plan = plan(doc, options, scope, ctx)?;
    let fields: Vec<Field> = plan.cols.iter().map(field_of).collect();
    let schema: SchemaRef = Arc::new(ArrowSchema::new(fields));
    let mut warnings = vec![0u64; plan.cols.len()];

    let bytes = save::atomic_write(dest, options.backup, |file| {
        let mut counting = CountingWriter {
            inner: file,
            bytes: 0,
            ctx,
        };
        match options.format {
            ColumnarFormat::Parquet => {
                let mut props = WriterProperties::builder()
                    .set_compression(compression_of(options.compression));
                if options.row_group_rows > 0 {
                    props = props.set_max_row_group_row_count(Some(options.row_group_rows));
                }
                let mut writer =
                    ArrowWriter::try_new(&mut counting, schema.clone(), Some(props.build()))
                        .map_err(write_err)?;
                write_batches(doc, &plan, &schema, &mut warnings, ctx, |batch| {
                    writer.write(batch).map_err(write_err)
                })?;
                writer.close().map_err(write_err)?;
            }
            ColumnarFormat::ArrowFile => {
                let mut writer =
                    IpcFileWriter::try_new(&mut counting, schema.as_ref()).map_err(write_err)?;
                write_batches(doc, &plan, &schema, &mut warnings, ctx, |batch| {
                    writer.write(batch).map_err(write_err)
                })?;
                writer.finish().map_err(write_err)?;
            }
            ColumnarFormat::ArrowStream => {
                let mut writer =
                    IpcStreamWriter::try_new(&mut counting, schema.as_ref()).map_err(write_err)?;
                write_batches(doc, &plan, &schema, &mut warnings, ctx, |batch| {
                    writer.write(batch).map_err(write_err)
                })?;
                writer.finish().map_err(write_err)?;
            }
        }
        Ok(counting.bytes)
    })?;
    ctx.flush_progress();

    let column_warnings: Vec<ColumnWarning> = plan
        .cols
        .iter()
        .zip(&warnings)
        .filter(|(_, &w)| w > 0)
        .map(|(c, &w)| ColumnWarning {
            name: c.name.clone(),
            invalid_cells: w,
        })
        .collect();
    Ok(ColumnarExportReport {
        format: options.format.wire_name().to_string(),
        rows: plan.resolved.rows.len() as u64,
        columns: plan.cols.len(),
        bytes,
        invalid_cells: warnings.iter().sum(),
        column_warnings,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use arrow::array::{Decimal128Array, StringArray, TimestampMicrosecondArray};

    use crate::dto::{Conjunction, FilterCondition, FilterGroup, FilterNode, FilterOp};
    use crate::job::JobRegistry;
    use crate::parquet_arrow::{self, ColumnarOpenOptions};
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    /// Attach a schema (by position) to an editable document.
    fn set_schema(
        doc: &mut Document,
        col: usize,
        lt: LogicalType,
        tweak: impl FnOnce(&mut ColumnSchema),
    ) {
        let id = doc.column_ids()[col].clone();
        let name = doc.headers()[col].clone();
        let mut schema = ColumnSchema::new(id, name, lt);
        tweak(&mut schema);
        doc.set_column_schema(schema);
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("export", Some(1), |_| {});
        (registry, ctx)
    }

    fn options(format: ColumnarFormat) -> ColumnarExportOptions {
        ColumnarExportOptions {
            format,
            ..ColumnarExportOptions::default()
        }
    }

    fn export_ok(
        doc: &Document,
        dest: &Path,
        options: &ColumnarExportOptions,
        scope: &ExportScope,
    ) -> ColumnarExportReport {
        let (_r, ctx) = ctx();
        run(doc, dest, options, scope, doc.revision(), &ctx).unwrap()
    }

    fn reopen(path: &Path) -> parquet_arrow::ColumnarFile {
        parquet_arrow::open_indexed(path, &ColumnarOpenOptions::default(), None).unwrap()
    }

    fn optional_plane(file: &parquet_arrow::ColumnarFile) -> Vec<TabularRow> {
        file.handle
            .read_optional(0, file.handle.n_rows(), None)
            .unwrap()
    }

    fn text_plane(file: &parquet_arrow::ColumnarFile) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        file.handle
            .visit(0..file.handle.n_rows(), &mut |_, row| {
                out.push(row.to_vec());
                Ok(true)
            })
            .unwrap();
        out
    }

    /// Write a one-batch parquet file directly through arrow (test input).
    fn write_parquet(path: &Path, cols: Vec<(&str, ArrayRef)>) {
        let fields: Vec<Field> = cols
            .iter()
            .map(|(n, a)| Field::new(*n, a.data_type().clone(), true))
            .collect();
        let schema = Arc::new(ArrowSchema::new(fields));
        let batch =
            RecordBatch::try_new(schema.clone(), cols.into_iter().map(|(_, a)| a).collect())
                .unwrap();
        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    // ----- null vs empty ---------------------------------------------------

    #[test]
    fn null_token_and_empty_string_round_trip_distinctly() {
        // A second column keeps every row non-blank (the CSV parser skips
        // fully blank lines).
        let mut d = doc_from("s,k\nNULL,a\n,b\nx,c", true);
        set_schema(&mut d, 0, LogicalType::Text, |s| {
            s.null_tokens = vec!["NULL".into()];
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.rows, 3);
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        assert_eq!(
            optional_plane(&file),
            vec![
                vec![None, some("a")],
                vec![some(""), some("b")],
                vec![some("x"), some("c")],
            ],
            "null token -> NULL; empty string stays an empty string"
        );
    }

    #[test]
    fn columnar_document_re_export_preserves_null_vs_empty() {
        // A columnar-backed document's Option plane is the source of truth:
        // NULL arrives as None and must survive a re-export without any null
        // token in play.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.parquet");
        write_parquet(
            &source,
            vec![(
                "s",
                Arc::new(StringArray::from(vec![None, Some(""), Some("x")])) as ArrayRef,
            )],
        );
        let doc = Document::from_columnar(1, Some(source.clone()), reopen(&source));

        let dest = dir.path().join("out.parquet");
        export_ok(
            &doc,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        let file = reopen(&dest);
        assert_eq!(
            optional_plane(&file),
            vec![vec![None], vec![some("")], vec![some("x")]]
        );
    }

    // ----- integers --------------------------------------------------------

    #[test]
    fn signed_integer_extremes_round_trip_as_int64() {
        let mut d = doc_from("i\n9223372036854775807\n-9223372036854775808\n0", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        assert_eq!(file.schemas[0].logical_type, LogicalType::Integer);
        let text = text_plane(&file);
        assert_eq!(text[0][0], i64::MAX.to_string());
        assert_eq!(text[1][0], i64::MIN.to_string());
        assert_eq!(text[2][0], "0");
    }

    #[test]
    fn u64_max_selects_uint64_and_round_trips_losslessly() {
        let mut d = doc_from("u\n18446744073709551615\n5", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        let text = text_plane(&file);
        assert_eq!(text[0][0], u64::MAX.to_string(), "u64::MAX lossless");
        assert_eq!(text[1][0], "5");
    }

    #[test]
    fn beyond_i64_with_negatives_warns_and_nulls() {
        // A single arrow integer type cannot carry BOTH u64::MAX and a
        // negative; the negative keeps Int64 and the oversized value becomes
        // null + a counted warning.
        let mut d = doc_from("i\n18446744073709551615\n-1", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 1);
        assert_eq!(report.column_warnings.len(), 1);
        assert_eq!(report.column_warnings[0].name, "i");

        let file = reopen(&dest);
        assert_eq!(
            optional_plane(&file),
            vec![vec![None], vec![some("-1")]],
            "the unrepresentable value exported as NULL"
        );
    }

    #[test]
    fn unparseable_cells_export_null_with_warning_count() {
        let mut d = doc_from("n\nabc\n5\nxyz", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 2);
        assert_eq!(report.column_warnings[0].invalid_cells, 2);

        let file = reopen(&dest);
        assert_eq!(
            optional_plane(&file),
            vec![vec![None], vec![some("5")], vec![None]]
        );
    }

    #[test]
    fn empty_cell_in_numeric_column_is_null_without_warning() {
        let mut d = doc_from("n,k\n,a\n7,b", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(
            report.invalid_cells, 0,
            "empty means 'no value', not invalid"
        );

        let file = reopen(&dest);
        assert_eq!(
            optional_plane(&file),
            vec![vec![None, some("a")], vec![some("7"), some("b")]]
        );
    }

    // ----- decimals --------------------------------------------------------

    #[test]
    fn decimal_scales_unify_to_the_widest_exactly() {
        let mut d = doc_from("d\n1.50\n-0.055\n2", true);
        set_schema(&mut d, 0, LogicalType::Decimal, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        assert_eq!(file.schemas[0].logical_type, LogicalType::Decimal);
        let text = text_plane(&file);
        assert_eq!(
            (
                text[0][0].as_str(),
                text[1][0].as_str(),
                text[2][0].as_str()
            ),
            ("1.500", "-0.055", "2.000"),
            "one column scale; every value rescaled exactly, never rounded"
        );
    }

    #[test]
    fn columnar_decimal_round_trips_with_identical_scale() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.parquet");
        write_parquet(
            &source,
            vec![(
                "dec",
                Arc::new(
                    Decimal128Array::from(vec![Some(150i128), Some(-5), None])
                        .with_precision_and_scale(12, 2)
                        .unwrap(),
                ) as ArrayRef,
            )],
        );
        let doc = Document::from_columnar(1, Some(source.clone()), reopen(&source));
        let before = optional_plane(&reopen(&source));

        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &doc,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);
        assert_eq!(
            optional_plane(&reopen(&dest)),
            before,
            "uniform source scale -> byte-identical decimal text (1.50 stays 1.50)"
        );
    }

    // ----- timestamps ------------------------------------------------------

    #[test]
    fn zoned_timestamp_preserves_zone_metadata_and_instants() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.parquet");
        let ticks = 1_700_000_000_123_456i64; // 2023-11-14T22:13:20.123456Z
        write_parquet(
            &source,
            vec![(
                "ts",
                Arc::new(
                    TimestampMicrosecondArray::from(vec![Some(ticks), None])
                        .with_timezone("Europe/Berlin"),
                ) as ArrayRef,
            )],
        );
        let opened = reopen(&source);
        assert_eq!(
            opened.schemas[0].time_zone.as_deref(),
            Some("Europe/Berlin")
        );
        let before = optional_plane(&opened);
        let doc = Document::from_columnar(1, Some(source.clone()), opened);

        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &doc,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        assert_eq!(
            file.schemas[0].time_zone.as_deref(),
            Some("Europe/Berlin"),
            "timezone metadata survives the round trip"
        );
        assert_eq!(optional_plane(&file), before, "UTC instants identical");
    }

    #[test]
    fn subsecond_timestamps_choose_nanoseconds() {
        let mut d = doc_from(
            "ts\n2024-01-02T03:04:05.123456789\n2024-01-02T03:04:06",
            true,
        );
        set_schema(&mut d, 0, LogicalType::Datetime, |s| {
            s.input_formats = Some(vec!["%Y-%m-%dT%H:%M:%S%.f".into()]);
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        let text = text_plane(&file);
        assert_eq!(
            text[0][0], "2024-01-02T03:04:05.123456789",
            "sub-microsecond digits survive via the nanosecond unit"
        );
        assert_eq!(text[1][0], "2024-01-02T03:04:06");
    }

    // ----- the remaining scalar types --------------------------------------

    #[test]
    fn float_boolean_and_date_round_trip() {
        let mut d = doc_from("f,b,d\n0.1,true,2024-01-01\n-1.5,false,1969-12-31", true);
        set_schema(&mut d, 0, LogicalType::Float, |_| {});
        set_schema(&mut d, 1, LogicalType::Boolean, |_| {});
        set_schema(&mut d, 2, LogicalType::Date, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );
        assert_eq!(report.invalid_cells, 0);

        let file = reopen(&dest);
        let logicals: Vec<LogicalType> = file.schemas.iter().map(|s| s.logical_type).collect();
        assert_eq!(
            logicals,
            [LogicalType::Float, LogicalType::Boolean, LogicalType::Date]
        );
        let text = text_plane(&file);
        assert_eq!(text[0], ["0.1", "true", "2024-01-01"]);
        assert_eq!(text[1], ["-1.5", "false", "1969-12-31"]);
    }

    // ----- untyped exports -------------------------------------------------

    #[test]
    fn no_schema_exports_all_utf8() {
        let d = doc_from("a,b\n1,x\n2,", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
        );

        let file = reopen(&dest);
        assert!(file
            .schemas
            .iter()
            .all(|s| s.logical_type == LogicalType::Text));
        assert_eq!(
            optional_plane(&file),
            vec![vec![some("1"), some("x")], vec![some("2"), some("")]],
            "text verbatim; the empty CSV cell stays an empty string, not null"
        );
    }

    #[test]
    fn typed_false_keeps_cell_text_verbatim() {
        let mut d = doc_from("n\n007", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let mut opts = options(ColumnarFormat::Parquet);
        opts.typed = false;
        export_ok(&d, &dest, &opts, &ExportScope::All);

        let file = reopen(&dest);
        assert_eq!(file.schemas[0].logical_type, LogicalType::Text);
        assert_eq!(text_plane(&file)[0][0], "007", "no canonicalisation");
    }

    // ----- scopes ----------------------------------------------------------

    #[test]
    fn scoped_export_writes_exact_rows_and_columns_in_order() {
        let mut d = doc_from("a,b,c\n1,2,3\n4,5,6\n7,8,9", true);
        d.set_filter(vec![0, 2]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let scope = ExportScope::SelectedColumns {
            columns: vec![2, 0],
        };
        let report = export_ok(&d, &dest, &options(ColumnarFormat::Parquet), &scope);
        assert_eq!((report.rows, report.columns), (2, 2));

        let file = reopen(&dest);
        assert_eq!(file.headers, ["c", "a"], "user column order preserved");
        assert_eq!(
            text_plane(&file),
            vec![vec!["3", "1"], vec!["9", "7"]],
            "exactly the filtered rows, in display order"
        );
    }

    #[test]
    fn empty_scope_writes_a_schema_only_file() {
        let mut d = doc_from("a\n1", true);
        d.set_filter(vec![]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let report = export_ok(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::VisibleRows,
        );
        assert_eq!(report.rows, 0);

        let file = reopen(&dest);
        assert_eq!(file.headers, ["a"]);
        assert_eq!(file.handle.n_rows(), 0);
    }

    // ----- the three containers -------------------------------------------

    #[test]
    fn arrow_file_and_stream_round_trip_like_parquet() {
        let mut d = doc_from("i,s\n42,x\n,\n18446744073709551615,y", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();

        let mut planes = Vec::new();
        for (format, name) in [
            (ColumnarFormat::Parquet, "out.parquet"),
            (ColumnarFormat::ArrowFile, "out.arrow"),
            (ColumnarFormat::ArrowStream, "out.arrows"),
        ] {
            let dest = dir.path().join(name);
            let report = export_ok(&d, &dest, &options(format), &ExportScope::All);
            assert_eq!(report.format, format.wire_name());
            let inspection = parquet_arrow::inspect(&dest, None).unwrap();
            assert_eq!(inspection.format, format.wire_name(), "format sniffs back");
            assert_eq!(inspection.row_count, 3);
            planes.push(optional_plane(&reopen(&dest)));
        }
        assert_eq!(planes[0], planes[1]);
        assert_eq!(planes[1], planes[2]);
        assert_eq!(
            planes[0][1],
            vec![None, some("")],
            "empty integer cell -> NULL; empty text cell stays empty"
        );
        assert_eq!(planes[0][2][0], some("18446744073709551615"));
    }

    #[test]
    fn parquet_compression_codecs_apply() {
        let mut d = doc_from("n\n1\n2\n3", true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();

        for (compression, expect) in [
            (ColumnarCompression::Uncompressed, "UNCOMPRESSED"),
            (ColumnarCompression::Snappy, "SNAPPY"),
            (ColumnarCompression::Zstd, "ZSTD"),
        ] {
            let dest = dir.path().join(format!("{expect}.parquet"));
            let mut opts = options(ColumnarFormat::Parquet);
            opts.compression = compression;
            export_ok(&d, &dest, &opts, &ExportScope::All);
            let inspection = parquet_arrow::inspect(&dest, None).unwrap();
            let codecs = inspection.compression.unwrap_or_default();
            assert!(codecs.contains(expect), "{expect} not in {codecs:?}");
            assert_eq!(text_plane(&reopen(&dest))[2][0], "3");
        }
    }

    // ----- row groups + statistics pruning ---------------------------------

    #[test]
    fn row_group_size_applies_and_pruned_filtered_reads_match_full_scan() {
        let mut csv = String::from("n\n");
        for i in 0..100 {
            csv.push_str(&format!("{i}\n"));
        }
        let mut d = doc_from(&csv, true);
        set_schema(&mut d, 0, LogicalType::Integer, |_| {});
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let mut opts = options(ColumnarFormat::Parquet);
        opts.row_group_rows = 10;
        export_ok(&d, &dest, &opts, &ExportScope::All);

        let inspection = parquet_arrow::inspect(&dest, None).unwrap();
        assert_eq!(inspection.chunk_count, 10, "row_group_rows bounds groups");

        let doc = Document::from_columnar(2, Some(dest.clone()), reopen(&dest));
        let spec = FilterGroup {
            conjunction: Conjunction::And,
            nodes: vec![FilterNode::Condition(FilterCondition {
                column: 0,
                op: FilterOp::Gte,
                value: "73".into(),
                case_sensitive: false,
            })],
        };
        let ranges = doc
            .filter_scan_ranges(&spec)
            .expect("statistics pruning applies");
        let visited: usize = ranges.iter().map(|r| r.len()).sum();
        assert!(visited < 100, "row groups below the bound are skipped");

        let matches = crate::filter::matching_rows(&doc, &spec).unwrap();
        assert_eq!(
            matches,
            (73..100).collect::<Vec<_>>(),
            "pruned filtered read returns exactly the full-scan matches"
        );
    }

    // ----- guards ----------------------------------------------------------

    #[test]
    fn cancelled_export_removes_all_output() {
        let d = doc_from("a\n1\n2\n3", true);
        let registry = JobRegistry::default();
        let ctx = registry.begin("export", Some(1), |_| {});
        registry.cancel(ctx.id);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cancel.parquet");
        let result = run(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
            d.revision(),
            &ctx,
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(!dest.exists(), "no destination file");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no staging litter"
        );
    }

    #[test]
    fn stale_revision_is_rejected_before_writing() {
        let mut d = doc_from("a\n1", true);
        let stale = d.revision();
        d.set_cell(0, 0, "changed".into()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let (_r, ctx) = ctx();
        let err = run(
            &d,
            &dest,
            &options(ColumnarFormat::Parquet),
            &ExportScope::All,
            stale,
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::StaleRevision { .. }), "{err}");
        assert!(!dest.exists());
    }
}
