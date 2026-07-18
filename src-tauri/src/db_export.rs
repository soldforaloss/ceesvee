//! F35 database export: write a CEESVEE document into a SQLite database.
//!
//! This is the ONE deliberate write path to a database file in CEESVEE —
//! everything else in F35/F36 goes through [`crate::safe_query`]'s read-only
//! guarded connections. (SQLite only this cycle; DuckDB is out of scope —
//! see the [`crate::db_browser`] module docs.)
//!
//! The flow is preview-then-write, like every destructive CEESVEE action:
//!
//! 1. **Mapping preview** ([`build_preview`] / `db_export_preview`): resolves
//!    each document column to a SQL column name and type. Defaults come from
//!    the F31 declared schema (`integer → INTEGER`, `decimal → NUMERIC`,
//!    `float → REAL`, `boolean → BOOLEAN`, everything else — including
//!    date/datetime/uuid/json and undeclared columns — `TEXT`); each column
//!    can be overridden per mapping entry. In append mode the EXISTING
//!    table's declared types win (they are what the rows must satisfy) and
//!    the preview lists every compatibility problem: appending never alters
//!    the target schema.
//! 2. **Conversion-failure preview**: the same cell conversion the write
//!    uses runs over the document (bounded to [`PREVIEW_SCAN_CAP`] rows,
//!    `scanComplete` says whether that covered everything) and reports the
//!    total failure count plus bounded samples, so the user sees exactly
//!    what would abort the write — before writing anything.
//! 3. **The write** ([`run_export`] / `start_db_export`): ONE transaction
//!    (`BEGIN IMMEDIATE`) covering target validation, DDL and every row.
//!    Any failure — conversion error, constraint violation under the
//!    "abort" policy, cancellation, disk error — rolls the whole
//!    transaction back and leaves the database file exactly as it was.
//!    `replace` drops and recreates the table INSIDE that transaction, so
//!    even a failing replace keeps the original table intact.
//!
//! Key semantics:
//! * **NULLs**: for typed (non-TEXT) columns an empty cell or a configured
//!   null token becomes SQL `NULL`; TEXT columns are written verbatim (the
//!   document cannot represent SQL NULL — reads narrowed it to "" — so a
//!   TEXT round trip turns NULL into '').
//! * **Values**: integers are bound as 64-bit ints (out-of-range i128 is a
//!   conversion failure), floats as REAL, booleans as 0/1, decimals as
//!   their canonical text (SQLite's NUMERIC affinity applies from there).
//!   Parsing reuses the F31 classifier, so declared locales and null
//!   tokens behave exactly like the rest of the app.
//! * **PK conflicts**: explicit policy — `abort` (plain INSERT: first
//!   conflict fails and rolls back everything), `skip` (INSERT OR IGNORE),
//!   `replace` (INSERT OR REPLACE) — applying to ANY uniqueness
//!   constraint on the target.
//! * **Approval**: the user picking the export target in the file dialog
//!   is approval, exactly like `db_open` — the path joins the
//!   SafeQueryEngine registry so refresh probes and F36 can read what was
//!   just written.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::db_browser::{self, blocking_err, is_without_rowid, logical_type_of_decl, quote_ident};
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::{JobCtx, JobRegistry};
use crate::safe_query::{self, ApprovedSources};
use crate::schema::{classify, CellState, ColumnSchema, LogicalType, TypedValue};
use crate::state::{AppState, SharedDocument};

/// The conversion-failure preview scans at most this many rows (mirrors the
/// F31 inference sample). `scanComplete` in the preview says whether the
/// whole document was covered; the write itself always validates every row.
const PREVIEW_SCAN_CAP: usize = 100_000;
/// At most this many failure samples are materialised (the count is exact).
const FAILURE_SAMPLES_MAX: usize = 50;
/// Failure-sample cell values are clipped to this many characters.
const SAMPLE_VALUE_MAX: usize = 120;
/// Cancellation/progress cadence inside the insert loop.
const CANCEL_EVERY: u64 = 1024;
/// How long the write connection waits on a locked database before failing.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Wire DTOs
// ---------------------------------------------------------------------------

/// Everything the export dialog sends: target, mode and per-column
/// overrides. The same spec drives both the preview and the write.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbExportSpec {
    /// Target database file (created for `create` mode when missing).
    pub path: String,
    pub table: String,
    /// "create" | "append" | "replace".
    pub mode: String,
    /// Per-column overrides keyed by stable column id; unlisted columns get
    /// defaults. ALL document columns are always exported.
    #[serde(default)]
    pub mappings: Vec<DbColumnMapIn>,
    /// "abort" (default) | "skip" | "replace".
    #[serde(default = "default_policy")]
    pub conflict_policy: String,
    /// Must be `true` for `replace` mode — the API-level record that the
    /// user explicitly confirmed dropping the existing table.
    #[serde(default)]
    pub confirm_replace: bool,
}

fn default_policy() -> String {
    "abort".into()
}

/// One per-column override in a [`DbExportSpec`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbColumnMapIn {
    pub column_id: String,
    /// SQL column name (default: the document header, disambiguated).
    #[serde(default)]
    pub sql_name: Option<String>,
    /// "TEXT" | "INTEGER" | "REAL" | "NUMERIC" | "BOOLEAN" (default: from
    /// the declared F31 schema, TEXT fallback). Ignored in append mode —
    /// the existing table's declared types win.
    #[serde(default)]
    pub sql_type: Option<String>,
    /// Part of the new table's PRIMARY KEY (create/replace only).
    #[serde(default)]
    pub primary_key: bool,
}

/// The resolved mapping + validation + conversion preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbExportPreview {
    /// Document revision the preview was computed against; echo it to
    /// `start_db_export` as the apply guard.
    pub revision: u64,
    pub table_exists: bool,
    /// Row estimate for the existing target table (replace confirmation UI).
    pub target_rows: Option<u64>,
    pub columns: Vec<DbExportColumn>,
    /// Blocking problems (append incompatibilities, existing table in
    /// create mode, …). Empty = the write can proceed.
    pub blocking: Vec<String>,
    /// Bounded conversion-failure samples ([`FAILURE_SAMPLES_MAX`]).
    pub failures: Vec<DbConversionFailure>,
    /// Exact failure count within the scanned range.
    pub failure_count: u64,
    pub rows_scanned: u64,
    /// Whether the scan covered the whole document (see [`PREVIEW_SCAN_CAP`]).
    pub scan_complete: bool,
}

/// One resolved column mapping.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbExportColumn {
    pub column_id: String,
    /// Document header.
    pub name: String,
    pub sql_name: String,
    pub sql_type: String,
    pub primary_key: bool,
    /// Append mode: the existing target column's declared type (verbatim).
    pub target_decl_type: Option<String>,
}

