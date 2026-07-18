//! F36 sandboxed SQL query workspace.
//!
//! Queries run through the F35 SafeQueryEngine ([`crate::safe_query`]):
//! read-only connections, deny-by-default authorizer, document virtual
//! tables with revision snapshot semantics. This module adds the workspace
//! surface on top:
//!
//! * **Approved-file tables** — a user-picked CSV / JSON / Parquet / Arrow
//!   file becomes a queryable table WITHOUT a full user-facing import. The
//!   file must be explicitly approved through the same
//!   [`ApprovedSources`] registry F35 database files use; the backing is a
//!   [`TabularSource`] served through the generic `ceesvee_src` virtual
//!   table (bounded, windowed reads; a mid-query rewrite aborts the query).
//!   Backings per format: Parquet/Arrow read in place through the F32
//!   [`ColumnarHandle`]; CSV parses in memory when small and index-spills
//!   (F10 machinery) when large; JSON/JSONL stream through the F33 import
//!   engine into an internal bounded document (in-memory under the spill
//!   budget, indexed above it) because JSON documents are not
//!   record-seekable. JSON `null`/missing narrow to the import engine's
//!   tokens (text plane); Parquet/Arrow NULL surfaces as SQL `NULL`.
//! * **Statement pre-validation** — single statement only (multi-statement
//!   input is rejected at prepare), leading-keyword whitelist (`SELECT`,
//!   `WITH`, `VALUES`, `EXPLAIN`), `sqlite3_stmt_readonly` assertion; the
//!   F35 authorizer remains underneath as defence in depth (it denies DML
//!   hidden behind CTEs or `EXPLAIN` at prepare time, before anything runs),
//!   and the read-only open flag underneath that.
//! * **Typed named parameters** — `:name` placeholders with a typed value
//!   model, always BOUND via SQLite parameter binding, never interpolated
//!   into SQL text. Missing, mistyped, unused and positional (`?`)
//!   parameters are rejected before the query runs.
//! * **Bounded execution** — row limit, byte limit and time limit enforced
//!   DURING streaming; per-row progress (rows produced) and cooperative
//!   cancellation through the job registry plus a SQLite progress handler.
//! * **Results** — one bounded in-memory result spool per workspace
//!   (replaced by the next run): windowed reads to the front end,
//!   materialization as a NEW derived document (editable or indexed by
//!   size), or direct CSV export through the atomic-save pipeline.
//! * **History** — a settings-persisted ring buffer (cap
//!   [`SQL_HISTORY_CAP`]); entries are data, NEVER auto-executed.
//! * **Saved queries** — the F37 project `queries` section
//!   ([`SavedQuery`]): definitions only (sql, typed params, source refs),
//!   never results, never auto-run — loading a project stores them
//!   verbatim, exactly like recipes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::derived::{self, DerivedDocumentBuilder};
use crate::document::Document;
use crate::dto::{ExportOptions, IndexedOpenStart};
use crate::error::{AppError, AppResult};
use crate::index;
use crate::job::{CancelToken, JobCtx, JobRegistry};
use crate::json_import::{self, JsonImportOptions};
use crate::parquet_arrow::{self, ColumnarFormat, ColumnarHandle, ColumnarOpenOptions};
use crate::parse::{self, ParseSettings};
use crate::safe_query::{self, value_to_text, ApprovedSources, SharedTabular};
use crate::settings;
use crate::state::{AppState, SharedDocument};
use crate::tabular::{
    self, ContentFingerprint, CsvSink, DocumentSource, RowCountHint, TabularColumn, TabularRow,
    TabularSource,
};

/// Default / hard result-row caps (the grid never needs more; a runaway
/// cross join must not exhaust memory on the 6 GB target machine).
const DEFAULT_MAX_ROWS: usize = 100_000;
const HARD_MAX_ROWS: usize = 1_000_000;
/// Default / hard result-byte caps (the spool is held in memory).
const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Default / hard time limits.
const DEFAULT_TIME_LIMIT_MS: u64 = 60_000;
const HARD_TIME_LIMIT_MS: u64 = 600_000;
/// Rows returned inline with the run summary; further windows are fetched
/// with `sql_result_rows` (bounded windows to React only).
const FIRST_WINDOW_ROWS: usize = 200;
/// Result-window fetch cap.
const RESULT_WINDOW_MAX: usize = 10_000;
/// Schema-browser DTO bounds (autocomplete payloads stay small).
const SCHEMA_MAX_COLUMNS: usize = 512;
const SCHEMA_MAX_TABLES: usize = 500;
/// CSV files at or under this parse fully in memory; larger ones open
/// through the F10 record index (windowed reads, bounded memory).
const CSV_MEMORY_BUDGET: u64 = 64 * 1024 * 1024;
/// SQLite VM steps between guard polls (cancellation + deadline).
const PROGRESS_STEP_OPS: std::ffi::c_int = 4_000;
/// Cancellation check cadence in row loops.
const CANCEL_EVERY: usize = 1024;
/// Upper bound on EXPLAIN QUERY PLAN nodes collected. The planner's own
/// compile-time limits already bound plan size, but the tree is shipped to the
/// front end, so cap it like every other schema payload (defence in depth).
const EXPLAIN_MAX_NODES: usize = 20_000;
/// History ring capacity (most recent first).
pub const SQL_HISTORY_CAP: usize = 100;

// ---------------------------------------------------------------------------
// Wire DTOs
// ---------------------------------------------------------------------------

/// Parameter value types the typed editor offers. Bind narrowing:
/// integer → SQL INTEGER, float/decimal → SQL REAL (SQLite has no exact
/// decimal type; decimal input is validated strictly — sign, digits,
/// optional fraction, no exponent — then bound as REAL), boolean → 0/1,
/// date/datetime → canonical ISO text (matching how the tabular text plane
/// stores them), null → SQL NULL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SqlParamType {
    Text,
    Integer,
    Decimal,
    Float,
    Boolean,
    Date,
    Datetime,
    Null,
}

/// One typed named parameter (`:name`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlParam {
    /// Name WITHOUT the leading `:`.
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: SqlParamType,
    #[serde(default)]
    pub value: Option<String>,
}

/// Execution caps for one run; omitted fields use defaults, everything is
/// clamped to the hard caps.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SqlLimits {
    pub max_rows: Option<usize>,
    pub max_bytes: Option<u64>,
    pub time_limit_ms: Option<u64>,
}

/// Resolved, clamped limits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedLimits {
    pub max_rows: usize,
    pub max_bytes: u64,
    pub time_limit: Duration,
}

impl SqlLimits {
    pub(crate) fn resolve(self) -> ResolvedLimits {
        ResolvedLimits {
            max_rows: self
                .max_rows
                .unwrap_or(DEFAULT_MAX_ROWS)
                .clamp(1, HARD_MAX_ROWS),
            max_bytes: self
                .max_bytes
                .unwrap_or(DEFAULT_MAX_BYTES)
                .clamp(1, HARD_MAX_BYTES),
            time_limit: Duration::from_millis(
                self.time_limit_ms
                    .unwrap_or(DEFAULT_TIME_LIMIT_MS)
                    .clamp(1, HARD_TIME_LIMIT_MS),
            ),
        }
    }
}

/// One open document to expose as a table.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlDocRef {
    pub doc_id: u64,
    /// Optional alias override; sanitized and de-duplicated either way.
    #[serde(default)]
    pub alias: Option<String>,
}

/// Which sources a query (or the schema browser) composes: open documents,
/// registered approved files, and at most ONE approved SQLite database
/// (attached as the main schema — `ATTACH` stays denied, so multi-database
/// queries are deliberately out of scope).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SqlSourceSelection {
    pub documents: Vec<SqlDocRef>,
    /// Registered file aliases (see `sql_register_file`).
    pub files: Vec<String>,
    /// Path of an F35-approved SQLite database.
    pub database: Option<String>,
}

/// One run request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlRunRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<SqlParam>,
    #[serde(default)]
    pub sources: SqlSourceSelection,
    #[serde(default)]
    pub limits: SqlLimits,
}

/// One column of a schema-browser table entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlColumnInfo {
    pub name: String,
    /// Declared/logical type label ("text", "integer", …) for display only.
    pub decl_type: String,
}

/// One queryable table for the schema browser / autocomplete list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlTableInfo {
    /// The name to use in SQL.
    pub alias: String,
    /// Human label (file name / table name).
    pub label: String,
    /// "document" | "csv" | "json" | "parquet" | "arrow" | "table" | "view".
    pub kind: String,
    pub columns: Vec<SqlColumnInfo>,
    /// True when the column list was capped at [`SCHEMA_MAX_COLUMNS`].
    pub columns_truncated: bool,
    pub row_count: Option<u64>,
    /// Backing file path, for registered files.
    pub path: Option<String>,
}

/// Everything the schema browser / autocomplete needs (bounded).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlSchemaDto {
    pub documents: Vec<SqlTableInfo>,
    pub files: Vec<SqlTableInfo>,
    pub database: Vec<SqlTableInfo>,
    /// True when the database table list was capped.
    pub database_truncated: bool,
}

/// Prepare-only dry run outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlValidation {
    pub ok: bool,
    pub error: Option<String>,
    /// Output columns, when the statement prepares.
    pub columns: Vec<String>,
    /// Named parameters the statement uses (without `:`).
    pub parameters: Vec<String>,
}

/// One node of the `EXPLAIN QUERY PLAN` tree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlPlanNode {
    pub id: i64,
    pub detail: String,
    pub children: Vec<SqlPlanNode>,
}

/// Summary returned by a run: identity of the stored spool plus the first
/// bounded window of rows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlRunSummary {
    pub result_id: u64,
    pub columns: Vec<String>,
    /// The first [`FIRST_WINDOW_ROWS`] rows (`null` = SQL NULL).
    pub rows: Vec<Vec<Option<String>>>,
    pub row_count: u64,
    pub truncated: bool,
    pub byte_count: u64,
    pub elapsed_ms: u64,
}

/// One window of a stored result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlResultWindow {
    pub start: usize,
    pub rows: Vec<Vec<Option<String>>>,
    pub total: u64,
    pub truncated: bool,
}

/// One history entry (settings-persisted; data only, never auto-executed —
/// re-running is always an explicit user action).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlHistoryEntry {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<SqlParam>,
    /// Aliases / paths of the sources the query ran against.
    #[serde(default)]
    pub sources: Vec<String>,
    pub ran_at_ms: u64,
    /// "done" | "failed" | "cancelled".
    pub status: String,
    #[serde(default)]
    pub row_count: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// F37 project `queries` section payload: DEFINITIONS ONLY — sql text,
/// typed parameters and source references. Never results, and never
/// auto-executed: loading a project stores these verbatim (garbage SQL loads
/// fine and only fails when the user explicitly runs it), exactly like
/// recipes. Unknown fields written by future versions round-trip through
/// `unknown`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedQuery {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sql: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<SqlParam>,
    /// Project source ids / registered file aliases the query reads.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(flatten)]
    pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Approved-file backings
// ---------------------------------------------------------------------------

/// Detected format of a registered file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Csv,
    Json,
    Parquet,
    Arrow,
}

impl FileKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            FileKind::Csv => "csv",
            FileKind::Json => "json",
            FileKind::Parquet => "parquet",
            FileKind::Arrow => "arrow",
        }
    }
}

/// Detect a file's kind: columnar magic bytes first (authoritative), then
/// the JSON extensions, CSV/delimited otherwise.
pub(crate) fn detect_file_kind(path: &Path) -> FileKind {
    match parquet_arrow::detect_format(path) {
        Ok(ColumnarFormat::Parquet) => FileKind::Parquet,
        Ok(_) => FileKind::Arrow,
        Err(_) => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if matches!(ext.as_str(), "json" | "jsonl" | "ndjson") {
                FileKind::Json
            } else {
                FileKind::Csv
            }
        }
    }
}

/// The owned backing behind one registered file table.
pub(crate) enum FileBacking {
    /// An internal (never registered / never editable-from-UI) document:
    /// in-memory or index-backed CSV, or a JSON import result.
    Doc(Box<Document>),
    /// A Parquet/Arrow file read in place (F32 windowed block reads).
    Columnar(Box<ColumnarHandle>),
}

/// [`TabularSource`] over one registered file, whatever its backing.
pub(crate) struct FileTabular {
    backing: FileBacking,
}

impl TabularSource for FileTabular {
    fn columns(&self) -> Vec<TabularColumn> {
        match &self.backing {
            FileBacking::Doc(d) => DocumentSource::new(d).columns(),
            FileBacking::Columnar(h) => parquet_arrow::ColumnarSource::new(h).columns(),
        }
    }

    fn has_header_row(&self) -> bool {
        match &self.backing {
            FileBacking::Doc(d) => DocumentSource::new(d).has_header_row(),
            FileBacking::Columnar(_) => true,
        }
    }

    fn row_count(&self) -> RowCountHint {
        match &self.backing {
            FileBacking::Doc(d) => DocumentSource::new(d).row_count(),
            FileBacking::Columnar(h) => parquet_arrow::ColumnarSource::new(h).row_count(),
        }
    }

    fn read_rows(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>> {
        match &self.backing {
            FileBacking::Doc(d) => DocumentSource::new(d).read_rows(offset, limit, ctx),
            FileBacking::Columnar(h) => {
                parquet_arrow::ColumnarSource::new(h).read_rows(offset, limit, ctx)
            }
        }
    }

    fn fingerprint(&self) -> ContentFingerprint {
        match &self.backing {
            FileBacking::Doc(d) => DocumentSource::new(d).fingerprint(),
            FileBacking::Columnar(h) => parquet_arrow::ColumnarSource::new(h).fingerprint(),
        }
    }
}

/// Open the queryable backing for an APPROVED file. The approved-source
/// check is the containment gate: unapproved paths (in any spelling — the
/// registry compares canonicalized paths) are refused before any byte is
/// read. `pointer` optionally selects the record array of a JSON object
/// document.
pub(crate) fn open_backing(
    approved: &ApprovedSources,
    path: &Path,
    pointer: Option<&str>,
    cache_root: &Path,
    ctx: Option<&JobCtx>,
) -> AppResult<(PathBuf, FileKind, FileTabular)> {
    let canonical = approved.check(path)?;
    let kind = detect_file_kind(&canonical);
    let backing = match kind {
        FileKind::Parquet | FileKind::Arrow => {
            if pointer.is_some_and(|p| !p.is_empty()) {
                return Err(AppError::invalid("JSON Pointers only apply to JSON files"));
            }
            let file =
                parquet_arrow::open_indexed(&canonical, &ColumnarOpenOptions::default(), ctx)?;
            FileBacking::Columnar(Box::new(file.handle))
        }
        FileKind::Json => {
            let options = JsonImportOptions {
                pointer: pointer.map(str::to_string),
                ..JsonImportOptions::default()
            };
            // Internal document (id 0, never registered in AppState): bounded
            // by the derived spill budget, index-backed above it.
            let doc = json_import::import(&canonical, &options, cache_root, 0, ctx)?;
            FileBacking::Doc(Box::new(doc))
        }
        FileKind::Csv => {
            if pointer.is_some_and(|p| !p.is_empty()) {
                return Err(AppError::invalid("JSON Pointers only apply to JSON files"));
            }
            let size = std::fs::metadata(&canonical)?.len();
            if size <= CSV_MEMORY_BUDGET {
                let bytes = std::fs::read(&canonical)?;
                let parsed = parse::parse(&bytes, &ParseSettings::default())?;
                // First row is treated as the header (the registration flow
                // has no header prompt this cycle).
                FileBacking::Doc(Box::new(Document::from_parsed(
                    0,
                    Some(canonical.clone()),
                    parsed,
                    true,
                )))
            } else {
                let settings = index::IndexSettings {
                    delimiter: None,
                    encoding: None,
                    has_header_row: Some(true),
                    chunk_size: 0,
                };
                let indexed = index::build_index(
                    &canonical,
                    cache_root,
                    &settings,
                    &mut |delta| match ctx {
                        Some(ctx) => ctx.advance(delta),
                        None => Ok(()),
                    },
                )?;
                FileBacking::Doc(Box::new(Document::from_index(
                    0,
                    Some(canonical.clone()),
                    indexed,
                )))
            }
        }
    };
    Ok((canonical, kind, FileTabular { backing }))
}

// ---------------------------------------------------------------------------
// Table-name sanitisation
// ---------------------------------------------------------------------------

/// Deterministic SQL-friendly table name: lowercase alphanumerics, runs of
/// anything else collapsed to `_`, guaranteed non-empty and not
/// digit-leading.
pub(crate) fn sanitize_table_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_underscore = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    let base = if trimmed.is_empty() {
        "table".to_string()
    } else {
        trimmed
    };
    if base.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("t_{base}")
    } else {
        base
    }
}

/// Make `base` unique among `taken` by suffixing `_2`, `_3`, ….
pub(crate) fn unique_table_alias(taken: &[String], base: &str) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    for i in 2.. {
        let candidate = format!("{base}_{i}");
        if !taken.iter().any(|t| t == &candidate) {
            return candidate;
        }
    }
    unreachable!("counter is unbounded")
}

// ---------------------------------------------------------------------------
// Workspace state
// ---------------------------------------------------------------------------

/// One registered approved file.
pub(crate) struct FileEntry {
    pub alias: String,
    /// Canonical approved path.
    pub path: PathBuf,
    pub kind: FileKind,
    pub backing: Arc<FileTabular>,
}

/// The stored result spool of the last run (bounded by the run's byte cap).
pub(crate) struct StoredResult {
    pub id: u64,
    pub columns: Vec<String>,
    pub rows: Vec<TabularRow>,
    pub truncated: bool,
    pub bytes: u64,
}

/// [`TabularSource`] view of a stored result, for export / materialization.
struct StoredResultSource<'a>(&'a StoredResult);

impl TabularSource for StoredResultSource<'_> {
    fn columns(&self) -> Vec<TabularColumn> {
        self.0
            .columns
            .iter()
            .enumerate()
            .map(|(i, name)| TabularColumn {
                name: name.clone(),
                id: Some(format!("col-{i}")),
                schema: None,
            })
            .collect()
    }

    fn row_count(&self) -> RowCountHint {
        RowCountHint::Exact(self.0.rows.len() as u64)
    }

    fn read_rows(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>> {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let n = self.0.rows.len();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(n);
        let end = start.saturating_add(limit).min(n);
        Ok(self.0.rows[start..end].to_vec())
    }

    fn fingerprint(&self) -> ContentFingerprint {
        ContentFingerprint::Unknown
    }
}

/// Process-wide workspace state, managed by Tauri: the registered approved
/// files and the single stored result spool (replaced on every run).
#[derive(Default)]
pub struct SqlWorkspace {
    files: Mutex<Vec<FileEntry>>,
    result: Mutex<Option<Arc<StoredResult>>>,
    next_result_id: AtomicU64,
}

impl SqlWorkspace {
    fn lock_files(&self) -> AppResult<std::sync::MutexGuard<'_, Vec<FileEntry>>> {
        self.files
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))
    }

    /// Register (or refresh) a file backing; the alias is stable across
    /// re-registration of the same canonical path.
    pub(crate) fn register(
        &self,
        canonical: PathBuf,
        kind: FileKind,
        backing: FileTabular,
    ) -> AppResult<SqlTableInfo> {
        let mut files = self.lock_files()?;
        let backing = Arc::new(backing);
        if let Some(existing) = files.iter_mut().find(|e| e.path == canonical) {
            existing.kind = kind;
            existing.backing = backing;
            return Ok(file_table_info(existing));
        }
        let stem = canonical
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let taken: Vec<String> = files.iter().map(|e| e.alias.clone()).collect();
        let alias = unique_table_alias(&taken, &sanitize_table_name(stem));
        let entry = FileEntry {
            alias,
            path: canonical,
            kind,
            backing,
        };
        let info = file_table_info(&entry);
        files.push(entry);
        Ok(info)
    }

    /// Forget a registered file, returning its canonical path.
    pub(crate) fn unregister(&self, alias: &str) -> AppResult<PathBuf> {
        let mut files = self.lock_files()?;
        let idx = files
            .iter()
            .position(|e| e.alias == alias)
            .ok_or_else(|| AppError::invalid(format!("no registered file source '{alias}'")))?;
        Ok(files.remove(idx).path)
    }

    pub(crate) fn list_files(&self) -> AppResult<Vec<SqlTableInfo>> {
        Ok(self.lock_files()?.iter().map(file_table_info).collect())
    }

    /// Resolve selected file aliases into vtab exposures.
    pub(crate) fn resolve_files(
        &self,
        aliases: &[String],
    ) -> AppResult<Vec<(String, SharedTabular)>> {
        let files = self.lock_files()?;
        let mut out: Vec<(String, SharedTabular)> = Vec::with_capacity(aliases.len());
        for alias in aliases {
            if out.iter().any(|(a, _)| a == alias) {
                continue; // selecting the same file twice is harmless
            }
            let entry = files.iter().find(|e| &e.alias == alias).ok_or_else(|| {
                AppError::invalid(format!(
                    "no registered file source '{alias}'; register the file first"
                ))
            })?;
            out.push((
                entry.alias.clone(),
                Arc::clone(&entry.backing) as SharedTabular,
            ));
        }
        Ok(out)
    }

    /// Store a run's spool, replacing the previous one.
    pub(crate) fn store_result(
        &self,
        columns: Vec<String>,
        rows: Vec<TabularRow>,
        truncated: bool,
        bytes: u64,
    ) -> AppResult<Arc<StoredResult>> {
        let id = self.next_result_id.fetch_add(1, Ordering::Relaxed) + 1;
        let stored = Arc::new(StoredResult {
            id,
            columns,
            rows,
            truncated,
            bytes,
        });
        *self
            .result
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))? =
            Some(Arc::clone(&stored));
        Ok(stored)
    }

    /// The stored result with `id`, if it is still the current one.
    pub(crate) fn result_arc(&self, id: u64) -> AppResult<Arc<StoredResult>> {
        self.result
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?
            .as_ref()
            .filter(|r| r.id == id)
            .cloned()
            .ok_or_else(|| {
                AppError::invalid("this query result is no longer available; run the query again")
            })
    }
}