/// One conversion-failure sample. `row` is the 0-based data row index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbConversionFailure {
    pub row: u64,
    /// SQL column name of the failing mapping.
    pub column: String,
    /// The offending cell text, clipped to [`SAMPLE_VALUE_MAX`] chars.
    pub value: String,
    pub reason: String,
}

/// What a finished export did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbExportResult {
    pub table: String,
    pub mode: String,
    pub rows_written: u64,
    /// Rows skipped by the "skip" conflict policy.
    pub rows_skipped: u64,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportMode {
    Create,
    Append,
    Replace,
}

impl ExportMode {
    fn parse(s: &str) -> AppResult<ExportMode> {
        match s {
            "create" => Ok(ExportMode::Create),
            "append" => Ok(ExportMode::Append),
            "replace" => Ok(ExportMode::Replace),
            other => Err(AppError::invalid(format!(
                "unknown export mode \"{other}\" (expected create, append or replace)"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ExportMode::Create => "create",
            ExportMode::Append => "append",
            ExportMode::Replace => "replace",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictPolicy {
    Abort,
    Skip,
    Replace,
}

impl ConflictPolicy {
    fn parse(s: &str) -> AppResult<ConflictPolicy> {
        match s {
            "abort" => Ok(ConflictPolicy::Abort),
            "skip" => Ok(ConflictPolicy::Skip),
            "replace" => Ok(ConflictPolicy::Replace),
            other => Err(AppError::invalid(format!(
                "unknown conflict policy \"{other}\" (expected abort, skip or replace)"
            ))),
        }
    }

    /// The INSERT conflict clause implementing the policy.
    fn verb(self) -> &'static str {
        match self {
            ConflictPolicy::Abort => "",
            ConflictPolicy::Skip => "OR IGNORE ",
            ConflictPolicy::Replace => "OR REPLACE ",
        }
    }
}

/// The five writable SQL column types. Deliberately small: every one has an
/// exact F31 validation type, so "will this cell convert?" has one answer
/// shared by the preview and the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlType {
    Text,
    Integer,
    Real,
    Numeric,
    Boolean,
}

impl SqlType {
    fn parse(s: &str) -> AppResult<SqlType> {
        match s.trim().to_ascii_uppercase().as_str() {
            "TEXT" => Ok(SqlType::Text),
            "INTEGER" => Ok(SqlType::Integer),
            "REAL" => Ok(SqlType::Real),
            "NUMERIC" => Ok(SqlType::Numeric),
            "BOOLEAN" => Ok(SqlType::Boolean),
            other => Err(AppError::invalid(format!(
                "unknown SQL type \"{other}\" (expected TEXT, INTEGER, REAL, NUMERIC or BOOLEAN)"
            ))),
        }
    }

    /// Declared type text used in generated DDL (and shown in the preview).
    fn decl(self) -> &'static str {
        match self {
            SqlType::Text => "TEXT",
            SqlType::Integer => "INTEGER",
            SqlType::Real => "REAL",
            SqlType::Numeric => "NUMERIC",
            SqlType::Boolean => "BOOLEAN",
        }
    }

    /// The default SQL type for a declared F31 logical type ("TEXT default"
    /// for everything CEESVEE would otherwise round-trip as text).
    fn for_logical(lt: LogicalType) -> SqlType {
        match lt {
            LogicalType::Integer => SqlType::Integer,
            LogicalType::Float => SqlType::Real,
            LogicalType::Decimal => SqlType::Numeric,
            LogicalType::Boolean => SqlType::Boolean,
            LogicalType::Text
            | LogicalType::Date
            | LogicalType::Datetime
            | LogicalType::Uuid
            | LogicalType::Json => SqlType::Text,
        }
    }

    /// The F31 logical type whose parser decides whether a cell converts.
    fn validation_type(self) -> LogicalType {
        match self {
            SqlType::Text => LogicalType::Text,
            SqlType::Integer => LogicalType::Integer,
            SqlType::Real => LogicalType::Float,
            SqlType::Numeric => LogicalType::Decimal,
            SqlType::Boolean => LogicalType::Boolean,
        }
    }
}

// ---------------------------------------------------------------------------
// Plan resolution
// ---------------------------------------------------------------------------

/// One document column's resolved write plan.
struct ColumnPlan {
    doc_col: usize,
    column_id: String,
    doc_name: String,
    sql_name: String,
    sql_type: SqlType,
    /// Validation schema: the declared F31 schema (locale, null tokens)
    /// with its logical type swapped for [`SqlType::validation_type`].
    schema: ColumnSchema,
    primary_key: bool,
    target_decl: Option<String>,
}

fn validate_table_name(table: &str) -> AppResult<&str> {
    let trimmed = table.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid("the table name is empty"));
    }
    if trimmed.contains('\0') {
        return Err(AppError::invalid("the table name contains a NUL character"));
    }
    if trimmed.to_ascii_lowercase().starts_with("sqlite_") {
        return Err(AppError::invalid(
            "table names starting with \"sqlite_\" are reserved by SQLite",
        ));
    }
    Ok(trimmed)
}

/// First name based on `base` that is not already used, comparing the way
/// SQLite compares identifiers (ASCII case-insensitively).
fn unique_name_ci(used: &[String], base: &str) -> String {
    if !used.iter().any(|u| u.eq_ignore_ascii_case(base)) {
        return base.to_string();
    }
    for i in 2.. {
        let candidate = format!("{base} ({i})");
        if !used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
            return candidate;
        }
    }
    unreachable!("the loop above always returns");
}

/// The validation schema for one column: declared F31 settings (locale,
/// null tokens) preserved, logical type forced to the SQL target's.
fn conversion_schema(
    declared: Option<&ColumnSchema>,
    id: &str,
    name: &str,
    sql_type: SqlType,
) -> ColumnSchema {
    let lt = sql_type.validation_type();
    match declared {
        Some(s) => {
            let mut s = s.clone();
            s.logical_type = lt;
            s
        }
        None => ColumnSchema::new(id, name, lt),
    }
}