fn file_table_info(entry: &FileEntry) -> SqlTableInfo {
    let columns = entry.backing.columns();
    let truncated = columns.len() > SCHEMA_MAX_COLUMNS;
    SqlTableInfo {
        alias: entry.alias.clone(),
        label: entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| entry.alias.clone()),
        kind: entry.kind.wire_name().to_string(),
        columns: columns
            .into_iter()
            .take(SCHEMA_MAX_COLUMNS)
            .map(|c| SqlColumnInfo {
                decl_type: c
                    .schema
                    .as_ref()
                    .map(|s| logical_name(s.logical_type))
                    .unwrap_or("text")
                    .to_string(),
                name: c.name,
            })
            .collect(),
        columns_truncated: truncated,
        row_count: match entry.backing.row_count() {
            RowCountHint::Exact(n) | RowCountHint::Estimate(n) => Some(n),
            RowCountHint::Unknown => None,
        },
        path: Some(entry.path.to_string_lossy().to_string()),
    }
}

fn logical_name(t: crate::schema::LogicalType) -> &'static str {
    use crate::schema::LogicalType as L;
    match t {
        L::Text => "text",
        L::Integer => "integer",
        L::Decimal => "decimal",
        L::Float => "float",
        L::Boolean => "boolean",
        L::Date => "date",
        L::Datetime => "datetime",
        L::Uuid => "uuid",
        L::Json => "json",
    }
}

// ---------------------------------------------------------------------------
// Statement pre-validation
// ---------------------------------------------------------------------------

/// Skip leading whitespace and SQL comments (`--` line, `/* */` block).
fn strip_sql_lead(sql: &str) -> AppResult<&str> {
    let mut rest = sql;
    loop {
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix("--") {
            rest = after.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
        } else if let Some(after) = trimmed.strip_prefix("/*") {
            rest = after
                .split_once("*/")
                .map(|(_, tail)| tail)
                .ok_or_else(|| AppError::invalid("unterminated /* comment in the query"))?;
        } else {
            return Ok(trimmed);
        }
    }
}

/// The leading keyword of the (comment-stripped) statement, uppercased.
fn leading_keyword(sql: &str) -> AppResult<String> {
    let rest = strip_sql_lead(sql)?;
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if word.is_empty() {
        return Err(AppError::invalid("the query is empty"));
    }
    Ok(word.to_ascii_uppercase())
}

/// Layer (a) of the F36 defence in depth: only `SELECT`, `WITH`, `VALUES`
/// and `EXPLAIN` statements may even reach prepare. (The authorizer and the
/// read-only open flag remain underneath.)
pub(crate) fn check_statement_kind(sql: &str) -> AppResult<()> {
    let kw = leading_keyword(sql)?;
    match kw.as_str() {
        "SELECT" | "WITH" | "VALUES" | "EXPLAIN" => Ok(()),
        other => Err(AppError::invalid(format!(
            "only SELECT, WITH, VALUES and EXPLAIN statements can run in the SQL workspace \
             (found '{other}')"
        ))),
    }
}

/// Prepare `sql` under the full pre-validation stack: keyword whitelist,
/// single-statement enforcement (multi-statement input fails at prepare),
/// the connection's authorizer (fires during prepare — DML hidden in a CTE
/// or behind `EXPLAIN` is denied HERE, before anything executes), and a
/// `sqlite3_stmt_readonly` assertion for non-EXPLAIN statements.
pub(crate) fn prepare_checked<'c>(
    conn: &'c Connection,
    sql: &str,
) -> AppResult<rusqlite::Statement<'c>> {
    check_statement_kind(sql)?;
    let stmt = conn.prepare(sql).map_err(|e| match e {
        rusqlite::Error::MultipleStatement => AppError::invalid(
            "run one statement at a time (the input contains multiple SQL statements)",
        ),
        other => AppError::from(other),
    })?;
    // EXPLAIN executes the statement in explain mode (it only lists the VM
    // program); the inner statement was already vetted by the authorizer.
    if leading_keyword(sql)? != "EXPLAIN" && !stmt.readonly() {
        return Err(AppError::invalid(
            "only read-only statements can run in the SQL workspace",
        ));
    }
    Ok(stmt)
}

// ---------------------------------------------------------------------------
// Typed parameters
// ---------------------------------------------------------------------------

/// Strict decimal shape: optional sign, digits, optional `.digits` — no
/// exponent, no inf/nan.
fn is_decimal(s: &str) -> bool {
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    match s.split_once('.') {
        Some((int, frac)) => digits(int) && digits(frac),
        None => digits(s),
    }
}

impl SqlParam {
    /// Convert to a bindable SQLite value; every mismatch is rejected here,
    /// BEFORE the query runs.
    pub(crate) fn to_sql_value(&self) -> AppResult<SqlValue> {
        if self.param_type == SqlParamType::Null {
            return Ok(SqlValue::Null);
        }
        let raw = self
            .value
            .as_deref()
            .ok_or_else(|| AppError::invalid(format!("parameter :{} has no value", self.name)))?;
        let bad = |what: &str| {
            AppError::invalid(format!(
                "parameter :{} is not a valid {what}: {raw:?}",
                self.name
            ))
        };
        match self.param_type {
            SqlParamType::Null => unreachable!("handled above"),
            SqlParamType::Text => Ok(SqlValue::Text(raw.to_string())),
            SqlParamType::Integer => raw
                .trim()
                .parse::<i64>()
                .map(SqlValue::Integer)
                .map_err(|_| bad("integer")),
            SqlParamType::Float => {
                let f = raw.trim().parse::<f64>().map_err(|_| bad("number"))?;
                if f.is_finite() {
                    Ok(SqlValue::Real(f))
                } else {
                    Err(bad("number"))
                }
            }
            SqlParamType::Decimal => {
                let t = raw.trim();
                if !is_decimal(t) {
                    return Err(bad("decimal"));
                }
                t.parse::<f64>()
                    .map(SqlValue::Real)
                    .map_err(|_| bad("decimal"))
            }
            SqlParamType::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => Ok(SqlValue::Integer(1)),
                "false" | "0" => Ok(SqlValue::Integer(0)),
                _ => Err(bad("boolean (true/false)")),
            },
            SqlParamType::Date => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
                .map(|d| SqlValue::Text(d.format("%Y-%m-%d").to_string()))
                .map_err(|_| bad("date (YYYY-MM-DD)")),
            SqlParamType::Datetime => {
                let t = raw.trim();
                let parsed = NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f")
                    .or_else(|_| NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f"))
                    .or_else(|_| chrono::DateTime::parse_from_rfc3339(t).map(|d| d.naive_utc()))
                    .map_err(|_| bad("datetime (ISO 8601)"))?;
                Ok(SqlValue::Text(
                    parsed.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
                ))
            }
        }
    }
}