/// Resolve defaults + overrides into one plan entry per document column
/// (append-mode target reconciliation happens later, in [`check_target`]).
/// Hard errors here mean the SPEC is malformed; target incompatibilities are
/// collected as blocking issues instead.
fn base_plan(doc: &Document, spec: &DbExportSpec) -> AppResult<Vec<ColumnPlan>> {
    let headers = doc.headers();
    let ids = doc.column_ids();
    let mut overrides: HashMap<&str, &DbColumnMapIn> = HashMap::new();
    for m in &spec.mappings {
        if !ids.iter().any(|id| id == &m.column_id) {
            return Err(AppError::invalid(format!(
                "mapping refers to unknown column id \"{}\"",
                m.column_id
            )));
        }
        if overrides.insert(m.column_id.as_str(), m).is_some() {
            return Err(AppError::invalid(format!(
                "duplicate mapping for column id \"{}\"",
                m.column_id
            )));
        }
    }

    let mut used: Vec<String> = Vec::with_capacity(headers.len());
    let mut plan: Vec<ColumnPlan> = Vec::with_capacity(headers.len());
    for (c, header) in headers.iter().enumerate() {
        let id = &ids[c];
        let declared = doc.schema().column(id);
        let ov = overrides.get(id.as_str()).copied();

        let sql_name = match ov.and_then(|m| m.sql_name.as_deref()) {
            Some(name) => {
                let name = name.trim();
                if name.is_empty() || name.contains('\0') {
                    return Err(AppError::invalid(format!(
                        "invalid SQL column name for \"{header}\""
                    )));
                }
                if used.iter().any(|u| u.eq_ignore_ascii_case(name)) {
                    return Err(AppError::invalid(format!(
                        "duplicate SQL column name \"{name}\" \
                         (SQLite column names are case-insensitive)"
                    )));
                }
                name.to_string()
            }
            None => {
                let base: String = header.chars().filter(|ch| *ch != '\0').collect();
                let base = base.trim();
                let base = if base.is_empty() {
                    format!("column_{}", c + 1)
                } else {
                    base.to_string()
                };
                unique_name_ci(&used, &base)
            }
        };
        used.push(sql_name.clone());

        let sql_type = match ov.and_then(|m| m.sql_type.as_deref()) {
            Some(t) => SqlType::parse(t)?,
            None => declared
                .map(|s| SqlType::for_logical(s.logical_type))
                .unwrap_or(SqlType::Text),
        };

        plan.push(ColumnPlan {
            doc_col: c,
            column_id: id.clone(),
            doc_name: header.clone(),
            sql_name,
            sql_type,
            schema: conversion_schema(declared, id, header, sql_type),
            primary_key: ov.map(|m| m.primary_key).unwrap_or(false),
            target_decl: None,
        });
    }
    Ok(plan)
}

// ---------------------------------------------------------------------------
// Target validation
// ---------------------------------------------------------------------------

struct TargetCheck {
    /// Whether a TABLE with the target name exists.
    exists: bool,
    issues: Vec<String>,
}

struct TargetColumn {
    name: String,
    decl: String,
    notnull: bool,
    has_default: bool,
    pk: i64,
}