/// Bind every named parameter the statement uses. `:name` placeholders
/// only; missing, unused and positional parameters are rejected. Values are
/// BOUND (never interpolated), so a malicious string value is inert data.
pub(crate) fn bind_params(
    stmt: &mut rusqlite::Statement<'_>,
    params: &[SqlParam],
) -> AppResult<()> {
    let n = stmt.parameter_count();
    let mut used = vec![false; params.len()];
    for i in 1..=n {
        let raw = stmt.parameter_name(i).ok_or_else(|| {
            AppError::invalid("use named parameters (:name), not positional '?' placeholders")
        })?;
        if !raw.starts_with(':') {
            return Err(AppError::invalid(format!(
                "use ':name' parameters (found '{raw}')"
            )));
        }
        let name = &raw[1..];
        let idx = params.iter().position(|p| p.name == name).ok_or_else(|| {
            AppError::invalid(format!(
                "the query uses :{name} but no such parameter was supplied"
            ))
        })?;
        used[idx] = true;
        let value = params[idx].to_sql_value()?;
        stmt.raw_bind_parameter(i, value)?;
    }
    if let Some((param, _)) = params.iter().zip(&used).find(|(_, u)| !**u) {
        return Err(AppError::invalid(format!(
            "parameter :{} is not used by the query",
            param.name
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// A completed (bounded) run.
#[derive(Debug)]
pub(crate) struct RunOutput {
    pub columns: Vec<String>,
    pub rows: Vec<TabularRow>,
    pub truncated: bool,
    pub bytes: u64,
}

fn time_limit_error(limit: Duration) -> AppError {
    AppError::invalid(format!(
        "the query exceeded its time limit ({} ms); raise the limit or narrow the query",
        limit.as_millis()
    ))
}

/// Run one pre-validated statement, streaming rows into a bounded spool
/// under `limits`. Cancellation (via `ctx`) and the deadline are polled both
/// between rows and inside long-running SQLite work through a progress
/// handler, so a runaway cross join aborts promptly. The handler is always
/// cleared before returning — the connection stays reusable.
pub(crate) fn run_query(
    conn: &Connection,
    sql: &str,
    params: &[SqlParam],
    limits: &ResolvedLimits,
    ctx: Option<&JobCtx>,
) -> AppResult<RunOutput> {
    let deadline = Instant::now() + limits.time_limit;
    let token: Option<CancelToken> = ctx.map(JobCtx::cancel_token);
    {
        let token = token.clone();
        conn.progress_handler(
            PROGRESS_STEP_OPS,
            Some(move || {
                token.as_ref().is_some_and(CancelToken::is_cancelled) || Instant::now() >= deadline
            }),
        )?;
    }
    let result = run_query_inner(conn, sql, params, limits, ctx, deadline);
    let _ = safe_query::clear_cancel_handler(conn);
    match result {
        // A progress-handler abort surfaces as Cancelled; when the job was
        // NOT cancelled it was the deadline that fired.
        Err(AppError::Cancelled)
            if !token.as_ref().is_some_and(CancelToken::is_cancelled)
                && Instant::now() >= deadline =>
        {
            Err(time_limit_error(limits.time_limit))
        }
        other => other,
    }
}

fn run_query_inner(
    conn: &Connection,
    sql: &str,
    params: &[SqlParam],
    limits: &ResolvedLimits,
    ctx: Option<&JobCtx>,
    deadline: Instant,
) -> AppResult<RunOutput> {
    let mut stmt = prepare_checked(conn, sql)?;
    bind_params(&mut stmt, params)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    if columns.is_empty() {
        return Err(AppError::invalid("the statement returns no columns"));
    }
    let n_cols = columns.len();

    let mut out: Vec<TabularRow> = Vec::new();
    let mut bytes = 0u64;
    let mut truncated = false;
    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        if out.len() >= limits.max_rows {
            truncated = true;
            break;
        }
        if Instant::now() >= deadline {
            return Err(time_limit_error(limits.time_limit));
        }
        let mut cells: TabularRow = Vec::with_capacity(n_cols);
        for i in 0..n_cols {
            let cell = value_to_text(row.get_ref(i)?);
            bytes += cell.as_deref().map_or(0, str::len) as u64 + 8;
            cells.push(cell);
        }
        out.push(cells);
        if let Some(ctx) = ctx {
            // Progress = rows produced; also observes cancellation.
            ctx.advance(1)?;
        }
        if bytes > limits.max_bytes {
            truncated = true;
            break;
        }
    }
    Ok(RunOutput {
        columns,
        rows: out,
        truncated,
        bytes,
    })
}

/// Prepare-only dry run: never steps the statement.
pub(crate) fn validate_query(conn: &Connection, sql: &str) -> SqlValidation {
    match prepare_checked(conn, sql) {
        Ok(stmt) => {
            let columns = stmt.column_names().iter().map(|c| c.to_string()).collect();
            let parameters = (1..=stmt.parameter_count())
                .filter_map(|i| {
                    stmt.parameter_name(i)
                        .map(|n| n.trim_start_matches(':').to_string())
                })
                .collect();
            SqlValidation {
                ok: true,
                error: None,
                columns,
                parameters,
            }
        }
        Err(e) => SqlValidation {
            ok: false,
            error: Some(e.to_string()),
            columns: Vec::new(),
            parameters: Vec::new(),
        },
    }
}

/// `EXPLAIN QUERY PLAN` for a pre-validated statement, as a tree. Bounded like
/// every other steppable path in the module: a progress handler polls both the
/// deadline and job cancellation, node collection is capped, and the handler
/// is always cleared before returning so the connection stays reusable.
pub(crate) fn explain_plan(
    conn: &Connection,
    sql: &str,
    params: &[SqlParam],
    limits: &ResolvedLimits,
    ctx: Option<&JobCtx>,
) -> AppResult<Vec<SqlPlanNode>> {
    check_statement_kind(sql)?;
    if leading_keyword(sql)? == "EXPLAIN" {
        return Err(AppError::invalid(
            "the statement is already an EXPLAIN; run it directly instead",
        ));
    }
    let deadline = Instant::now() + limits.time_limit;
    let token: Option<CancelToken> = ctx.map(JobCtx::cancel_token);
    {
        let token = token.clone();
        conn.progress_handler(
            PROGRESS_STEP_OPS,
            Some(move || {
                token.as_ref().is_some_and(CancelToken::is_cancelled) || Instant::now() >= deadline
            }),
        )?;
    }
    let result = explain_plan_collect(conn, sql, params, limits, ctx, deadline);
    let _ = safe_query::clear_cancel_handler(conn);
    match result {
        // A progress-handler abort surfaces as Cancelled; when the job was
        // NOT cancelled it was the deadline that fired.
        Err(AppError::Cancelled)
            if !token.as_ref().is_some_and(CancelToken::is_cancelled)
                && Instant::now() >= deadline =>
        {
            Err(time_limit_error(limits.time_limit))
        }
        other => other,
    }
}

fn explain_plan_collect(
    conn: &Connection,
    sql: &str,
    params: &[SqlParam],
    limits: &ResolvedLimits,
    ctx: Option<&JobCtx>,
    deadline: Instant,
) -> AppResult<Vec<SqlPlanNode>> {
    // The authorizer vets the inner statement while the wrapped form
    // prepares, so DML cannot hide behind the wrapper; multi-statement input
    // still fails at prepare.
    let wrapped = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&wrapped).map_err(|e| match e {
        rusqlite::Error::MultipleStatement => AppError::invalid(
            "run one statement at a time (the input contains multiple SQL statements)",
        ),
        other => AppError::from(other),
    })?;
    bind_params(&mut stmt, params)?;
    let mut flat: Vec<(i64, i64, String)> = Vec::new();
    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        if flat.len() >= EXPLAIN_MAX_NODES {
            break;
        }
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        if Instant::now() >= deadline {
            return Err(time_limit_error(limits.time_limit));
        }
        let id: i64 = row.get(0)?;
        let parent: i64 = row.get(1)?;
        let detail: String = row.get(3)?;
        flat.push((id, parent, detail));
    }
    fn build(flat: &[(i64, i64, String)], parent: i64) -> Vec<SqlPlanNode> {
        flat.iter()
            .filter(|(_, p, _)| *p == parent)
            .map(|(id, _, detail)| SqlPlanNode {
                id: *id,
                detail: detail.clone(),
                children: build(flat, *id),
            })
            .collect()
    }
    Ok(build(&flat, 0))
}

// ---------------------------------------------------------------------------
// Result consumption: materialize + export
// ---------------------------------------------------------------------------

/// Build a NEW derived document from a stored result. SQL `NULL` narrows to
/// an empty cell (the document text plane carries no null bit). Small
/// results stay in-memory editable; large ones (or `force_indexed`) spill
/// through the F10 index machinery and open read-only.
pub(crate) fn materialize_result(
    result: &StoredResult,
    doc_id: u64,
    cache_root: &Path,
    force_indexed: bool,
    ctx: Option<&JobCtx>,
) -> AppResult<Document> {
    let budget = if force_indexed {
        0
    } else {
        derived::SPILL_BUDGET
    };
    let mut builder =
        DerivedDocumentBuilder::new(result.columns.clone(), cache_root.to_path_buf(), budget);
    if let Some(ctx) = ctx {
        ctx.set_total(result.rows.len() as u64);
    }
    for (i, row) in result.rows.iter().enumerate() {
        builder.push_row(row.iter().map(|c| c.clone().unwrap_or_default()).collect())?;
        if let Some(ctx) = ctx {
            if i % CANCEL_EVERY == 0 {
                ctx.check()?;
            }
            ctx.advance(1)?;
        }
    }
    builder.finish(doc_id, &mut |_| match ctx {
        Some(ctx) => ctx.check(),
        None => Ok(()),
    })
}

/// Export a stored result directly to a delimited file through the shared
/// sink (atomic commit; missing/NULL narrows to an empty field).
pub(crate) fn export_result(
    result: &StoredResult,
    dest: &Path,
    options: &ExportOptions,
    ctx: Option<&JobCtx>,
) -> AppResult<u64> {
    let mut sink = CsvSink::create(dest, options)?;
    tabular::copy(&StoredResultSource(result), &mut sink, ctx)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Prepend `entry` to the persisted history ring (cap [`SQL_HISTORY_CAP`],
/// most recent first).
pub(crate) fn push_history(settings_dir: &Path, entry: SqlHistoryEntry) -> AppResult<()> {
    let mut s = settings::load_settings(settings_dir);
    s.sql_history.insert(0, entry);
    s.sql_history.truncate(SQL_HISTORY_CAP);
    settings::save_settings(settings_dir, &s)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Source assembly + schema browsing
// ---------------------------------------------------------------------------

/// A selection resolved into concrete vtab exposures.
pub(crate) struct AssembledSources {
    pub docs: Vec<(String, SharedDocument)>,
    pub files: Vec<(String, SharedTabular)>,
    pub database: Option<PathBuf>,
}

/// Resolve a selection into concrete vtab exposures: registered file
/// backings, open-document handles with deterministic sanitized aliases
/// (file aliases are reserved first; document aliases dedupe against them
/// in list order), and the optional database path (checked against the
/// approved registry at connect).
///
/// Document/file vtabs land in the temp schema, which SQLite resolves BEFORE
/// the main (database) schema for an unqualified name — so a source alias
/// equal to a real table/view in the selected database would silently shadow
/// it. When `strict_db_collision` is set (the executing paths — run, validate,
/// explain) such a clash is refused with a clear error; the schema browser
/// passes `false` so it can still enumerate every source.
pub(crate) fn assemble_sources(
    state: &Mutex<AppState>,
    workspace: &SqlWorkspace,
    approved: &ApprovedSources,
    selection: &SqlSourceSelection,
    strict_db_collision: bool,
) -> AppResult<AssembledSources> {
    let files = workspace.resolve_files(&selection.files)?;
    let mut taken: Vec<String> = files.iter().map(|(a, _)| a.clone()).collect();

    // Clone the handles first (holding the registry lock only for lookups),
    // then read each document briefly for its display name.
    let mut handles: Vec<(u64, Option<String>, SharedDocument)> = Vec::new();
    {
        let registry = state
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?;
        for d in &selection.documents {
            handles.push((d.doc_id, d.alias.clone(), registry.doc(d.doc_id)?));
        }
    }
    let mut docs: Vec<(String, SharedDocument)> = Vec::with_capacity(handles.len());
    for (doc_id, alias, handle) in handles {
        let base = match alias {
            Some(a) if !a.trim().is_empty() => a,
            _ => {
                let guard = handle
                    .read()
                    .map_err(|_| AppError::Other("internal document lock error".into()))?;
                let name = guard.meta().file_name;
                let stem = Path::new(&name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .unwrap_or(name);
                if stem.trim().is_empty() {
                    format!("doc{doc_id}")
                } else {
                    stem
                }
            }
        };
        let alias = unique_table_alias(&taken, &sanitize_table_name(&base));
        taken.push(alias.clone());
        docs.push((alias, handle));
    }
    let database = selection.database.as_ref().map(PathBuf::from);
    if strict_db_collision {
        if let Some(db_path) = &database {
            let db_names = db_table_names(approved, db_path)?;
            let aliases = docs
                .iter()
                .map(|(a, _)| a.as_str())
                .chain(files.iter().map(|(a, _)| a.as_str()));
            check_db_alias_collisions(aliases, &db_names)?;
        }
    }
    Ok(AssembledSources {
        docs,
        files,
        database,
    })
}

/// Top-level table/view names of an approved database, lowercased for the
/// case-insensitive comparison SQLite applies to ASCII identifiers.
fn db_table_names(
    approved: &ApprovedSources,
    path: &Path,
) -> AppResult<std::collections::HashSet<String>> {
    let canonical = approved.check(path)?;
    let conn = safe_query::open_guarded(&canonical)?;
    let mut names = std::collections::HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type IN ('table','view') \
         AND name NOT LIKE 'sqlite_%'",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        names.insert(name.to_ascii_lowercase());
    }
    Ok(names)
}

/// Refuse the query if any source alias collides (case-insensitively) with a
/// selected-database table/view name: an unqualified reference would resolve to
/// the temp-schema vtab, silently reading the file/document instead of the
/// database table.
fn check_db_alias_collisions<'a>(
    aliases: impl Iterator<Item = &'a str>,
    db_names: &std::collections::HashSet<String>,
) -> AppResult<()> {
    let mut clashes: Vec<String> = aliases
        .filter(|a| db_names.contains(&a.to_ascii_lowercase()))
        .map(str::to_string)
        .collect();
    if clashes.is_empty() {
        return Ok(());
    }
    clashes.sort();
    clashes.dedup();
    Err(AppError::invalid(format!(
        "these source names also name tables in the selected database: {}. \
         An unqualified reference would read the file/document instead of the \
         database table — rename the source, unselect the database, or \
         schema-qualify the database table as main.<name>.",
        clashes.join(", ")
    )))
}

/// Bounded table info for one open document.
fn doc_table_info(alias: &str, doc: &Document) -> SqlTableInfo {
    let source = DocumentSource::new(doc);
    let columns = source.columns();
    let truncated = columns.len() > SCHEMA_MAX_COLUMNS;
    SqlTableInfo {
        alias: alias.to_string(),
        label: doc.meta().file_name,
        kind: "document".to_string(),
        columns: columns
            .into_iter()
            .take(SCHEMA_MAX_COLUMNS)
            .map(|c| SqlColumnInfo {
                decl_type: c
                    .schema
                    .as_ref()
                    .map(|s| logical_name(s.logical_type))
                    .unwrap_or("text")
                    .to_string(),
                name: c.name,
            })
            .collect(),
        columns_truncated: truncated,
        row_count: Some(doc.n_rows() as u64),
        path: None,
    }
}

/// Bounded table+column info for an approved database (tables and views,
/// capped; columns per table capped). Read through a guarded connection.
pub(crate) fn db_table_infos(
    approved: &ApprovedSources,
    path: &Path,
) -> AppResult<(Vec<SqlTableInfo>, bool)> {
    let canonical = approved.check(path)?;
    let conn = safe_query::open_guarded(&canonical)?;
    let mut names: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT name, type FROM sqlite_schema WHERE type IN ('table','view') \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if names.len() > SCHEMA_MAX_TABLES {
                break;
            }
            names.push((row.get(0)?, row.get(1)?));
        }
    }
    let truncated = names.len() > SCHEMA_MAX_TABLES;
    names.truncate(SCHEMA_MAX_TABLES);
    let mut out = Vec::with_capacity(names.len());
    for (name, kind) in names {
        let mut columns: Vec<SqlColumnInfo> = Vec::new();
        let mut columns_truncated = false;
        let mut stmt = conn.prepare("SELECT name, type FROM pragma_table_info(?1)")?;
        let mut rows = stmt.query([&name])?;
        while let Some(row) = rows.next()? {
            if columns.len() >= SCHEMA_MAX_COLUMNS {
                columns_truncated = true;
                break;
            }
            columns.push(SqlColumnInfo {
                name: row.get(0)?,
                decl_type: row.get::<_, String>(1)?.to_ascii_lowercase(),
            });
        }
        out.push(SqlTableInfo {
            alias: name.clone(),
            label: name,
            kind,
            columns,
            columns_truncated,
            row_count: None,
            path: None,
        });
    }
    Ok((out, truncated))
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

fn blocking_err<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Other(format!("background task failed: {e}"))
}

fn settings_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::Other(format!("application-data directory unavailable: {e}")))
}

fn cache_root(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::Other(format!("no cache directory: {e}")))?
        .join("indexes"))
}

/// Approve a user-picked file and register it as a queryable table. This is
/// THE approval entry point for file sources (mirrors `db_open` for
/// databases): only paths that came through here are readable by queries.
#[tauri::command]
pub async fn sql_register_file(
    path: String,
    pointer: Option<String>,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
) -> AppResult<SqlTableInfo> {
    let ctx = jobs.begin_for_app(&app, "sqlRegisterFile", None);
    crate::job::run_blocking(ctx, move |ctx| {
        let approved = app.state::<ApprovedSources>();
        approved.approve(Path::new(&path))?;
        let root = cache_root(&app)?;
        let (canonical, kind, backing) = open_backing(
            &approved,
            Path::new(&path),
            pointer.as_deref(),
            &root,
            Some(ctx),
        )?;
        app.state::<SqlWorkspace>()
            .register(canonical, kind, backing)
    })
    .await
}

/// Forget a registered file table and revoke its approval.
#[tauri::command]
pub fn sql_unregister_file(
    alias: String,
    workspace: State<'_, SqlWorkspace>,
    approved: State<'_, ApprovedSources>,
) -> AppResult<()> {
    let path = workspace.unregister(&alias)?;
    approved.revoke(&path);
    Ok(())
}

/// The registered file tables.
#[tauri::command]
pub fn sql_list_files(workspace: State<'_, SqlWorkspace>) -> AppResult<Vec<SqlTableInfo>> {
    workspace.list_files()
}

/// Bounded schema-browser / autocomplete payload for a selection.
#[tauri::command]
pub async fn sql_schema(
    selection: SqlSourceSelection,
    app: tauri::AppHandle,
) -> AppResult<SqlSchemaDto> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Mutex<AppState>>();
        let workspace = app.state::<SqlWorkspace>();
        let approved = app.state::<ApprovedSources>();
        let assembled = assemble_sources(&state, &workspace, &approved, &selection, false)?;
        let mut documents = Vec::with_capacity(assembled.docs.len());
        for (alias, handle) in &assembled.docs {
            let guard = handle
                .read()
                .map_err(|_| AppError::Other("internal document lock error".into()))?;
            documents.push(doc_table_info(alias, &guard));
        }
        let files = workspace.list_files()?;
        let (database, database_truncated) = match &selection.database {
            Some(p) => db_table_infos(&approved, Path::new(p))?,
            None => (Vec::new(), false),
        };
        Ok(SqlSchemaDto {
            documents,
            files,
            database,
            database_truncated,
        })
    })
    .await
    .map_err(blocking_err)?
}

/// Prepare-only dry run: syntax/authorization errors, output columns and
/// the named parameters the statement uses. Never executes anything.
#[tauri::command]
pub async fn sql_validate(
    request: SqlRunRequest,
    app: tauri::AppHandle,
) -> AppResult<SqlValidation> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Mutex<AppState>>();
        let workspace = app.state::<SqlWorkspace>();
        let approved = app.state::<ApprovedSources>();
        let validation = match assemble_sources(
            &state,
            &workspace,
            &approved,
            &request.sources,
            true,
        )
        .and_then(|a| {
            safe_query::connect_with_sources(&approved, a.database.as_deref(), &a.docs, &a.files)
        }) {
            Ok(conn) => validate_query(&conn, &request.sql),
            Err(e) => SqlValidation {
                ok: false,
                error: Some(e.to_string()),
                columns: Vec::new(),
                parameters: Vec::new(),
            },
        };
        Ok(validation)
    })
    .await
    .map_err(blocking_err)?
}

/// `EXPLAIN QUERY PLAN` tree for a statement (pre-validated; parameters
/// bound so the plan reflects real bindings). Runs as a cancellable job with
/// the same resolved time limit as a real run, so a pathological plan can be
/// aborted from the UI instead of pinning a blocking-pool thread.
#[tauri::command]
pub async fn sql_explain(
    request: SqlRunRequest,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
) -> AppResult<Vec<SqlPlanNode>> {
    let ctx = jobs.begin_for_app(&app, "sqlExplain", None);
    crate::job::run_blocking(ctx, move |ctx| {
        let state = app.state::<Mutex<AppState>>();
        let workspace = app.state::<SqlWorkspace>();
        let approved = app.state::<ApprovedSources>();
        let a = assemble_sources(&state, &workspace, &approved, &request.sources, true)?;
        let conn =
            safe_query::connect_with_sources(&approved, a.database.as_deref(), &a.docs, &a.files)?;
        let limits = request.limits.resolve();
        explain_plan(&conn, &request.sql, &request.params, &limits, Some(ctx))
    })
    .await
}

/// Run one statement as a cancellable job. The bounded result spool is
/// stored (replacing the previous one) and the first window returned; the
/// run is appended to the persisted history whatever its outcome.
#[tauri::command]
pub async fn sql_run(
    request: SqlRunRequest,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
) -> AppResult<SqlRunSummary> {
    let ctx = jobs.begin_for_app(&app, "sqlQuery", None);
    crate::job::run_blocking(ctx, move |ctx| {
        let state = app.state::<Mutex<AppState>>();
        let workspace = app.state::<SqlWorkspace>();
        let approved = app.state::<ApprovedSources>();
        let assembled = assemble_sources(&state, &workspace, &approved, &request.sources, true)?;

        let mut history_sources: Vec<String> =
            assembled.docs.iter().map(|(a, _)| a.clone()).collect();
        history_sources.extend(assembled.files.iter().map(|(a, _)| a.clone()));
        if let Some(db) = &assembled.database {
            history_sources.push(db.to_string_lossy().to_string());
        }

        let conn = safe_query::connect_with_sources(
            &approved,
            assembled.database.as_deref(),
            &assembled.docs,
            &assembled.files,
        )?;
        let limits = request.limits.resolve();
        let started = Instant::now();
        let outcome = run_query(&conn, &request.sql, &request.params, &limits, Some(ctx));
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let (status, row_count, error) = match &outcome {
            Ok(out) => ("done", Some(out.rows.len() as u64), None),
            Err(AppError::Cancelled) => ("cancelled", None, None),
            Err(e) => ("failed", None, Some(e.to_string())),
        };
        if let Ok(dir) = settings_dir(&app) {
            let _ = push_history(
                &dir,
                SqlHistoryEntry {
                    sql: request.sql.clone(),
                    params: request.params.clone(),
                    sources: history_sources,
                    ran_at_ms: now_ms(),
                    status: status.to_string(),
                    row_count,
                    error,
                },
            );
        }

        let output = outcome?;
        let stored =
            workspace.store_result(output.columns, output.rows, output.truncated, output.bytes)?;
        Ok(SqlRunSummary {
            result_id: stored.id,
            columns: stored.columns.clone(),
            rows: stored
                .rows
                .iter()
                .take(FIRST_WINDOW_ROWS)
                .cloned()
                .collect(),
            row_count: stored.rows.len() as u64,
            truncated: stored.truncated,
            byte_count: stored.bytes,
            elapsed_ms,
        })
    })
    .await
}

/// One bounded window of the stored result.
#[tauri::command]
pub fn sql_result_rows(
    result_id: u64,
    start: usize,
    limit: usize,
    workspace: State<'_, SqlWorkspace>,
) -> AppResult<SqlResultWindow> {
    let result = workspace.result_arc(result_id)?;
    let n = result.rows.len();
    let start = start.min(n);
    let end = start.saturating_add(limit.min(RESULT_WINDOW_MAX)).min(n);
    Ok(SqlResultWindow {
        start,
        rows: result.rows[start..end].to_vec(),
        total: n as u64,
        truncated: result.truncated,
    })
}