/// Validate the plan against the CURRENT database state and, in append
/// mode, reconcile the plan with the existing table (its declared types and
/// stored column names win). Collects problems instead of failing so the
/// preview can show all of them at once; the write treats any issue as a
/// hard error. Runs inside the write transaction so validation and write
/// see the same state.
fn check_target(
    conn: &Connection,
    table: &str,
    plan: &mut [ColumnPlan],
    mode: ExportMode,
) -> AppResult<TargetCheck> {
    let mut issues: Vec<String> = Vec::new();

    // Anything already using the name? (Tables, views, indexes and triggers
    // share one namespace.)
    let mut stmt = conn.prepare("SELECT type FROM sqlite_master WHERE name = ?1 COLLATE NOCASE")?;
    let types: Vec<String> = stmt
        .query_map([table], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    let exists = types.iter().any(|t| t == "table");
    if !exists {
        if let Some(other) = types.first() {
            issues.push(format!(
                "\"{table}\" already names a {other} in this database — pick another table name"
            ));
        }
    }

    match mode {
        ExportMode::Create => {
            if exists {
                issues.push(format!(
                    "table \"{table}\" already exists — choose append or replace"
                ));
            }
        }
        ExportMode::Replace => {
            // Replacing a missing table is just create; nothing to check.
        }
        ExportMode::Append => {
            if !exists {
                if types.is_empty() {
                    issues.push(format!(
                        "table \"{table}\" does not exist — use create instead"
                    ));
                }
                return Ok(TargetCheck { exists, issues });
            }
            let mut stmt = conn.prepare(
                "SELECT name, type, \"notnull\", dflt_value, pk
                 FROM pragma_table_info(?1) ORDER BY cid",
            )?;
            let target: Vec<TargetColumn> = stmt
                .query_map([table], |r| {
                    Ok(TargetColumn {
                        name: r.get(0)?,
                        decl: r.get(1)?,
                        notnull: r.get::<_, i64>(2)? != 0,
                        has_default: r.get::<_, Option<String>>(3)?.is_some(),
                        pk: r.get(4)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            drop(stmt);

            // Reconcile: every document column must land on an existing
            // target column; the target's stored name and declared type win.
            let mut mapped: Vec<bool> = vec![false; target.len()];
            for col in plan.iter_mut() {
                match target
                    .iter()
                    .position(|t| t.name.eq_ignore_ascii_case(&col.sql_name))
                {
                    Some(ti) => {
                        if mapped[ti] {
                            issues.push(format!(
                                "two document columns map onto target column \"{}\"",
                                target[ti].name
                            ));
                        }
                        mapped[ti] = true;
                        let t = &target[ti];
                        col.sql_name = t.name.clone();
                        col.sql_type = SqlType::for_logical(logical_type_of_decl(&t.decl));
                        col.schema.logical_type = col.sql_type.validation_type();
                        col.target_decl = Some(t.decl.clone());
                    }
                    None => issues.push(format!(
                        "column \"{}\" does not exist in table \"{table}\" — appending never \
                         alters the table (rename the mapping or export to a new table)",
                        col.sql_name
                    )),
                }
            }

            // Unmapped NOT NULL columns without a default would fail every
            // insert; surface that up front. A lone INTEGER PRIMARY KEY of a
            // rowid table is exempt — SQLite auto-assigns it.
            let pk_count = target.iter().filter(|t| t.pk > 0).count();
            let without_rowid = is_without_rowid(conn, table)?;
            for (ti, t) in target.iter().enumerate() {
                let rowid_alias = !without_rowid
                    && pk_count == 1
                    && t.pk == 1
                    && t.decl.eq_ignore_ascii_case("integer");
                if !mapped[ti] && t.notnull && !t.has_default && !rowid_alias {
                    issues.push(format!(
                        "target column \"{}\" is NOT NULL without a default and no document \
                         column maps onto it",
                        t.name
                    ));
                }
            }
        }
    }
    Ok(TargetCheck { exists, issues })
}

// ---------------------------------------------------------------------------
// Cell conversion (shared by preview and write)
// ---------------------------------------------------------------------------

/// Convert one document cell to the SQL value its column plan calls for.
/// `Err(reason)` is a conversion failure: the preview reports it, the write
/// aborts (and rolls back) on it.
fn convert_cell(col: &ColumnPlan, raw: &str) -> Result<Value, String> {
    if col.sql_type == SqlType::Text {
        // Verbatim, including empty cells and null tokens: TEXT columns
        // carry exactly what the grid shows.
        return Ok(Value::Text(raw.to_string()));
    }
    match classify(Some(raw), &col.schema) {
        CellState::Missing | CellState::NullToken | CellState::Empty => Ok(Value::Null),
        CellState::Invalid(reason) => Err(reason),
        CellState::Valid(tv) => match tv {
            TypedValue::Integer(i) => i64::try_from(i)
                .map(Value::Integer)
                .map_err(|_| "integer is outside SQLite's 64-bit integer range".to_string()),
            TypedValue::Float(f) => Ok(Value::Real(f)),
            // Canonical text; SQLite's NUMERIC affinity converts it to
            // INTEGER/REAL when that is lossless-per-SQLite, text otherwise.
            TypedValue::Decimal(d) => Ok(Value::Text(d.to_plain_string())),
            TypedValue::Boolean(b) => Ok(Value::Integer(b as i64)),
            // The validation type is always one of the five above; other
            // variants cannot appear. Defensive: write the raw text.
            _ => Ok(Value::Text(raw.to_string())),
        },
    }
}

fn clip_value(raw: &str) -> String {
    if raw.chars().count() <= SAMPLE_VALUE_MAX {
        return raw.to_string();
    }
    let mut out: String = raw.chars().take(SAMPLE_VALUE_MAX).collect();
    out.push('…');
    out
}

/// Scan up to `cap` rows for conversion failures: exact count within the
/// scanned range plus bounded samples. Returns
/// `(samples, count, rows_scanned, scan_complete)`.
fn scan_failures(
    doc: &Document,
    plan: &[ColumnPlan],
    cap: usize,
) -> AppResult<(Vec<DbConversionFailure>, u64, u64, bool)> {
    let typed: Vec<&ColumnPlan> = plan
        .iter()
        .filter(|c| c.sql_type != SqlType::Text)
        .collect();
    let n = doc.n_rows();
    if typed.is_empty() || n == 0 {
        // TEXT-only exports cannot fail conversion; skip the scan.
        return Ok((Vec::new(), 0, 0, true));
    }
    let scan_end = n.min(cap);
    let mut samples: Vec<DbConversionFailure> = Vec::new();
    let mut count = 0u64;
    doc.visit_rows(0..scan_end, &mut |i, row| {
        for col in &typed {
            let raw = row.get(col.doc_col).map(String::as_str).unwrap_or("");
            if let Err(reason) = convert_cell(col, raw) {
                count += 1;
                if samples.len() < FAILURE_SAMPLES_MAX {
                    samples.push(DbConversionFailure {
                        row: i as u64,
                        column: col.sql_name.clone(),
                        value: clip_value(raw),
                        reason,
                    });
                }
            }
        }
        Ok(true)
    })?;
    Ok((samples, count, scan_end as u64, scan_end == n))
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// Build the full export preview: resolved mapping, target validation and
/// the bounded conversion-failure report. Read-only — nothing is written.
pub(crate) fn build_preview(doc: &Document, spec: &DbExportSpec) -> AppResult<DbExportPreview> {
    let mode = ExportMode::parse(&spec.mode)?;
    ConflictPolicy::parse(&spec.conflict_policy)?;
    let table = validate_table_name(&spec.table)?;
    let mut plan = base_plan(doc, spec)?;

    let path = Path::new(&spec.path);
    let mut blocking: Vec<String> = Vec::new();
    let mut table_exists = false;
    let mut target_rows = None;
    if path.exists() {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| AppError::invalid(format!("cannot open database file: {e}")))?;
        let conn = safe_query::open_guarded(&canonical)?;
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| {
            AppError::invalid(format!(
                "'{}' does not look like a SQLite database: {e}",
                spec.path
            ))
        })?;
        let check = check_target(&conn, table, &mut plan, mode)?;
        table_exists = check.exists;
        blocking.extend(check.issues);
        if table_exists {
            let (estimate, _) = db_browser::row_estimate(&conn, table, "table")?;
            target_rows = Some(estimate);
        }
    } else if mode != ExportMode::Create {
        blocking.push(format!("database file does not exist: {}", spec.path));
    }

    let (failures, failure_count, rows_scanned, scan_complete) =
        scan_failures(doc, &plan, PREVIEW_SCAN_CAP)?;

    Ok(DbExportPreview {
        revision: doc.revision(),
        table_exists,
        target_rows,
        columns: plan
            .iter()
            .map(|c| DbExportColumn {
                column_id: c.column_id.clone(),
                name: c.doc_name.clone(),
                sql_name: c.sql_name.clone(),
                sql_type: c.sql_type.decl().to_string(),
                primary_key: c.primary_key,
                target_decl_type: c.target_decl.clone(),
            })
            .collect(),
        blocking,
        failures,
        failure_count,
        rows_scanned,
        scan_complete,
    })
}

// ---------------------------------------------------------------------------
// The write
// ---------------------------------------------------------------------------

fn generated_ddl(table: &str, plan: &[ColumnPlan]) -> String {
    let mut ddl = format!("CREATE TABLE {} (", quote_ident(table));
    for (i, col) in plan.iter().enumerate() {
        if i > 0 {
            ddl.push_str(", ");
        }
        ddl.push_str(&quote_ident(&col.sql_name));
        ddl.push(' ');
        ddl.push_str(col.sql_type.decl());
    }
    let pk: Vec<&ColumnPlan> = plan.iter().filter(|c| c.primary_key).collect();
    if !pk.is_empty() {
        ddl.push_str(", PRIMARY KEY (");
        for (i, col) in pk.iter().enumerate() {
            if i > 0 {
                ddl.push_str(", ");
            }
            ddl.push_str(&quote_ident(&col.sql_name));
        }
        ddl.push(')');
    }
    ddl.push(')');
    ddl
}

fn insert_sql(table: &str, plan: &[ColumnPlan], policy: ConflictPolicy) -> String {
    let cols = plan
        .iter()
        .map(|c| quote_ident(&c.sql_name))
        .collect::<Vec<_>>()
        .join(", ");
    let marks = (1..=plan.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT {}INTO {} ({cols}) VALUES ({marks})",
        policy.verb(),
        quote_ident(table),
    )
}

/// Everything between BEGIN and COMMIT: target validation, DDL, every row.
/// Returning `Err` drops the transaction, which rolls back — the database
/// is left exactly as it was.
fn write_transaction(
    conn: &mut Connection,
    doc: &Document,
    plan: &mut [ColumnPlan],
    table: &str,
    mode: ExportMode,
    policy: ConflictPolicy,
    ctx: Option<&JobCtx>,
) -> AppResult<DbExportResult> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let check = check_target(&tx, table, plan, mode)?;
    if !check.issues.is_empty() {
        return Err(AppError::invalid(check.issues.join("; ")));
    }
    match mode {
        ExportMode::Create => tx.execute_batch(&generated_ddl(table, plan))?,
        ExportMode::Replace => {
            // DROP + CREATE inside the SAME transaction: a failure later in
            // the write rolls the drop back too, keeping the original table.
            tx.execute_batch(&format!("DROP TABLE IF EXISTS {}", quote_ident(table)))?;
            tx.execute_batch(&generated_ddl(table, plan))?;
        }
        ExportMode::Append => {}
    }

    let plan: &[ColumnPlan] = plan;
    let n_rows = doc.n_rows();
    if let Some(ctx) = ctx {
        ctx.set_total(n_rows as u64);
        ctx.set_message(format!("Writing {table}"));
        ctx.check()?;
    }

    let mut written = 0u64;
    let mut skipped = 0u64;
    {
        let sql = insert_sql(table, plan, policy);
        let mut stmt = tx.prepare(&sql)?;
        let mut since = 0u64;
        doc.visit_rows(0..n_rows, &mut |i, row| {
            let mut values: Vec<Value> = Vec::with_capacity(plan.len());
            for col in plan {
                let raw = row.get(col.doc_col).map(String::as_str).unwrap_or("");
                let value = convert_cell(col, raw).map_err(|reason| {
                    AppError::invalid(format!(
                        "row {}, column \"{}\": {reason} — nothing was written \
                         (the transaction was rolled back)",
                        i + 1,
                        col.sql_name,
                    ))
                })?;
                values.push(value);
            }
            match stmt.execute(rusqlite::params_from_iter(values)) {
                // INSERT OR IGNORE reports 0 changed rows for a skip.
                Ok(0) => skipped += 1,
                Ok(_) => written += 1,
                Err(rusqlite::Error::SqliteFailure(f, msg))
                    if f.code == rusqlite::ffi::ErrorCode::ConstraintViolation =>
                {
                    return Err(AppError::invalid(format!(
                        "row {} violates the table's constraints ({}) — nothing was written; \
                         choose the \"skip\" or \"replace\" conflict policy to handle key \
                         conflicts",
                        i + 1,
                        msg.unwrap_or_else(|| "constraint violation".into()),
                    )));
                }
                Err(e) => return Err(e.into()),
            }
            since += 1;
            if since == CANCEL_EVERY {
                if let Some(ctx) = ctx {
                    ctx.advance(since)?;
                }
                since = 0;
            }
            Ok(true)
        })?;
        if let Some(ctx) = ctx {
            ctx.advance(since)?;
            ctx.flush_progress();
        }
    }

    tx.commit()?;
    Ok(DbExportResult {
        table: table.to_string(),
        mode: mode.as_str().to_string(),
        rows_written: written,
        rows_skipped: skipped,
    })
}

/// Run one export end to end: revision guard, plan resolution, ONE
/// transaction (validate → DDL → rows → commit). Any failure rolls back and
/// leaves the database file unchanged; a file newly created for `create`
/// mode is removed again on failure. The document read lock is held
/// throughout, so the exported rows are exactly one revision.
pub(crate) fn run_export(
    handle: &SharedDocument,
    expected_revision: u64,
    spec: &DbExportSpec,
    ctx: Option<&JobCtx>,
) -> AppResult<DbExportResult> {
    let mode = ExportMode::parse(&spec.mode)?;
    let policy = ConflictPolicy::parse(&spec.conflict_policy)?;
    let table = validate_table_name(&spec.table)?;
    if mode == ExportMode::Replace && !spec.confirm_replace {
        return Err(AppError::invalid(
            "replacing a table requires explicit confirmation",
        ));
    }

    let doc = handle
        .read()
        .map_err(|_| AppError::Other("internal document lock error".into()))?;
    doc.check_revision(expected_revision)?;
    let mut plan = base_plan(&doc, spec)?;

    let path = Path::new(&spec.path);
    let existed = path.exists();
    if !existed && mode != ExportMode::Create {
        return Err(AppError::invalid(format!(
            "database file does not exist: {}",
            spec.path
        )));
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if mode == ExportMode::Create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut conn = Connection::open_with_flags(path, flags)
        .map_err(|e| AppError::invalid(format!("cannot open database for writing: {e}")))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    if existed {
        // Fail with a clear message before BEGIN when the file is not SQLite.
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| {
            AppError::invalid(format!(
                "'{}' does not look like a SQLite database: {e}",
                spec.path
            ))
        })?;
    }
    if let Some(ctx) = ctx {
        // Long statements (DROP of a huge table, constraint rebuilds) abort
        // through the progress handler; the per-row checks handle the rest.
        safe_query::install_cancel_handler(&conn, ctx.cancel_token())?;
    }

    let result = write_transaction(&mut conn, &doc, &mut plan, table, mode, policy, ctx);
    drop(conn);
    if result.is_err() && !existed {
        // `create` made the file; do not leave a stub behind on failure.
        let _ = std::fs::remove_file(path);
    }
    result
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Reports of finished exports, keyed by job id (fetched once via
/// [`db_export_report`] after the `job-finished` event, then dropped).
#[derive(Default)]
pub struct DbExportReports(Mutex<HashMap<u64, DbExportResult>>);

impl DbExportReports {
    fn store(&self, job_id: u64, result: DbExportResult) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(job_id, result);
        }
    }

    fn take(&self, job_id: u64) -> Option<DbExportResult> {
        self.0.lock().ok().and_then(|mut map| map.remove(&job_id))
    }
}

/// Mapping + conversion-failure preview for exporting `doc_id` into a
/// SQLite database. Picking the target in the file dialog is the user's
/// approval — the path joins the SafeQueryEngine registry (like `db_open`).
#[tauri::command]
pub async fn db_export_preview(
    doc_id: u64,
    spec: DbExportSpec,
    app: tauri::AppHandle,
) -> AppResult<DbExportPreview> {
    tauri::async_runtime::spawn_blocking(move || {
        if Path::new(&spec.path).exists() {
            app.state::<ApprovedSources>()
                .approve(Path::new(&spec.path))?;
        }
        let handle = app
            .state::<std::sync::Mutex<AppState>>()
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?
            .doc(doc_id)?;
        let doc = handle
            .read()
            .map_err(|_| AppError::Other("internal document lock error".into()))?;
        build_preview(&doc, &spec)
    })
    .await
    .map_err(blocking_err)?
}

/// Start the export as a cancellable background job (kind `dbExport`).
/// Guarded by `expected_revision` (echoed from the preview); the result is
/// fetched with [`db_export_report`] after the `job-finished` event.
#[tauri::command]
pub async fn start_db_export(
    doc_id: u64,
    expected_revision: u64,
    spec: DbExportSpec,
    app: tauri::AppHandle,
    state: State<'_, std::sync::Mutex<AppState>>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    // Malformed specs and stale revisions fail the invoke, not the job.
    ExportMode::parse(&spec.mode)?;
    ConflictPolicy::parse(&spec.conflict_policy)?;
    validate_table_name(&spec.table)?;
    let handle = state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .doc(doc_id)?;
    handle
        .read()
        .map_err(|_| AppError::Other("internal document lock error".into()))?
        .check_revision(expected_revision)?;

    let ctx = jobs.begin_for_app(&app, "dbExport", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let result = run_export(&handle, expected_revision, &spec, Some(ctx))?;
            // The write succeeded, so the file exists: record the user's
            // approval of the target (see the module docs).
            let _ = app_for_job
                .state::<ApprovedSources>()
                .approve(Path::new(&spec.path));
            app_for_job.state::<DbExportReports>().store(job_id, result);
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Fetch (and consume) the report of a finished export job.
#[tauri::command]
pub fn db_export_report(
    job_id: u64,
    reports: State<'_, DbExportReports>,
) -> AppResult<DbExportResult> {
    reports
        .take(job_id)
        .ok_or_else(|| AppError::invalid(format!("no export report for job {job_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    /// Editable document from CSV with declared F31 types on some columns.
    fn shared_doc(csv: &str, types: &[(usize, LogicalType)]) -> SharedDocument {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        let mut doc = Document::from_parsed(1, None, parsed, true);
        let ids = doc.column_ids().to_vec();
        let headers = doc.headers().to_vec();
        for &(c, lt) in types {
            doc.set_column_schema(ColumnSchema::new(ids[c].clone(), headers[c].clone(), lt));
        }
        Arc::new(RwLock::new(doc))
    }

    fn spec_for(path: &Path, table: &str, mode: &str) -> DbExportSpec {
        DbExportSpec {
            path: path.to_string_lossy().to_string(),
            table: table.to_string(),
            mode: mode.to_string(),
            mappings: Vec::new(),
            conflict_policy: "abort".to_string(),
            confirm_replace: false,
        }
    }

    fn export(handle: &SharedDocument, spec: &DbExportSpec) -> AppResult<DbExportResult> {
        let revision = handle.read().unwrap().revision();
        run_export(handle, revision, spec, None)
    }

    fn preview(handle: &SharedDocument, spec: &DbExportSpec) -> DbExportPreview {
        build_preview(&handle.read().unwrap(), spec).unwrap()
    }

    /// All rows of a table via an independent connection, NULLs kept.
    fn table_rows(path: &Path, table: &str, order_by: &str) -> Vec<Vec<Option<String>>> {
        let conn = Connection::open(path).unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT * FROM {} ORDER BY {order_by}",
                quote_ident(table)
            ))
            .unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                let mut cells = Vec::with_capacity(n);
                for i in 0..n {
                    cells.push(safe_query::value_to_text(r.get_ref(i)?));
                }
                Ok(cells)
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows
    }

    fn existing_db(dir: &Path) -> PathBuf {
        let path = dir.join("target.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, qty INTEGER NOT NULL, note TEXT);
             INSERT INTO items VALUES (1, 10, 'existing');",
        )
        .unwrap();
        path
    }

    // ----- mapping resolution ----------------------------------------------

    #[test]
    fn default_mapping_uses_declared_schema_with_text_fallback() {
        let handle = shared_doc(
            "id,price,when,plain\n1,2.5,2024-01-01,x\n",
            &[
                (0, LogicalType::Integer),
                (1, LogicalType::Decimal),
                (2, LogicalType::Date),
            ],
        );
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_for(&dir.path().join("new.sqlite"), "t", "create");
        let p = preview(&handle, &spec);
        let types: Vec<&str> = p.columns.iter().map(|c| c.sql_type.as_str()).collect();
        assert_eq!(types, vec!["INTEGER", "NUMERIC", "TEXT", "TEXT"]);
        assert!(p.blocking.is_empty());
        assert!(!p.table_exists);
        assert!(p.scan_complete);
        assert_eq!(p.failure_count, 0);
    }

    #[test]
    fn duplicate_and_empty_headers_get_unique_sql_names() {
        let handle = shared_doc("a,A,\n1,2,3\n", &[]);
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_for(&dir.path().join("new.sqlite"), "t", "create");
        let p = preview(&handle, &spec);
        let names: Vec<&str> = p.columns.iter().map(|c| c.sql_name.as_str()).collect();
        // Case-insensitive uniqueness (SQLite identifier semantics).
        assert_eq!(names, vec!["a", "A (2)", "column_3"]);
        // And the export actually succeeds with those names.
        assert!(export(&handle, &spec).is_ok());
    }

    #[test]
    fn malformed_mappings_are_rejected() {
        let handle = shared_doc("a,b\n1,2\n", &[]);
        let ids = handle.read().unwrap().column_ids().to_vec();
        let dir = tempfile::tempdir().unwrap();

        let mut spec = spec_for(&dir.path().join("new.sqlite"), "t", "create");
        spec.mappings = vec![DbColumnMapIn {
            column_id: "nope".into(),
            sql_name: None,
            sql_type: None,
            primary_key: false,
        }];
        assert!(export(&handle, &spec)
            .unwrap_err()
            .to_string()
            .contains("unknown column id"));

        spec.mappings = vec![DbColumnMapIn {
            column_id: ids[0].clone(),
            sql_name: None,
            sql_type: Some("VARCHAR".into()),
            primary_key: false,
        }];
        assert!(export(&handle, &spec)
            .unwrap_err()
            .to_string()
            .contains("unknown SQL type"));

        // Case-insensitive rename collision.
        spec.mappings = vec![DbColumnMapIn {
            column_id: ids[1].clone(),
            sql_name: Some("A".into()),
            sql_type: None,
            primary_key: false,
        }];
        assert!(export(&handle, &spec)
            .unwrap_err()
            .to_string()
            .contains("duplicate SQL column name"));

        // Reserved table names never reach SQL.
        let spec = spec_for(&dir.path().join("new.sqlite"), "sqlite_master", "create");
        assert!(export(&handle, &spec)
            .unwrap_err()
            .to_string()
            .contains("reserved"));
    }

    // ----- create -----------------------------------------------------------

    #[test]
    fn create_writes_typed_table_with_pk_and_nulls() {
        let handle = shared_doc(
            "id,price,flag,note\n1,10.50,true,hello\n2,,false,\n",
            &[
                (0, LogicalType::Integer),
                (1, LogicalType::Decimal),
                (2, LogicalType::Boolean),
            ],
        );
        let ids = handle.read().unwrap().column_ids().to_vec();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.sqlite");
        let mut spec = spec_for(&path, "out", "create");
        spec.mappings = vec![DbColumnMapIn {
            column_id: ids[0].clone(),
            sql_name: None,
            sql_type: None,
            primary_key: true,
        }];
        let result = export(&handle, &spec).unwrap();
        assert_eq!(result.rows_written, 2);
        assert_eq!(result.rows_skipped, 0);
        assert_eq!(result.mode, "create");

        // Declared column types and the PK round-trip through pragmas.
        let conn = Connection::open(&path).unwrap();
        let cols: Vec<(String, String, i64)> = conn
            .prepare("SELECT name, type, pk FROM pragma_table_info('out') ORDER BY cid")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            cols,
            vec![
                ("id".into(), "INTEGER".into(), 1),
                ("price".into(), "NUMERIC".into(), 0),
                ("flag".into(), "BOOLEAN".into(), 0),
                ("note".into(), "TEXT".into(), 0),
            ]
        );
        drop(conn);

        let rows = table_rows(&path, "out", "id");
        // Typed empties become SQL NULL; TEXT empties stay ''.
        assert_eq!(
            rows,
            vec![
                vec![
                    Some("1".into()),
                    Some("10.5".into()), // NUMERIC affinity on the canonical text
                    Some("1".into()),    // booleans stored as 0/1
                    Some("hello".into()),
                ],
                vec![Some("2".into()), None, Some("0".into()), Some("".into())],
            ]
        );
    }

    #[test]
    fn create_refuses_existing_table_and_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let handle = shared_doc("id,qty,note\n9,9,new\n", &[]);
        let before = std::fs::read(&path).unwrap();
        let err = export(&handle, &spec_for(&path, "items", "create")).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "file untouched");
    }

    #[test]
    fn failed_create_into_new_file_removes_the_stub() {
        let handle = shared_doc("n\nnot-a-number\n", &[(0, LogicalType::Integer)]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.sqlite");
        let err = export(&handle, &spec_for(&path, "t", "create")).unwrap_err();
        assert!(err.to_string().contains("row 1"), "{err}");
        assert!(!path.exists(), "no empty stub database is left behind");
    }

    #[test]
    fn integer_out_of_i64_range_is_a_conversion_failure() {
        let handle = shared_doc("n\n99999999999999999999\n", &[(0, LogicalType::Integer)]);
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_for(&dir.path().join("new.sqlite"), "t", "create");
        let p = preview(&handle, &spec);
        assert_eq!(p.failure_count, 1);
        assert!(
            p.failures[0].reason.contains("64-bit"),
            "{}",
            p.failures[0].reason
        );
        assert!(export(&handle, &spec).is_err());
    }

    // ----- conversion-failure preview --------------------------------------

    #[test]
    fn preview_reports_conversion_failures_accurately() {
        let handle = shared_doc(
            "n,t\n1,ok\nabc,also ok\n3,fine\n4.5,text never fails\n",
            &[(0, LogicalType::Integer)],
        );
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_for(&dir.path().join("new.sqlite"), "t", "create");
        let p = preview(&handle, &spec);
        assert_eq!(p.failure_count, 2, "rows 2 and 4 fail integer conversion");
        assert_eq!(p.rows_scanned, 4);
        assert!(p.scan_complete);
        assert_eq!(p.failures.len(), 2);
        assert_eq!(p.failures[0].row, 1, "0-based data row");
        assert_eq!(p.failures[0].column, "n");
        assert_eq!(p.failures[0].value, "abc");
        assert!(!p.failures[0].reason.is_empty());
        assert_eq!(p.failures[1].row, 3);
        // The write refuses at the FIRST failing row with its coordinates.
        let err = export(&handle, &spec).unwrap_err();
        assert!(err.to_string().contains("row 2"), "{err}");
        assert!(err.to_string().contains("\"n\""), "{err}");
    }

    // ----- append -----------------------------------------------------------

    #[test]
    fn append_converts_against_target_declared_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        // Undeclared document columns (all text) map onto the target's
        // INTEGER/INTEGER/TEXT declarations, which drive conversion.
        let handle = shared_doc("id,qty,note\n2,20,two\n3,30,\n", &[]);
        let spec = spec_for(&path, "items", "append");
        let p = preview(&handle, &spec);
        assert!(p.blocking.is_empty(), "{:?}", p.blocking);
        assert!(p.table_exists);
        assert_eq!(p.target_rows, Some(1));
        assert_eq!(
            p.columns
                .iter()
                .map(|c| c.sql_type.as_str())
                .collect::<Vec<_>>(),
            vec!["INTEGER", "INTEGER", "TEXT"],
            "append takes types from the target, not the document"
        );
        assert_eq!(p.columns[0].target_decl_type.as_deref(), Some("INTEGER"));

        let result = export(&handle, &spec).unwrap();
        assert_eq!(result.rows_written, 2);
        let rows = table_rows(&path, "items", "id");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1][1].as_deref(), Some("20"));
        assert_eq!(rows[2][2].as_deref(), Some(""), "TEXT stays verbatim");
    }

    #[test]
    fn append_subset_leaves_unmapped_nullable_columns_null() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        // Only id + qty: `note` is nullable and gets NULL; `id` is the rowid
        // alias so it could even be omitted.
        let handle = shared_doc("id,qty\n5,50\n", &[]);
        let spec = spec_for(&path, "items", "append");
        assert!(preview(&handle, &spec).blocking.is_empty());
        export(&handle, &spec).unwrap();
        let rows = table_rows(&path, "items", "id");
        assert_eq!(rows[1], vec![Some("5".into()), Some("50".into()), None]);
    }

    #[test]
    fn append_incompatibilities_are_refused_with_no_alter() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let schema_before: String = {
            let conn = Connection::open(&path).unwrap();
            conn.query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'items'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Extra document column that the target does not have.
        let extra = shared_doc("id,qty,note,surprise\n7,70,x,y\n", &[]);
        let spec = spec_for(&path, "items", "append");
        let p = preview(&extra, &spec);
        assert!(
            p.blocking.iter().any(|b| b.contains("surprise")),
            "{:?}",
            p.blocking
        );
        let err = export(&extra, &spec).unwrap_err();
        assert!(err.to_string().contains("surprise"), "{err}");

        // Missing NOT NULL column (qty) without a default.
        let missing = shared_doc("id,note\n7,x\n", &[]);
        let p = preview(&missing, &spec);
        assert!(
            p.blocking
                .iter()
                .any(|b| b.contains("qty") && b.contains("NOT NULL")),
            "{:?}",
            p.blocking
        );
        assert!(export(&missing, &spec).is_err());

        // Appending to a missing table is refused (create exists for that).
        let err = export(&extra, &spec_for(&path, "nope", "append")).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");

        // Nothing changed the table or its schema.
        let conn = Connection::open(&path).unwrap();
        let schema_after: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'items'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(schema_before, schema_after, "no implicit ALTER, ever");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    // ----- rollback ---------------------------------------------------------

    #[test]
    fn rollback_on_conversion_failure_leaves_db_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let before = std::fs::read(&path).unwrap();
        // Row 1 converts fine (and IS inserted before row 2 fails): the
        // rollback must take it back out again.
        let handle = shared_doc("id,qty,note\n2,20,ok\n3,BAD,x\n", &[]);
        let err = export(&handle, &spec_for(&path, "items", "append")).unwrap_err();
        assert!(err.to_string().contains("row 2"), "{err}");
        assert!(err.to_string().contains("\"qty\""), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a failed export leaves the database byte-identical"
        );
    }

    // ----- PK conflict policies --------------------------------------------

    #[test]
    fn pk_conflict_abort_rolls_back_everything() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let before = std::fs::read(&path).unwrap();
        // First row is clean and gets inserted; the second collides with the
        // existing id=1. Abort rolls both back.
        let handle = shared_doc("id,qty,note\n8,80,clean\n1,99,collides\n", &[]);
        let err = export(&handle, &spec_for(&path, "items", "append")).unwrap_err();
        assert!(err.to_string().contains("row 2"), "{err}");
        assert!(err.to_string().contains("constraint"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn pk_conflict_skip_keeps_existing_rows_and_counts_skips() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let handle = shared_doc("id,qty,note\n1,99,collides\n2,20,new\n", &[]);
        let mut spec = spec_for(&path, "items", "append");
        spec.conflict_policy = "skip".into();
        let result = export(&handle, &spec).unwrap();
        assert_eq!(result.rows_written, 1);
        assert_eq!(result.rows_skipped, 1);
        let rows = table_rows(&path, "items", "id");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1].as_deref(), Some("10"), "existing row kept");
        assert_eq!(rows[1][2].as_deref(), Some("new"));
    }

    #[test]
    fn pk_conflict_replace_overwrites_conflicting_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let handle = shared_doc("id,qty,note\n1,99,replaced\n", &[]);
        let mut spec = spec_for(&path, "items", "append");
        spec.conflict_policy = "replace".into();
        let result = export(&handle, &spec).unwrap();
        assert_eq!(result.rows_written, 1);
        assert_eq!(result.rows_skipped, 0);
        let rows = table_rows(&path, "items", "id");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1].as_deref(), Some("99"));
        assert_eq!(rows[0][2].as_deref(), Some("replaced"));
    }

    // ----- replace ----------------------------------------------------------

    #[test]
    fn replace_requires_confirmation_and_is_fully_transactional() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let good = shared_doc("id,note\n7,fresh\n", &[(0, LogicalType::Integer)]);

        // 1. No confirmation → refused before anything opens.
        let spec = spec_for(&path, "items", "replace");
        let err = export(&good, &spec).unwrap_err();
        assert!(err.to_string().contains("confirmation"), "{err}");

        // 2. Confirmed but failing mid-write → the DROP is rolled back too:
        //    the ORIGINAL table survives intact.
        let before = std::fs::read(&path).unwrap();
        let bad = shared_doc("id,note\nNOPE,x\n", &[(0, LogicalType::Integer)]);
        let mut spec = spec_for(&path, "items", "replace");
        spec.confirm_replace = true;
        let err = export(&bad, &spec).unwrap_err();
        assert!(err.to_string().contains("row 1"), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a failed replace keeps the original table byte-identical"
        );

        // 3. Confirmed and clean → old table gone, new schema + rows in.
        let result = export(&good, &spec).unwrap();
        assert_eq!(result.rows_written, 1);
        let rows = table_rows(&path, "items", "id");
        assert_eq!(rows, vec![vec![Some("7".into()), Some("fresh".into())]]);
        let conn = Connection::open(&path).unwrap();
        let cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('items')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cols, 2, "replace installed the new two-column schema");
    }

    // ----- cancellation -----------------------------------------------------

    #[test]
    fn cancelled_export_rolls_back_and_db_stays_usable() {
        let dir = tempfile::tempdir().unwrap();
        let path = existing_db(dir.path());
        let before = std::fs::read(&path).unwrap();
        let handle = shared_doc("id,qty,note\n2,20,x\n3,30,y\n", &[]);
        let registry = JobRegistry::default();
        let ctx = registry.begin("dbExport", None, |_| {});
        registry.cancel(ctx.id);
        let revision = handle.read().unwrap().revision();
        let result = run_export(
            &handle,
            revision,
            &spec_for(&path, "items", "append"),
            Some(&ctx),
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // The file is immediately writable by others afterwards.
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("INSERT INTO items VALUES (42, 1, 'after-cancel')", [])
            .unwrap();
    }

    // ----- integration with read-only db-backed documents -------------------

    #[test]
    fn virtual_backed_document_exports_to_another_database() {
        // Open a table read-only from database A (never materialised),
        // export it into a new database B: the windowed visit path feeds the
        // write directly.
        let dir = tempfile::tempdir().unwrap();
        let src = existing_db(dir.path());
        let canonical = std::fs::canonicalize(&src).unwrap();
        let (headers, backing) =
            crate::db_browser::build_table_backing(&canonical, "items", None).unwrap();
        let doc = Document::from_virtual(3, "a → items".into(), headers, Box::new(backing));
        let handle: SharedDocument = Arc::new(RwLock::new(doc));

        let dest = dir.path().join("copy.sqlite");
        let result = export(&handle, &spec_for(&dest, "items_copy", "create")).unwrap();
        assert_eq!(result.rows_written, 1);
        let rows = table_rows(&dest, "items_copy", "id");
        assert_eq!(
            rows,
            vec![vec![
                Some("1".into()),
                Some("10".into()),
                Some("existing".into()),
            ]]
        );
    }

    #[test]
    fn stale_revision_refuses_the_export() {
        let handle = shared_doc("a\n1\n", &[]);
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_for(&dir.path().join("new.sqlite"), "t", "create");
        let stale = handle.read().unwrap().revision();
        handle.write().unwrap().set_cell(0, 0, "2".into()).unwrap();
        let result = run_export(&handle, stale, &spec, None);
        assert!(matches!(result, Err(AppError::StaleRevision { .. })));
    }
}