/// Materialize the stored result as a NEW derived document (editable when
/// small, indexed when large or forced). The document registers under the
/// returned doc id when the job finishes.
#[tauri::command]
pub async fn sql_materialize(
    result_id: u64,
    force_indexed: bool,
    app: tauri::AppHandle,
    state: State<'_, Mutex<AppState>>,
    jobs: State<'_, JobRegistry>,
    workspace: State<'_, SqlWorkspace>,
) -> AppResult<IndexedOpenStart> {
    let result = workspace.result_arc(result_id)?;
    let doc_id = state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .alloc_id();
    let root = cache_root(&app)?;
    let ctx = jobs.begin_for_app(&app, "derive", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let mut doc = materialize_result(&result, doc_id, &root, force_indexed, Some(ctx))?;
            doc.set_display_name(format!("Query result {}", result.id));
            app_for_job
                .state::<Mutex<AppState>>()
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// Export the stored result directly to a delimited file (atomic commit).
#[tauri::command]
pub async fn sql_export(
    result_id: u64,
    path: String,
    options: ExportOptions,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
    workspace: State<'_, SqlWorkspace>,
) -> AppResult<u64> {
    let result = workspace.result_arc(result_id)?;
    let ctx = jobs.begin_for_app(&app, "sqlExport", None);
    crate::job::run_blocking(ctx, move |ctx| {
        export_result(&result, Path::new(&path), &options, Some(ctx))
    })
    .await
}

/// The persisted query history (most recent first). Entries are data —
/// nothing here executes.
#[tauri::command]
pub fn sql_history(app: tauri::AppHandle) -> AppResult<Vec<SqlHistoryEntry>> {
    Ok(settings::load_settings(&settings_dir(&app)?).sql_history)
}

/// Clear the persisted query history.
#[tauri::command]
pub fn sql_history_clear(app: tauri::AppHandle) -> AppResult<()> {
    let dir = settings_dir(&app)?;
    let mut s = settings::load_settings(&dir);
    s.sql_history.clear();
    settings::save_settings(&dir, &s)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::safe_query::connect_with_sources;
    use std::sync::RwLock;

    fn doc(csv: &str) -> SharedDocument {
        let parsed = parse::parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Arc::new(RwLock::new(Document::from_parsed(1, None, parsed, true)))
    }

    fn test_db(dir: &Path) -> (ApprovedSources, PathBuf) {
        let path = dir.join("test.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT, price REAL);
             INSERT INTO items VALUES (1, 'apple', 1.5), (2, NULL, 2.0), (3, 'plum', NULL);",
        )
        .unwrap();
        let approved = ApprovedSources::default();
        approved.approve(&path).unwrap();
        (approved, path)
    }

    fn limits() -> ResolvedLimits {
        SqlLimits::default().resolve()
    }

    fn run(conn: &Connection, sql: &str) -> AppResult<RunOutput> {
        run_query(conn, sql, &[], &limits(), None)
    }

    fn param(name: &str, ptype: SqlParamType, value: Option<&str>) -> SqlParam {
        SqlParam {
            name: name.into(),
            param_type: ptype,
            value: value.map(str::to_string),
        }
    }

    fn approve_file(dir: &Path, name: &str, contents: &[u8]) -> (ApprovedSources, PathBuf) {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        let approved = ApprovedSources::default();
        approved.approve(&path).unwrap();
        (approved, path)
    }

    fn write_parquet(path: &Path) {
        use arrow::array::{ArrayRef, Int64Array, StringArray};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, true),
            Field::new("label", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![Some(1), Some(2), Some(3)])) as ArrayRef,
                Arc::new(StringArray::from(vec![Some("one"), None, Some("three")])) as ArrayRef,
            ],
        )
        .unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    // ----- table names ------------------------------------------------------

    #[test]
    fn table_names_sanitize_deterministically_and_dedupe() {
        assert_eq!(sanitize_table_name("My Data (v2).csv"), "my_data_v2_csv");
        assert_eq!(sanitize_table_name("2024 sales"), "t_2024_sales");
        assert_eq!(sanitize_table_name("___"), "table");
        assert_eq!(sanitize_table_name(""), "table");
        let taken = vec!["a".to_string(), "a_2".to_string()];
        assert_eq!(unique_table_alias(&taken, "a"), "a_3");
        assert_eq!(unique_table_alias(&taken, "b"), "b");
    }

    // ----- statement pre-validation -----------------------------------------

    #[test]
    fn keyword_whitelist_allows_select_class_only() {
        for ok in [
            "SELECT 1",
            "  select 1",
            "-- lead comment\nSELECT 1",
            "/* block */ WITH t(x) AS (VALUES (1)) SELECT x FROM t",
            "VALUES (1, 2)",
            "EXPLAIN SELECT 1",
            "EXPLAIN QUERY PLAN SELECT 1",
        ] {
            assert!(check_statement_kind(ok).is_ok(), "should pass: {ok}");
        }
        for denied in [
            "PRAGMA data_version",
            "PRAGMA journal_mode = WAL",
            "BEGIN",
            "COMMIT",
            "ATTACH DATABASE 'x' AS y",
            "CREATE TABLE t(a)",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "VACUUM",
            "ANALYZE",
            "REINDEX",
            "",
            "   ",
            "-- only a comment",
        ] {
            let err = check_statement_kind(denied).unwrap_err().to_string();
            assert!(
                err.contains("SQL workspace") || err.contains("empty"),
                "should be rejected pre-prepare: {denied} -> {err}"
            );
        }
    }

    #[test]
    fn denial_matrix_fails_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = test_db(dir.path());
        let d = doc("a\n1\n");
        let conn = connect_with_sources(&approved, Some(&path), &[("d".into(), d)], &[]).unwrap();

        let denied = [
            // Blocked by the keyword whitelist (never reaches prepare).
            "ATTACH DATABASE 'x' AS y",
            "INSERT INTO items VALUES (9, 'x', 9.9)",
            "UPDATE items SET name = 'x'",
            "DELETE FROM items",
            "DROP TABLE items",
            "CREATE TABLE t2(a)",
            "CREATE VIEW v2 AS SELECT 1",
            "ALTER TABLE items ADD COLUMN extra TEXT",
            "PRAGMA journal_mode = WAL",
            "PRAGMA writable_schema = 1",
            "PRAGMA data_version",
            "ANALYZE",
            "REINDEX",
            "VACUUM",
            "BEGIN",
            // Pass the keyword check but die at prepare: the authorizer
            // denies the inner operation before anything executes.
            "WITH x AS (SELECT 1) INSERT INTO items(id, name, price) SELECT 9, 'x', 9.9 FROM x",
            "EXPLAIN INSERT INTO items VALUES (9, 'x', 9.9)",
            "EXPLAIN QUERY PLAN DELETE FROM items",
            "SELECT load_extension('evil')",
            "WITH x AS (SELECT load_extension('evil') AS e) SELECT * FROM x",
            // Multi-statement input is rejected at prepare.
            "SELECT 1; DROP TABLE items",
            "SELECT 1; SELECT 2",
        ];
        for sql in denied {
            assert!(run(&conn, sql).is_err(), "must be denied: {sql}");
        }
        // Nothing executed: table intact, document intact.
        let out = run(&conn, "SELECT COUNT(*) FROM items").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("3"));
        let out = run(&conn, "SELECT COUNT(*) FROM d").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("1"));

        // The multi-statement rejection carries its own clear message.
        let err = run(&conn, "SELECT 1; SELECT 2").unwrap_err().to_string();
        assert!(err.contains("one statement at a time"), "{err}");
    }

    #[test]
    fn metadata_reads_stay_available_through_select() {
        // Autocomplete needs pragma table functions; they are reachable
        // through SELECT (allowed) even though bare PRAGMA is not.
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = test_db(dir.path());
        let conn = connect_with_sources(&approved, Some(&path), &[], &[]).unwrap();
        let out = run(&conn, "SELECT name FROM pragma_table_info('items')").unwrap();
        assert_eq!(out.rows.len(), 3);
    }

    // ----- approved-path containment ----------------------------------------

    #[test]
    fn unapproved_paths_fail_in_any_form() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.csv");
        std::fs::write(&file, "a,b\n1,2\n").unwrap();
        let approved = ApprovedSources::default();

        // Absolute, dotted and URI spellings of an unapproved path all fail.
        assert!(open_backing(&approved, &file, None, cache.path(), None).is_err());
        let dotted = dir.path().join("sub").join("..").join("data.csv");
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        assert!(open_backing(&approved, &dotted, None, cache.path(), None).is_err());
        let uri = format!("file:///{}", file.display());
        assert!(open_backing(&approved, Path::new(&uri), None, cache.path(), None).is_err());

        // An unapproved database path is refused by the connection factory.
        assert!(connect_with_sources(&approved, Some(&file), &[], &[]).is_err());

        // Approval is by canonical identity: after approving the plain
        // path, the dotted spelling resolves to the same file and works.
        approved.approve(&file).unwrap();
        assert!(open_backing(&approved, &dotted, None, cache.path(), None).is_ok());
    }

    // ----- file virtual tables ----------------------------------------------

    #[test]
    fn csv_file_registers_and_joins_with_documents() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (approved, path) = approve_file(
            dir.path(),
            "labels.csv",
            b"id,label\n1,one\n2,two\n3,three\n",
        );

        let ws = SqlWorkspace::default();
        let (canonical, kind, backing) =
            open_backing(&approved, &path, None, cache.path(), None).unwrap();
        assert_eq!(kind, FileKind::Csv);
        let info = ws.register(canonical.clone(), kind, backing).unwrap();
        assert_eq!(info.alias, "labels");
        assert_eq!(info.kind, "csv");
        assert_eq!(info.row_count, Some(3));
        assert_eq!(info.columns.len(), 2);

        // Re-registering the same canonical path keeps the alias.
        let (c2, k2, b2) = open_backing(&approved, &path, None, cache.path(), None).unwrap();
        assert_eq!(ws.register(c2, k2, b2).unwrap().alias, "labels");
        assert_eq!(ws.list_files().unwrap().len(), 1);

        let files = ws.resolve_files(&["labels".into()]).unwrap();
        let d = doc("id,qty\n1,10\n3,30\n");
        let conn = connect_with_sources(&approved, None, &[("orders".into(), d)], &files).unwrap();
        let out = run(
            &conn,
            "SELECT COUNT(*) FROM labels l JOIN orders o ON l.id = o.id",
        )
        .unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("2"));

        // Unknown aliases are refused.
        assert!(ws.resolve_files(&["nope".into()]).is_err());
        // Unregister forgets the table.
        ws.unregister("labels").unwrap();
        assert!(ws.resolve_files(&["labels".into()]).is_err());
    }

    #[test]
    fn clashing_file_stems_get_deterministic_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let approved = ApprovedSources::default();
        let a = dir.path().join("data.csv");
        std::fs::write(&a, "x\n1\n").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let b = sub.join("data.csv");
        std::fs::write(&b, "y\n2\n").unwrap();
        approved.approve(&a).unwrap();
        approved.approve(&b).unwrap();

        let ws = SqlWorkspace::default();
        let (ca, ka, ba) = open_backing(&approved, &a, None, cache.path(), None).unwrap();
        let (cb, kb, bb) = open_backing(&approved, &b, None, cache.path(), None).unwrap();
        assert_eq!(ws.register(ca, ka, ba).unwrap().alias, "data");
        assert_eq!(ws.register(cb, kb, bb).unwrap().alias, "data_2");
    }

    #[test]
    fn parquet_file_vtab_keeps_sql_null() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.parquet");
        write_parquet(&path);
        let approved = ApprovedSources::default();
        approved.approve(&path).unwrap();

        let (_, kind, backing) = open_backing(&approved, &path, None, cache.path(), None).unwrap();
        assert_eq!(kind, FileKind::Parquet);
        let src: SharedTabular = Arc::new(backing);
        let conn = connect_with_sources(&approved, None, &[], &[("p".into(), src)]).unwrap();
        let out = run(&conn, "SELECT COUNT(*) FROM p WHERE label IS NULL").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("1"));
        let out = run(&conn, "SELECT label FROM p WHERE id = 3").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("three"));
    }

    #[test]
    fn jsonl_and_json_files_become_tables() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (approved, jsonl) = approve_file(
            dir.path(),
            "rows.jsonl",
            b"{\"a\": 1, \"b\": \"x\"}\n{\"a\": 2}\n",
        );
        let (_, kind, backing) = open_backing(&approved, &jsonl, None, cache.path(), None).unwrap();
        assert_eq!(kind, FileKind::Json);
        let src: SharedTabular = Arc::new(backing);
        let conn = connect_with_sources(&approved, None, &[], &[("j".into(), src)]).unwrap();
        // Keys union across records; a missing field lands as the import
        // engine's missing token (empty text), not SQL NULL — text plane.
        let out = run(&conn, "SELECT COUNT(*) FROM j WHERE b = ''").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("1"));
        let out = run(&conn, "SELECT a FROM j ORDER BY a").unwrap();
        assert_eq!(out.rows.len(), 2);

        // A .json record array works; an object document needs a pointer.
        let array = dir.path().join("arr.json");
        std::fs::write(&array, br#"[{"v": 10}, {"v": 20}]"#).unwrap();
        approved.approve(&array).unwrap();
        let (_, _, backing) = open_backing(&approved, &array, None, cache.path(), None).unwrap();
        let src: SharedTabular = Arc::new(backing);
        let conn = connect_with_sources(&approved, None, &[], &[("arr".into(), src)]).unwrap();
        let out = run(&conn, "SELECT SUM(CAST(v AS INTEGER)) FROM arr").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("30"));

        let object = dir.path().join("obj.json");
        std::fs::write(&object, br#"{"meta": 1, "records": [{"v": 5}, {"v": 7}]}"#).unwrap();
        approved.approve(&object).unwrap();
        assert!(
            open_backing(&approved, &object, None, cache.path(), None).is_err(),
            "an object document without a pointer needs a record array"
        );
        let (_, _, backing) =
            open_backing(&approved, &object, Some("/records"), cache.path(), None).unwrap();
        let src: SharedTabular = Arc::new(backing);
        let conn = connect_with_sources(&approved, None, &[], &[("o".into(), src)]).unwrap();
        let out = run(&conn, "SELECT COUNT(*) FROM o").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("2"));
    }

    #[test]
    fn file_rewrite_mid_query_aborts_instead_of_mixing_versions() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.csv");
        let mut csv = String::from("n\n");
        for i in 0..3000 {
            csv.push_str(&format!("{i}\n"));
        }
        std::fs::write(&path, &csv).unwrap();
        let approved = ApprovedSources::default();
        approved.approve(&path).unwrap();

        // Index-backed CSV (the large-file path): reads go back to the file,
        // so the vtab pins the file fingerprint and re-checks per window.
        let settings = index::IndexSettings {
            delimiter: Some(b','),
            encoding: Some(encoding_rs::UTF_8),
            has_header_row: Some(true),
            chunk_size: 0,
        };
        let indexed = index::build_index(&path, cache.path(), &settings, &mut |_| Ok(())).unwrap();
        let backing = FileTabular {
            backing: FileBacking::Doc(Box::new(Document::from_index(
                0,
                Some(path.clone()),
                indexed,
            ))),
        };
        let src: SharedTabular = Arc::new(backing);
        let conn = connect_with_sources(&approved, None, &[], &[("big".into(), src)]).unwrap();

        let mut stmt = conn.prepare("SELECT n FROM big").unwrap();
        let mut rows = stmt.query([]).unwrap();
        for _ in 0..1024 {
            assert!(rows.next().unwrap().is_some());
        }
        // Rewrite the backing file mid-scan (different size => new stat
        // fingerprint), then keep stepping: the next window refill must
        // abort rather than serve rows from the new file.
        std::fs::write(&path, "n\nrewritten\n").unwrap();
        let mut failed = false;
        loop {
            match rows.next() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => {
                    failed = true;
                    assert!(e.to_string().contains("changed"), "unexpected error: {e}");
                    break;
                }
            }
        }
        assert!(failed, "a mid-query rewrite must abort the scan");
    }

    // ----- typed parameters --------------------------------------------------

    #[test]
    fn params_are_bound_not_interpolated() {
        let approved = ApprovedSources::default();
        let d = doc("name\nAda\nBob\n");
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();
        // A malicious value is inert data under binding; interpolation
        // would turn this into `... WHERE name = 'x' OR '1'='1'` (2 rows).
        let out = run_query(
            &conn,
            "SELECT COUNT(*) FROM d WHERE name = :name",
            &[param("name", SqlParamType::Text, Some("x' OR '1'='1"))],
            &limits(),
            None,
        )
        .unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("0"));

        let out = run_query(
            &conn,
            "SELECT COUNT(*) FROM d WHERE name = :name",
            &[param("name", SqlParamType::Text, Some("Ada"))],
            &limits(),
            None,
        )
        .unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("1"));
    }

    #[test]
    fn params_validate_before_the_query_runs() {
        let approved = ApprovedSources::default();
        let d = doc("v\n1\n2\n");
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();
        let sql = "SELECT COUNT(*) FROM d WHERE CAST(v AS INTEGER) > :min";

        // Missing parameter.
        let err = run_query(&conn, sql, &[], &limits(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains(":min"), "{err}");
        // Mistyped value.
        let err = run_query(
            &conn,
            sql,
            &[param("min", SqlParamType::Integer, Some("abc"))],
            &limits(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a valid integer"), "{err}");
        // No value at all.
        let err = run_query(
            &conn,
            sql,
            &[param("min", SqlParamType::Integer, None)],
            &limits(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no value"), "{err}");
        // Unused extra parameter.
        let err = run_query(
            &conn,
            sql,
            &[
                param("min", SqlParamType::Integer, Some("1")),
                param("stray", SqlParamType::Text, Some("x")),
            ],
            &limits(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not used"), "{err}");
        // Positional placeholders are rejected outright.
        let err = run_query(
            &conn,
            "SELECT COUNT(*) FROM d WHERE v = ?",
            &[],
            &limits(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("named parameters"), "{err}");

        // A well-typed run works.
        let out = run_query(
            &conn,
            sql,
            &[param("min", SqlParamType::Integer, Some("1"))],
            &limits(),
            None,
        )
        .unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("1"));
    }

    #[test]
    fn typed_values_narrow_correctly() {
        assert_eq!(
            param("p", SqlParamType::Integer, Some(" 42 "))
                .to_sql_value()
                .unwrap(),
            SqlValue::Integer(42)
        );
        assert_eq!(
            param("p", SqlParamType::Boolean, Some("TRUE"))
                .to_sql_value()
                .unwrap(),
            SqlValue::Integer(1)
        );
        assert_eq!(
            param("p", SqlParamType::Decimal, Some("-12.50"))
                .to_sql_value()
                .unwrap(),
            SqlValue::Real(-12.5)
        );
        assert!(param("p", SqlParamType::Decimal, Some("1e5"))
            .to_sql_value()
            .is_err());
        assert!(param("p", SqlParamType::Float, Some("NaN"))
            .to_sql_value()
            .is_err());
        assert_eq!(
            param("p", SqlParamType::Date, Some("2026-07-18"))
                .to_sql_value()
                .unwrap(),
            SqlValue::Text("2026-07-18".into())
        );
        assert!(param("p", SqlParamType::Date, Some("18/07/2026"))
            .to_sql_value()
            .is_err());
        assert_eq!(
            param("p", SqlParamType::Datetime, Some("2026-07-18T10:30:00"))
                .to_sql_value()
                .unwrap(),
            SqlValue::Text("2026-07-18 10:30:00".into())
        );
        assert_eq!(
            param("p", SqlParamType::Null, None).to_sql_value().unwrap(),
            SqlValue::Null
        );
    }

    // ----- limits, time and cancellation ------------------------------------

    #[test]
    fn row_and_byte_limits_truncate_the_spool() {
        let approved = ApprovedSources::default();
        let d = doc("v\naaaa\nbbbb\ncccc\n");
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();

        let capped = ResolvedLimits {
            max_rows: 2,
            max_bytes: u64::MAX,
            time_limit: Duration::from_secs(60),
        };
        let out = run_query(&conn, "SELECT v FROM d", &[], &capped, None).unwrap();
        assert_eq!(out.rows.len(), 2);
        assert!(out.truncated);

        let capped = ResolvedLimits {
            max_rows: usize::MAX,
            max_bytes: 1,
            time_limit: Duration::from_secs(60),
        };
        let out = run_query(&conn, "SELECT v FROM d", &[], &capped, None).unwrap();
        assert_eq!(out.rows.len(), 1, "stops right after the byte budget");
        assert!(out.truncated);
    }

    #[test]
    fn time_limit_aborts_with_a_clear_message() {
        let approved = ApprovedSources::default();
        let conn = connect_with_sources(&approved, None, &[], &[]).unwrap();
        let tight = ResolvedLimits {
            max_rows: usize::MAX,
            max_bytes: u64::MAX,
            time_limit: Duration::from_millis(30),
        };
        let err = run_query(
            &conn,
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c LIMIT 100000000)
             SELECT COUNT(*) FROM c",
            &[],
            &tight,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("time limit"), "{err}");
    }

    #[test]
    fn cancel_aborts_and_the_connection_stays_usable() {
        let approved = ApprovedSources::default();
        let d = doc("v\n1\n2\n");
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        let result = run_query(&conn, "SELECT v FROM d", &[], &limits(), Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
        // The guard handler was cleared: the same connection runs again.
        let out = run(&conn, "SELECT COUNT(*) FROM d").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("2"));
    }

    #[test]
    fn cancelled_materialize_cleans_its_temp_spill() {
        let cache = tempfile::tempdir().unwrap();
        let result = StoredResult {
            id: 1,
            columns: vec!["a".into()],
            rows: vec![vec![Some("1".into())], vec![Some("2".into())]],
            truncated: false,
            bytes: 2,
        };
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        // force_indexed spills on the very first row; the cancel lands right
        // after, so the spill guard must clean the temp directory up.
        let out = materialize_result(&result, 9, cache.path(), true, Some(&ctx));
        assert!(matches!(out, Err(AppError::Cancelled)));
        let leftovers = std::fs::read_dir(cache.path()).unwrap().count();
        assert_eq!(leftovers, 0, "cancelled spill directory must be removed");
    }

    // ----- results: materialize + export ------------------------------------

    #[test]
    fn results_materialize_editable_and_indexed() {
        let cache = tempfile::tempdir().unwrap();
        let result = StoredResult {
            id: 7,
            columns: vec!["name".into(), "total".into()],
            rows: vec![
                vec![Some("Ada".into()), Some("10".into())],
                vec![Some("Bob".into()), None], // SQL NULL narrows to empty
            ],
            truncated: false,
            bytes: 10,
        };
        let editable = materialize_result(&result, 5, cache.path(), false, None).unwrap();
        assert!(editable.is_editable());
        assert_eq!(editable.headers(), &["name", "total"]);
        assert_eq!(editable.n_rows(), 2);
        assert_eq!(editable.rows()[1], vec!["Bob".to_string(), String::new()]);

        let indexed = materialize_result(&result, 6, cache.path(), true, None).unwrap();
        assert!(!indexed.is_editable(), "forced spill opens indexed");
        assert_eq!(indexed.n_rows(), 2);
        let rows = indexed.fetch_rows(&[0, 1]).unwrap();
        assert_eq!(rows[0][0], "Ada");
        assert_eq!(rows[1][1], "");
    }

    #[test]
    fn results_export_directly_to_csv() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let result = StoredResult {
            id: 3,
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Some("1".into()), None]],
            truncated: false,
            bytes: 1,
        };
        let options = ExportOptions {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            quote_style: "minimal".into(),
            line_ending: "lf".into(),
            bom: false,
            include_headers: true,
            backup: crate::dto::BackupPolicy::None,
        };
        let bytes = export_result(&result, &dest, &options, None).unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), bytes);
        assert_eq!(
            String::from_utf8(std::fs::read(&dest).unwrap()).unwrap(),
            "a,b\n1,\n"
        );
    }

    #[test]
    fn stored_results_window_and_expire() {
        let ws = SqlWorkspace::default();
        let first = ws
            .store_result(vec!["a".into()], vec![vec![Some("1".into())]], false, 1)
            .unwrap();
        assert!(ws.result_arc(first.id).is_ok());
        let second = ws
            .store_result(vec!["a".into()], vec![vec![Some("2".into())]], false, 1)
            .unwrap();
        assert!(
            ws.result_arc(first.id).is_err(),
            "a new run replaces the stored spool"
        );
        assert_eq!(ws.result_arc(second.id).unwrap().rows.len(), 1);
    }

    // ----- snapshot semantics ------------------------------------------------

    #[test]
    fn queries_see_the_current_document_revision() {
        let approved = ApprovedSources::default();
        let d = doc("v\nold\n");
        d.write().unwrap().set_cell(0, 0, "new".into()).unwrap();
        let conn =
            connect_with_sources(&approved, None, &[("d".into(), Arc::clone(&d))], &[]).unwrap();
        let out = run(&conn, "SELECT v FROM d").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("new"));

        // Another edit + a fresh connection (one per run) sees the newest.
        d.write().unwrap().set_cell(0, 0, "newest".into()).unwrap();
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();
        let out = run(&conn, "SELECT v FROM d").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("newest"));
    }

    #[test]
    fn mid_query_edit_aborts_a_live_document_scan() {
        // >200k cells forces the live (revision-check) vtab strategy.
        let mut csv = String::from("a,b,c\n");
        for i in 0..70_000 {
            csv.push_str(&format!("{i},x,y\n"));
        }
        let approved = ApprovedSources::default();
        let d = doc(&csv);
        let conn =
            connect_with_sources(&approved, None, &[("d".into(), Arc::clone(&d))], &[]).unwrap();
        let mut stmt = conn.prepare("SELECT a FROM d").unwrap();
        let mut rows = stmt.query([]).unwrap();
        for _ in 0..1024 {
            assert!(rows.next().unwrap().is_some());
        }
        d.write().unwrap().set_cell(0, 0, "edited".into()).unwrap();
        let mut failed = false;
        loop {
            match rows.next() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => {
                    failed = true;
                    assert!(
                        e.to_string()
                            .contains("changed while the query was running"),
                        "unexpected error: {e}"
                    );
                    break;
                }
            }
        }
        assert!(failed, "a mid-query edit must abort the scan");
    }

    // ----- validation + explain ---------------------------------------------

    #[test]
    fn validation_is_a_prepare_only_dry_run() {
        let approved = ApprovedSources::default();
        let d = doc("a,b\n1,2\n");
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();

        let ok = validate_query(&conn, "SELECT a, b FROM d WHERE a > :min");
        assert!(ok.ok);
        assert_eq!(ok.columns, vec!["a", "b"]);
        assert_eq!(ok.parameters, vec!["min"]);

        let bad = validate_query(&conn, "SELECT nope FROM d");
        assert!(!bad.ok);
        assert!(bad.error.unwrap().contains("nope"));

        let denied = validate_query(&conn, "DELETE FROM d");
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("SQL workspace"));
    }

    #[test]
    fn explain_query_plan_builds_a_tree_and_stays_guarded() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = test_db(dir.path());
        let d = doc("id,label\n1,one\n");
        let conn = connect_with_sources(&approved, Some(&path), &[("d".into(), d)], &[]).unwrap();
        let plan = explain_plan(
            &conn,
            "SELECT * FROM items i JOIN d ON d.id = i.id WHERE i.id > :min",
            &[param("min", SqlParamType::Integer, Some("0"))],
            &limits(),
            None,
        )
        .unwrap();
        assert!(!plan.is_empty(), "a join plan has nodes");
        let all: String = format!("{plan:?}");
        assert!(all.contains("SCAN") || all.contains("SEARCH"), "{all}");

        assert!(explain_plan(&conn, "EXPLAIN SELECT 1", &[], &limits(), None).is_err());
        assert!(explain_plan(&conn, "DELETE FROM items", &[], &limits(), None).is_err());
        assert!(explain_plan(&conn, "SELECT 1; SELECT 2", &[], &limits(), None).is_err());
    }

    #[test]
    fn explain_is_cancellable_and_time_limited_and_releases_its_handler() {
        let approved = ApprovedSources::default();
        let d = doc("id,label\n1,one\n");
        let conn = connect_with_sources(&approved, None, &[("d".into(), d)], &[]).unwrap();

        // Cancellation: a pre-cancelled job aborts the plan step.
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        let cancelled = explain_plan(&conn, "SELECT * FROM d", &[], &limits(), Some(&ctx));
        assert!(matches!(cancelled, Err(AppError::Cancelled)));

        // An already-expired deadline surfaces the time-limit error, not a
        // silent full plan.
        let expired = ResolvedLimits {
            max_rows: usize::MAX,
            max_bytes: u64::MAX,
            time_limit: Duration::from_nanos(0),
        };
        let err = explain_plan(&conn, "SELECT * FROM d", &[], &expired, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("time limit"), "{err}");

        // The progress handler was cleared after each call: the same
        // connection still explains normally (no leaked interrupt handler).
        let plan = explain_plan(&conn, "SELECT * FROM d", &[], &limits(), None).unwrap();
        assert!(!plan.is_empty());
    }

    // ----- history -----------------------------------------------------------

    #[test]
    fn history_is_a_persisted_ring_never_executed_on_load() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..105u64 {
            push_history(
                dir.path(),
                SqlHistoryEntry {
                    sql: format!("SELECT {i}"),
                    params: vec![param("p", SqlParamType::Integer, Some("1"))],
                    sources: vec!["d".into()],
                    ran_at_ms: i,
                    status: "done".into(),
                    row_count: Some(1),
                    error: None,
                },
            )
            .unwrap();
        }
        // Loading is pure data access: a history full of statements (even
        // hostile ones) is never prepared or executed here.
        let loaded = settings::load_settings(dir.path());
        assert_eq!(loaded.sql_history.len(), SQL_HISTORY_CAP);
        assert_eq!(loaded.sql_history[0].sql, "SELECT 104", "newest first");
        assert_eq!(loaded.sql_history[0].params.len(), 1);
        assert_eq!(
            loaded.sql_history[SQL_HISTORY_CAP - 1].sql,
            format!("SELECT {}", 105 - SQL_HISTORY_CAP)
        );
    }

    // ----- schema browser ----------------------------------------------------

    #[test]
    fn schema_dtos_are_bounded_and_typed() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = test_db(dir.path());
        let (tables, truncated) = db_table_infos(&approved, &path).unwrap();
        assert!(!truncated);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].alias, "items");
        assert_eq!(tables[0].kind, "table");
        assert_eq!(tables[0].columns.len(), 3);
        assert_eq!(tables[0].columns[0].decl_type, "integer");

        // An unapproved database path is refused here too.
        let other = dir.path().join("other.sqlite");
        Connection::open(&other).unwrap();
        assert!(db_table_infos(&approved, &other).is_err());

        let d = doc("a,b\n1,2\n");
        let guard = d.read().unwrap();
        let info = doc_table_info("mydoc", &guard);
        assert_eq!(info.alias, "mydoc");
        assert_eq!(info.kind, "document");
        assert_eq!(info.row_count, Some(1));
        assert_eq!(info.columns[0].decl_type, "text");
    }

    // ----- source-name collisions with a selected database -------------------

    #[test]
    fn temp_vtab_would_shadow_a_same_named_db_table() {
        // Evidence the collision guard is load-bearing: a temp-schema vtab
        // named like a real database table shadows it for an unqualified
        // reference (SQLite resolves temp before main), so without the guard a
        // query silently reads the document/file instead of the table.
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = test_db(dir.path()); // real table `items` (3 rows)
        let d = doc("id,name,price\n99,DOCVALUE,0\n"); // exposed AS `items`
        let conn =
            connect_with_sources(&approved, Some(&path), &[("items".into(), d)], &[]).unwrap();

        // Unqualified `items` resolves to the temp doc vtab, NOT the db table.
        let out = run(&conn, "SELECT name FROM items").unwrap();
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0][0].as_deref(), Some("DOCVALUE"));
        // main-qualification (the guard's suggested escape hatch) reaches the
        // real database table.
        let out = run(&conn, "SELECT COUNT(*) FROM main.items").unwrap();
        assert_eq!(out.rows[0][0].as_deref(), Some("3"));
    }

    #[test]
    fn selected_source_shadowing_a_db_table_is_refused_in_strict_mode() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (approved, db) = test_db(dir.path()); // db has table `items`

        // Register an approved file whose alias sanitizes to `items`.
        let file = dir.path().join("items.csv");
        std::fs::write(&file, "x\n1\n").unwrap();
        approved.approve(&file).unwrap();
        let workspace = SqlWorkspace::default();
        let (canonical, kind, backing) =
            open_backing(&approved, &file, None, cache.path(), None).unwrap();
        assert_eq!(
            workspace.register(canonical, kind, backing).unwrap().alias,
            "items"
        );

        let state = Mutex::new(AppState::default());
        let db_str = db.to_string_lossy().to_string();
        let clash = SqlSourceSelection {
            documents: vec![],
            files: vec!["items".into()],
            database: Some(db_str.clone()),
        };

        // Strict (run / validate / explain): the clash is refused, clearly.
        // (`.err()` rather than `.unwrap_err()` — AssembledSources holds a
        // trait object and is not Debug.)
        let err = assemble_sources(&state, &workspace, &approved, &clash, true)
            .err()
            .expect("a name clash must be refused")
            .to_string();
        assert!(
            err.contains("items") && err.contains("selected database"),
            "{err}"
        );

        // Lenient (schema browser): still assembles so every source is listed.
        assert!(assemble_sources(&state, &workspace, &approved, &clash, false).is_ok());

        // No database selected → no clash possible even in strict mode.
        let no_db = SqlSourceSelection {
            documents: vec![],
            files: vec!["items".into()],
            database: None,
        };
        assert!(assemble_sources(&state, &workspace, &approved, &no_db, true).is_ok());

        // A non-colliding database table name is fine in strict mode too.
        let other = dir.path().join("widgets.csv");
        std::fs::write(&other, "x\n1\n").unwrap();
        approved.approve(&other).unwrap();
        let (c2, k2, b2) = open_backing(&approved, &other, None, cache.path(), None).unwrap();
        let a2 = workspace.register(c2, k2, b2).unwrap().alias;
        assert_eq!(a2, "widgets");
        let ok = SqlSourceSelection {
            documents: vec![],
            files: vec!["widgets".into()],
            database: Some(db_str),
        };
        assert!(assemble_sources(&state, &workspace, &approved, &ok, true).is_ok());
    }

    // ----- saved queries (project section payload) ---------------------------

    #[test]
    fn saved_queries_round_trip_and_preserve_unknown_fields() {
        let json = serde_json::json!({
            "id": "q1",
            "name": "My query",
            "sql": "SELECT broken FROM nowhere ; DROP TABLE x", // never validated here
            "params": [{"name": "min", "type": "integer", "value": "1"}],
            "sources": ["src-1"],
            "futureField": {"keep": true}
        });
        let q: SavedQuery = serde_json::from_value(json).unwrap();
        assert_eq!(q.name, "My query");
        assert_eq!(q.params[0].param_type, SqlParamType::Integer);
        let back = serde_json::to_value(&q).unwrap();
        assert_eq!(back["futureField"]["keep"], serde_json::json!(true));
        assert_eq!(back["params"][0]["type"], serde_json::json!("integer"));
    }
}
