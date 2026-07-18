//! SafeQueryEngine core (F35, the foundation F36 queries build on).
//!
//! Every SQL touchpoint in CEESVEE goes through this module, which enforces
//! four independent layers of containment:
//!
//! 1. **Approved-source registry** ([`ApprovedSources`]): only database file
//!    paths the user explicitly opened (plus open CEESVEE documents, exposed
//!    as virtual tables) are reachable. Nothing else on disk is readable —
//!    the connection factory refuses unapproved paths outright.
//! 2. **Authorizer-guarded connections** ([`connect`]): every connection is
//!    opened read-only and carries a SQLite authorizer that denies everything
//!    except SELECT-class operations — `ATTACH`, DDL, DML, `PRAGMA` writes
//!    and `load_extension` all fail at prepare time. Read-only metadata
//!    pragmas (`table_info`, `data_version`, …) are allowlisted by name.
//! 3. **Resource limits** ([`QueryLimits`], [`run_select`]): result
//!    materialisation is capped by row count and byte budget; results are
//!    truncated, never unbounded.
//! 4. **Cancellation** ([`install_cancel_handler`]): a SQLite progress
//!    handler polls the job's [`CancelToken`] every few thousand VM steps, so
//!    a long scan aborts with `SQLITE_INTERRUPT` (mapped to
//!    [`AppError::Cancelled`]) without waiting for a row boundary.
//!
//! ## Document virtual tables and revision snapshots
//!
//! [`connect`] can expose open CEESVEE documents as read-only virtual tables
//! (module `ceesvee_doc`, one temp-schema table per exposed document). A
//! query must see ONE consistent revision even if the user edits the
//! document mid-query, so the vtab picks a strategy at connect time:
//!
//! * **Snapshot copy** — documents small enough by BOTH cell count
//!   ([`SNAPSHOT_MAX_CELLS`]) and total size ([`SNAPSHOT_MAX_BYTES`]) are
//!   copied outright; the query reads the copy and concurrent edits are
//!   invisible. The byte budget bounds memory even for a document with few
//!   but very large cells: at most a few MB for the largest snapshot.
//! * **Revision-check-abort** — larger documents are read live in bounded
//!   windows; the revision pinned at connect time is re-checked on every
//!   window refill and a mismatch aborts the query with a clear error. This
//!   trades a retryable failure for never buffering a huge document, and
//!   never returns rows from two different revisions.
//!
//! The exposure list is fixed per connection (vtabs are created BEFORE the
//! authorizer is installed, because `CREATE VIRTUAL TABLE` is DDL that the
//! guard would deny). Connections are cheap; callers wanting a different
//! document set open a new one.
//!
//! ## Generic tabular virtual tables (F36)
//!
//! [`connect_with_sources`] additionally exposes arbitrary
//! [`TabularSource`]s (module `ceesvee_src`) — the F36 SQL workspace uses it
//! to make approved CSV/JSON/Parquet/Arrow FILES queryable without a full
//! import. Reads are windowed (bounded memory) and the source's
//! [`ContentFingerprint`] is pinned at connect time and re-checked on every
//! window refill, so a file rewritten mid-query aborts instead of mixing
//! rows from two versions. Cells surface the tabular missing/`NULL` bit as
//! SQL `NULL`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_int, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::ffi;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;
use rusqlite::vtab::{
    dequote, escape_double_quote, Context, CreateVTab, Filters, IndexInfo, Module, VTab,
    VTabConfig, VTabConnection, VTabCursor, VTabKind,
};
use rusqlite::{Connection, OpenFlags};

use crate::error::{AppError, AppResult};
use crate::job::CancelToken;
use crate::state::SharedDocument;
use crate::tabular::{ContentFingerprint, RowCountHint, TabularRow, TabularSource};

/// Documents whose `rows × columns` is at or below this are snapshot-copied
/// at vtab connect time; larger documents use revision-check-abort (see the
/// module docs for the trade-off).
pub(crate) const SNAPSHOT_MAX_CELLS: usize = 200_000;
/// A snapshot copy also stops here: a document with few but very large cells
/// (long free text, embedded JSON, blobs-as-text) can slip under
/// [`SNAPSHOT_MAX_CELLS`] yet still be hundreds of MB, so the copy is bounded
/// by accumulated bytes too and falls back to revision-check-abort the moment
/// this budget is crossed. Sized so the largest snapshot stays within a few MB.
pub(crate) const SNAPSHOT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Rows fetched per window when reading a large document live.
const LIVE_WINDOW: usize = 1024;
/// SQLite VM steps between cancellation polls (sub-millisecond granularity).
const PROGRESS_STEP_OPS: c_int = 4_000;
/// How long a guarded connection waits on a locked database before failing.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Default result caps for [`run_select`]: generous enough for a full F36
/// result grid, small enough that a runaway cross join cannot exhaust memory.
const DEFAULT_MAX_ROWS: usize = 250_000;
const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> AppError {
        if let rusqlite::Error::SqliteFailure(f, msg) = &e {
            match f.code {
                // A progress-handler abort surfaces as SQLITE_INTERRUPT; map
                // it onto the app's cancellation error so job status and
                // cleanup behave exactly like every other cancelled job.
                ffi::ErrorCode::OperationInterrupted => return AppError::Cancelled,
                ffi::ErrorCode::AuthorizationForStatementDenied => {
                    return AppError::invalid(format!(
                        "this operation is not allowed on a database opened in CEESVEE \
                         (read-only queries only): {}",
                        msg.as_deref().unwrap_or("not authorized")
                    ));
                }
                _ => {}
            }
        }
        AppError::Other(format!("database error: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Approved-source registry
// ---------------------------------------------------------------------------

/// The explicit allowlist of database files the SafeQueryEngine may open.
/// Paths enter it only when the user picks a file (the F35 open flow);
/// everything is compared canonicalized so path spellings cannot bypass it.
/// Open CEESVEE documents are the other approved source kind — they are
/// passed straight to [`connect`] as vtab exposures, never read from disk.
#[derive(Default)]
pub struct ApprovedSources {
    paths: Mutex<HashSet<PathBuf>>,
}

impl ApprovedSources {
    /// Record a user-approved database file, returning its canonical path.
    pub fn approve(&self, path: &Path) -> AppResult<PathBuf> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| AppError::invalid(format!("cannot open database file: {e}")))?;
        self.paths
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?
            .insert(canonical.clone());
        Ok(canonical)
    }

    /// Resolve `path` against the registry; unapproved paths are refused.
    pub fn check(&self, path: &Path) -> AppResult<PathBuf> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| AppError::invalid(format!("cannot open database file: {e}")))?;
        let approved = self
            .paths
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?
            .contains(&canonical);
        if approved {
            Ok(canonical)
        } else {
            Err(AppError::invalid(
                "this database file has not been opened in CEESVEE; open it first",
            ))
        }
    }

    /// Forget an approval (e.g. when the last browser session closes).
    pub fn revoke(&self, canonical: &Path) {
        if let Ok(mut paths) = self.paths.lock() {
            paths.remove(canonical);
        }
    }
}

// ---------------------------------------------------------------------------
// The authorizer
// ---------------------------------------------------------------------------

/// Read-only metadata pragmas the guarded connection may use. SQLite hands
/// `PRAGMA name(arg)` to the authorizer with `arg` as the "value", so the
/// allowlist is by NAME — every name here is a pure read regardless of the
/// argument form. Anything not listed (including every write pragma) is
/// denied.
const READONLY_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "database_list",
    "collation_list",
    "function_list",
    "data_version",
    "schema_version",
    "user_version",
    "application_id",
    "freelist_count",
    "page_count",
    "page_size",
    "encoding",
];

/// The deny-by-default authorizer: SELECT-class reads only. Installed on
/// every connection [`connect`] hands out, AFTER any document vtabs are
/// created (their `CREATE VIRTUAL TABLE` is DDL this guard would deny).
fn authorize(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        // Reading: the whole point.
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => {
            Authorization::Allow
        }
        // Read transactions/savepoints are harmless on a read-only handle
        // and let callers take a consistent multi-statement view.
        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => Authorization::Allow,
        // Functions: everything except extension loading. The SQL
        // `load_extension()` function is additionally disabled at the C
        // level by default; the deny here is defence in depth.
        AuthAction::Function { function_name } => {
            if function_name.eq_ignore_ascii_case("load_extension") {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }
        AuthAction::Pragma { pragma_name, .. } => {
            if READONLY_PRAGMAS
                .iter()
                .any(|p| pragma_name.eq_ignore_ascii_case(p))
            {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        // Everything else — ATTACH/DETACH, all DDL and DML, ANALYZE,
        // REINDEX, vtab creation — is denied.
        _ => Authorization::Deny,
    }
}

// ---------------------------------------------------------------------------
// Connection factory
// ---------------------------------------------------------------------------

/// Open a guarded, read-only connection.
///
/// * `path`: an APPROVED database file, or `None` for a document-only
///   connection (an empty in-memory main database).
/// * `documents`: open CEESVEE documents to expose as read-only virtual
///   tables named `alias` (in the temp schema, so they vanish with the
///   connection). The set is fixed for the connection's lifetime.
///
/// The authorizer is installed last; from then on the connection can only
/// run SELECT-class statements (see [`authorize`]).
pub fn connect(
    approved: &ApprovedSources,
    path: Option<&Path>,
    documents: &[(String, SharedDocument)],
) -> AppResult<Connection> {
    connect_with_sources(approved, path, documents, &[])
}

/// A thread-safe, owned tabular source exposed as a `ceesvee_src` virtual
/// table (the F36 approved-file backings).
pub type SharedTabular = Arc<dyn TabularSource + Send + Sync>;

/// [`connect`], plus arbitrary [`TabularSource`]s exposed as read-only
/// virtual tables named `alias` (module docs: generic tabular vtabs). Like
/// document vtabs, the set is fixed for the connection's lifetime and every
/// vtab lands in the temp schema, created BEFORE the authorizer.
pub fn connect_with_sources(
    approved: &ApprovedSources,
    path: Option<&Path>,
    documents: &[(String, SharedDocument)],
    sources: &[(String, SharedTabular)],
) -> AppResult<Connection> {
    let conn = match path {
        Some(p) => {
            let canonical = approved.check(p)?;
            open_readonly_approved(&canonical)?
        }
        None => Connection::open_in_memory()?,
    };
    conn.busy_timeout(BUSY_TIMEOUT)?;
    if !documents.is_empty() {
        let exposed: ExposedDocs = Arc::default();
        register_document_module(&conn, Arc::clone(&exposed))?;
        for (alias, doc) in documents {
            expose_document(&conn, &exposed, alias, Arc::clone(doc))?;
        }
    }
    if !sources.is_empty() {
        let exposed: ExposedSrcs = Arc::default();
        register_source_module(&conn, Arc::clone(&exposed))?;
        for (alias, source) in sources {
            expose_source(&conn, &exposed, alias, Arc::clone(source))?;
        }
    }
    conn.authorizer(Some(authorize))?;
    Ok(conn)
}

/// Open an ALREADY-canonicalized, already-approved path read-only (no vtabs,
/// authorizer installed). Internal fast path for the F35 browser, which
/// approves once and opens several connections against the same file.
pub(crate) fn open_guarded(canonical: &Path) -> AppResult<Connection> {
    let conn = open_readonly_approved(canonical)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.authorizer(Some(authorize))?;
    Ok(conn)
}

fn open_readonly_approved(canonical: &Path) -> AppResult<Connection> {
    // READ_ONLY: writes fail at the OS level even if every other layer were
    // bypassed. No URI flag: plain paths only, `file:` tricks never parse.
    Ok(Connection::open_with_flags(
        canonical,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// Poll `token` every few thousand SQLite VM steps; a cancelled token aborts
/// the running statement with `SQLITE_INTERRUPT` → [`AppError::Cancelled`].
pub fn install_cancel_handler(conn: &Connection, token: CancelToken) -> AppResult<()> {
    conn.progress_handler(PROGRESS_STEP_OPS, Some(move || token.is_cancelled()))?;
    Ok(())
}

/// Remove a previously installed cancellation poll (used when a connection
/// outlives the job that opened it, e.g. the F35 table backing).
pub fn clear_cancel_handler(conn: &Connection) -> AppResult<()> {
    conn.progress_handler(0, None::<fn() -> bool>)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Limits + bounded SELECT materialisation
// ---------------------------------------------------------------------------

/// Caps applied when materialising a result set.
#[derive(Debug, Clone, Copy)]
pub struct QueryLimits {
    /// Maximum rows collected; further rows mark the output truncated.
    pub max_rows: usize,
    /// Maximum accumulated cell bytes; crossing it stops collection.
    pub max_bytes: u64,
}

impl Default for QueryLimits {
    fn default() -> QueryLimits {
        QueryLimits {
            max_rows: DEFAULT_MAX_ROWS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// A bounded, fully materialised result set. Cells are `None` for SQL
/// `NULL` (the missing-vs-empty distinction the tabular contract carries).
#[derive(Debug, Clone)]
pub struct QueryOutput {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// True when a limit stopped collection before the result set ended.
    pub truncated: bool,
}

/// Run one read-only SELECT-class statement and materialise its result under
/// `limits`, polling `cancel` between rows. The authorizer already blocks
/// non-read statements at prepare time; the explicit `readonly` check here
/// is belt and braces.
pub fn run_select(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limits: QueryLimits,
    cancel: Option<&CancelToken>,
) -> AppResult<QueryOutput> {
    let mut stmt = conn.prepare(sql)?;
    if !stmt.readonly() {
        return Err(AppError::invalid(
            "only read-only statements can run against a database opened in CEESVEE",
        ));
    }
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    if columns.is_empty() {
        return Err(AppError::invalid("the statement returns no columns"));
    }
    let n_cols = columns.len();

    let mut out: Vec<Vec<Option<String>>> = Vec::new();
    let mut bytes = 0u64;
    let mut truncated = false;
    let mut rows = stmt.query(params)?;
    while let Some(row) = rows.next()? {
        if let Some(token) = cancel {
            if token.is_cancelled() {
                return Err(AppError::Cancelled);
            }
        }
        if out.len() >= limits.max_rows {
            truncated = true;
            break;
        }
        let mut cells: Vec<Option<String>> = Vec::with_capacity(n_cols);
        for i in 0..n_cols {
            let cell = value_to_text(row.get_ref(i)?);
            bytes += cell.as_deref().map_or(0, str::len) as u64 + 8;
            cells.push(cell);
        }
        out.push(cells);
        if bytes > limits.max_bytes {
            truncated = true;
            break;
        }
    }
    Ok(QueryOutput {
        columns,
        rows: out,
        truncated,
    })
}

/// Canonical SQL-value → text narrowing used everywhere F35 shows or stores
/// database cells: `NULL` stays distinct (`None`); numbers print in Rust's
/// shortest round-trip form; BLOBs render as a lossy placeholder — binary
/// columns are out of scope for the tabular contract this cycle (see
/// `tabular.rs`, "Text values").
pub fn value_to_text(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(format!("[BLOB {} bytes]", b.len())),
    }
}

// ---------------------------------------------------------------------------
// The document virtual table
// ---------------------------------------------------------------------------

/// Alias → document map handed to the vtab module as aux data.
type ExposedDocs = Arc<Mutex<HashMap<String, SharedDocument>>>;

const DOC_MODULE_NAME: &CStr = c"ceesvee_doc";

fn register_document_module(conn: &Connection, docs: ExposedDocs) -> AppResult<()> {
    // A `const` promotes to a `'static` borrow (same pattern as rusqlite's
    // own generate_series module).
    const MODULE: Module<DocVTab> = Module::read_only_module();
    conn.create_module(DOC_MODULE_NAME, &MODULE, Some(docs))?;
    Ok(())
}

/// Create the temp-schema virtual table for one exposed document. Must run
/// BEFORE the authorizer is installed.
fn expose_document(
    conn: &Connection,
    exposed: &ExposedDocs,
    alias: &str,
    doc: SharedDocument,
) -> AppResult<()> {
    if alias.is_empty() || alias.contains('\0') {
        return Err(AppError::invalid("invalid document table name"));
    }
    exposed
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .insert(alias.to_string(), doc);
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE temp.\"{}\" USING ceesvee_doc('{}')",
        escape_double_quote(alias),
        alias.replace('\'', "''"),
    ))?;
    Ok(())
}

/// How a connected document vtab serves rows (module docs: revision
/// snapshot semantics).
#[derive(Clone)]
enum Strategy {
    /// The whole document was copied at connect time; concurrent edits are
    /// invisible to the query.
    Snapshot(Arc<Vec<Vec<String>>>),
    /// Live windowed reads; `revision` was pinned at connect time and every
    /// window refill re-checks it, aborting the query on a mismatch.
    Live { doc: SharedDocument, revision: u64 },
}

/// One `ceesvee_doc` virtual table (one exposed document).
#[repr(C)]
pub struct DocVTab {
    base: ffi::sqlite3_vtab,
    n_cols: usize,
    n_rows: usize,
    strategy: Strategy,
}

unsafe impl<'vtab> VTab<'vtab> for DocVTab {
    type Aux = ExposedDocs;
    type Cursor = DocCursor;

    fn connect(
        db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        args: &[&[u8]],
    ) -> rusqlite::Result<(Cow<'static, CStr>, Self)> {
        let aux = aux.ok_or_else(|| module_err("document registry missing"))?;
        let raw = args
            .first()
            .ok_or_else(|| module_err("usage: ceesvee_doc('<alias>')"))?;
        let alias_arg = std::str::from_utf8(raw)
            .map_err(|_| module_err("document table name must be UTF-8"))?;
        let alias = dequote(alias_arg.trim());
        let doc = {
            let map = aux
                .lock()
                .map_err(|_| module_err("document registry poisoned"))?;
            map.get(alias.as_ref())
                .cloned()
                .ok_or_else(|| module_err(&format!("no exposed document named '{alias}'")))?
        };

        let guard = doc
            .read()
            .map_err(|_| module_err("document lock poisoned"))?;
        let headers = guard.headers().to_vec();
        let n_cols = headers.len();
        let n_rows = guard.n_rows();

        let strategy = if n_rows.saturating_mul(n_cols.max(1)) <= SNAPSHOT_MAX_CELLS {
            // Copy under BOTH a cell-count and a byte budget. A document with
            // few but very large cells passes the cell gate yet can dwarf "a
            // few MB", so track accumulated bytes and fall back to the live
            // (revision-check) strategy the instant the byte budget is crossed
            // — never buffering hundreds of MB on a 6 GB machine.
            let mut copy: Vec<Vec<String>> = Vec::with_capacity(n_rows);
            let mut bytes: usize = 0;
            let mut over_budget = false;
            guard
                .visit_rows(0..n_rows, &mut |_, row| {
                    for cell in row {
                        bytes = bytes.saturating_add(cell.len());
                    }
                    if bytes > SNAPSHOT_MAX_BYTES {
                        over_budget = true;
                        return Ok(false);
                    }
                    copy.push(row.to_vec());
                    Ok(true)
                })
                .map_err(|e| module_err(&e.to_string()))?;
            if over_budget {
                let revision = guard.revision();
                drop(guard);
                Strategy::Live { doc, revision }
            } else {
                Strategy::Snapshot(Arc::new(copy))
            }
        } else {
            let revision = guard.revision();
            drop(guard);
            Strategy::Live { doc, revision }
        };

        let decl = text_table_decl(&headers)?;

        // Not usable from within triggers or views of the outer database:
        // queries must name the document table directly.
        db.config(VTabConfig::DirectOnly)?;

        Ok((
            decl,
            DocVTab {
                base: ffi::sqlite3_vtab::default(),
                n_cols,
                n_rows,
                strategy,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<bool> {
        // Full scan only; the engine filters. Cost scales with size so the
        // planner puts the document on the right side of joins.
        info.set_estimated_cost(self.n_rows as f64 * 10.0 + 1.0);
        info.set_estimated_rows(self.n_rows.max(1) as i64);
        Ok(true)
    }

    fn open(&'vtab mut self) -> rusqlite::Result<DocCursor> {
        Ok(DocCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            n_rows: self.n_rows,
            strategy: self.strategy.clone(),
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
        })
    }
}

impl<'vtab> CreateVTab<'vtab> for DocVTab {
    const KIND: VTabKind = VTabKind::Default;
}

/// Cursor over one document vtab. `buffer` is only used by the live
/// strategy; snapshots index the shared copy directly.
#[repr(C)]
pub struct DocCursor {
    base: ffi::sqlite3_vtab_cursor,
    n_rows: usize,
    strategy: Strategy,
    pos: usize,
    buffer: Vec<Vec<String>>,
    buffer_start: usize,
}

impl DocCursor {
    /// Refill the live window so `pos` is buffered, re-checking the pinned
    /// revision: results must never mix rows from two revisions.
    fn ensure_buffered(&mut self) -> rusqlite::Result<()> {
        let Strategy::Live { doc, revision } = &self.strategy else {
            return Ok(());
        };
        if self.pos >= self.n_rows
            || (self.pos >= self.buffer_start && self.pos < self.buffer_start + self.buffer.len())
        {
            return Ok(());
        }
        let guard = doc
            .read()
            .map_err(|_| module_err("document lock poisoned"))?;
        if guard.revision() != *revision {
            return Err(module_err(
                "the document changed while the query was running; run the query again",
            ));
        }
        let start = self.pos;
        let end = (start + LIVE_WINDOW).min(self.n_rows);
        let mut window: Vec<Vec<String>> = Vec::with_capacity(end - start);
        guard
            .visit_rows(start..end, &mut |_, row| {
                window.push(row.to_vec());
                Ok(true)
            })
            .map_err(|e| module_err(&e.to_string()))?;
        if window.len() != end - start {
            return Err(module_err(
                "the document changed while the query was running; run the query again",
            ));
        }
        self.buffer = window;
        self.buffer_start = start;
        Ok(())
    }
}

unsafe impl VTabCursor for DocCursor {
    fn filter(
        &mut self,
        _idx_num: c_int,
        _idx_str: Option<&str>,
        _args: &Filters<'_>,
    ) -> rusqlite::Result<()> {
        self.pos = 0;
        self.buffer.clear();
        self.buffer_start = 0;
        self.ensure_buffered()
    }

    fn next(&mut self) -> rusqlite::Result<()> {
        self.pos += 1;
        self.ensure_buffered()
    }

    fn eof(&self) -> bool {
        self.pos >= self.n_rows
    }

    fn column(&self, ctx: &mut Context, i: c_int) -> rusqlite::Result<()> {
        let row = match &self.strategy {
            Strategy::Snapshot(rows) => rows.get(self.pos),
            Strategy::Live { .. } => self.buffer.get(self.pos - self.buffer_start),
        };
        let cell = row
            .and_then(|r| r.get(i as usize))
            .map_or("", String::as_str);
        ctx.set_result(&cell)
    }

    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok(self.pos as i64 + 1)
    }
}

fn module_err(msg: &str) -> rusqlite::Error {
    rusqlite::Error::ModuleError(msg.to_string())
}

/// Build a `CREATE TABLE` declaration (all-TEXT columns) from raw column
/// names: control characters stripped, empty names replaced positionally,
/// duplicates disambiguated — without touching the underlying source.
fn text_table_decl(names: &[String]) -> rusqlite::Result<Cow<'static, CStr>> {
    let mut used: Vec<String> = Vec::with_capacity(names.len());
    let mut decl = String::from("CREATE TABLE x(");
    for (i, header) in names.iter().enumerate() {
        let base: String = header.chars().filter(|c| *c != '\0').collect();
        let base = if base.is_empty() {
            format!("column_{}", i + 1)
        } else {
            base
        };
        let name = crate::derived::unique_column_name(&used, &base);
        used.push(name.clone());
        if i > 0 {
            decl.push_str(", ");
        }
        decl.push('"');
        decl.push_str(&escape_double_quote(&name));
        decl.push_str("\" TEXT");
    }
    decl.push(')');
    CString::new(decl)
        .map(Cow::Owned)
        .map_err(|_| module_err("invalid column names"))
}

// ---------------------------------------------------------------------------
// The generic tabular-source virtual table (F36)
// ---------------------------------------------------------------------------

/// Alias → source map handed to the `ceesvee_src` module as aux data.
type ExposedSrcs = Arc<Mutex<HashMap<String, SharedTabular>>>;

const SRC_MODULE_NAME: &CStr = c"ceesvee_src";

fn register_source_module(conn: &Connection, srcs: ExposedSrcs) -> AppResult<()> {
    const MODULE: Module<SrcVTab> = Module::read_only_module();
    conn.create_module(SRC_MODULE_NAME, &MODULE, Some(srcs))?;
    Ok(())
}

/// Create the temp-schema virtual table for one exposed tabular source. Must
/// run BEFORE the authorizer is installed.
fn expose_source(
    conn: &Connection,
    exposed: &ExposedSrcs,
    alias: &str,
    source: SharedTabular,
) -> AppResult<()> {
    if alias.is_empty() || alias.contains('\0') {
        return Err(AppError::invalid("invalid source table name"));
    }
    exposed
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .insert(alias.to_string(), source);
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE temp.\"{}\" USING ceesvee_src('{}')",
        escape_double_quote(alias),
        alias.replace('\'', "''"),
    ))?;
    Ok(())
}

/// One `ceesvee_src` virtual table (one exposed [`TabularSource`]). Reads
/// are windowed and never buffer more than [`LIVE_WINDOW`] rows; the
/// source's fingerprint is pinned at connect time and re-checked on every
/// window refill so a query never mixes rows from two source versions.
#[repr(C)]
pub struct SrcVTab {
    base: ffi::sqlite3_vtab,
    n_cols: usize,
    /// Row-count hint for the planner (`None` when the source has none).
    row_hint: Option<u64>,
    source: SharedTabular,
    pinned: ContentFingerprint,
}

unsafe impl<'vtab> VTab<'vtab> for SrcVTab {
    type Aux = ExposedSrcs;
    type Cursor = SrcCursor;

    fn connect(
        db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        args: &[&[u8]],
    ) -> rusqlite::Result<(Cow<'static, CStr>, Self)> {
        let aux = aux.ok_or_else(|| module_err("source registry missing"))?;
        let raw = args
            .first()
            .ok_or_else(|| module_err("usage: ceesvee_src('<alias>')"))?;
        let alias_arg =
            std::str::from_utf8(raw).map_err(|_| module_err("source table name must be UTF-8"))?;
        let alias = dequote(alias_arg.trim());
        let source = {
            let map = aux
                .lock()
                .map_err(|_| module_err("source registry poisoned"))?;
            map.get(alias.as_ref())
                .cloned()
                .ok_or_else(|| module_err(&format!("no exposed source named '{alias}'")))?
        };

        let names: Vec<String> = source.columns().into_iter().map(|c| c.name).collect();
        let decl = text_table_decl(&names)?;
        let row_hint = match source.row_count() {
            RowCountHint::Exact(n) | RowCountHint::Estimate(n) => Some(n),
            RowCountHint::Unknown => None,
        };

        // Not usable from within triggers or views of the outer database:
        // queries must name the source table directly.
        db.config(VTabConfig::DirectOnly)?;

        let pinned = source.fingerprint();
        Ok((
            decl,
            SrcVTab {
                base: ffi::sqlite3_vtab::default(),
                n_cols: names.len(),
                row_hint,
                source,
                pinned,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<bool> {
        // Full scan only; the engine filters. Cost scales with size so the
        // planner puts large sources on the right side of joins.
        let n = self.row_hint.unwrap_or(10_000);
        info.set_estimated_cost(n as f64 * 10.0 + 1.0);
        info.set_estimated_rows(n.max(1) as i64);
        Ok(true)
    }

    fn open(&'vtab mut self) -> rusqlite::Result<SrcCursor> {
        Ok(SrcCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            source: Arc::clone(&self.source),
            pinned: self.pinned,
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
            at_end: true,
        })
    }
}

impl<'vtab> CreateVTab<'vtab> for SrcVTab {
    const KIND: VTabKind = VTabKind::Default;
}

/// Cursor over one tabular-source vtab: a sliding window of at most
/// [`LIVE_WINDOW`] rows.
#[repr(C)]
pub struct SrcCursor {
    base: ffi::sqlite3_vtab_cursor,
    source: SharedTabular,
    pinned: ContentFingerprint,
    pos: usize,
    buffer: Vec<TabularRow>,
    buffer_start: usize,
    at_end: bool,
}

impl SrcCursor {
    /// Refill the window so `pos` is buffered, re-checking the pinned
    /// fingerprint: results must never mix rows from two source versions.
    /// An empty read marks the end of the scan.
    fn ensure_buffered(&mut self) -> rusqlite::Result<()> {
        if self.at_end
            || (self.pos >= self.buffer_start && self.pos < self.buffer_start + self.buffer.len())
        {
            return Ok(());
        }
        if self.source.fingerprint() != self.pinned {
            return Err(module_err(
                "the source changed while the query was running; run the query again",
            ));
        }
        let window = self
            .source
            .read_rows(self.pos as u64, LIVE_WINDOW, None)
            .map_err(|e| module_err(&e.to_string()))?;
        self.buffer_start = self.pos;
        if window.is_empty() {
            self.buffer.clear();
            self.at_end = true;
        } else {
            self.buffer = window;
        }
        Ok(())
    }
}

unsafe impl VTabCursor for SrcCursor {
    fn filter(
        &mut self,
        _idx_num: c_int,
        _idx_str: Option<&str>,
        _args: &Filters<'_>,
    ) -> rusqlite::Result<()> {
        self.pos = 0;
        self.buffer.clear();
        self.buffer_start = 0;
        self.at_end = false;
        self.ensure_buffered()
    }

    fn next(&mut self) -> rusqlite::Result<()> {
        self.pos += 1;
        self.ensure_buffered()
    }

    fn eof(&self) -> bool {
        self.at_end
    }

    fn column(&self, ctx: &mut Context, i: c_int) -> rusqlite::Result<()> {
        // A tabular missing/NULL cell (`None`) surfaces as SQL NULL, keeping
        // the missing-vs-empty distinction the contract carries.
        let cell = self
            .buffer
            .get(self.pos - self.buffer_start)
            .and_then(|r| r.get(i as usize))
            .cloned()
            .flatten();
        ctx.set_result(&cell)
    }

    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok(self.pos as i64 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};
    use std::sync::RwLock;

    fn doc(csv: &str) -> SharedDocument {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Arc::new(RwLock::new(Document::from_parsed(1, None, parsed, true)))
    }

    /// A document big enough to force the live (revision-check) strategy.
    fn big_doc() -> SharedDocument {
        let mut csv = String::from("n\n");
        for i in 0..(SNAPSHOT_MAX_CELLS + 10) {
            csv.push_str(&format!("{i}\n"));
        }
        doc(&csv)
    }

    /// A document with FEW rows but very large cells: comfortably under
    /// [`SNAPSHOT_MAX_CELLS`] yet over [`SNAPSHOT_MAX_BYTES`], and past
    /// [`LIVE_WINDOW`] rows so a mid-query edit hits a window refill.
    fn wide_cell_doc() -> SharedDocument {
        let cell = "x".repeat(8192);
        let rows = SNAPSHOT_MAX_BYTES / 8192 + 200; // safely over the byte budget
        let mut csv = String::from("v\n");
        for _ in 0..rows {
            csv.push_str(&cell);
            csv.push('\n');
        }
        doc(&csv)
    }

    fn test_db(dir: &Path) -> PathBuf {
        let path = dir.join("test.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT, price REAL);
             CREATE INDEX idx_items_name ON items(name);
             INSERT INTO items VALUES (1, 'apple', 1.5), (2, NULL, 2.0), (3, 'plum', NULL);",
        )
        .unwrap();
        path
    }

    fn approved_db(dir: &Path) -> (ApprovedSources, PathBuf) {
        let path = test_db(dir);
        let approved = ApprovedSources::default();
        approved.approve(&path).unwrap();
        (approved, path)
    }

    // ----- registry -------------------------------------------------------

    #[test]
    fn unapproved_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = test_db(dir.path());
        let approved = ApprovedSources::default();
        assert!(connect(&approved, Some(&path), &[]).is_err());
        approved.approve(&path).unwrap();
        assert!(connect(&approved, Some(&path), &[]).is_ok());
    }

    #[test]
    fn revoked_paths_are_refused_again() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let canonical = approved.check(&path).unwrap();
        approved.revoke(&canonical);
        assert!(connect(&approved, Some(&path), &[]).is_err());
    }

    // ----- authorizer denial matrix ----------------------------------------

    #[test]
    fn authorizer_denies_everything_but_reads() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let conn = connect(&approved, Some(&path), &[]).unwrap();

        let other = dir.path().join("other.sqlite");
        Connection::open(&other).unwrap(); // exists, so only auth can fail it
        let denied = [
            format!(
                "ATTACH DATABASE '{}' AS other",
                other.display().to_string().replace('\\', "/")
            ),
            "INSERT INTO items VALUES (9, 'x', 9.9)".into(),
            "UPDATE items SET name = 'x'".into(),
            "DELETE FROM items".into(),
            "DROP TABLE items".into(),
            "CREATE TABLE t2(a)".into(),
            "CREATE INDEX i2 ON items(name)".into(),
            "CREATE VIEW v2 AS SELECT 1".into(),
            "CREATE TRIGGER tr AFTER INSERT ON items BEGIN SELECT 1; END".into(),
            "ALTER TABLE items ADD COLUMN extra TEXT".into(),
            "PRAGMA journal_mode = WAL".into(),
            "PRAGMA writable_schema = 1".into(),
            "PRAGMA user_version = 7".into(),
            "ANALYZE".into(),
            "REINDEX idx_items_name".into(),
            "VACUUM".into(),
            "SELECT load_extension('evil')".into(),
        ];
        for sql in &denied {
            // Most statements are denied at prepare time by the authorizer;
            // a few (VACUUM) only fail at execution against the read-only
            // handle. Either point of failure is a denial.
            let result = match conn.prepare(sql) {
                Ok(_) => conn.execute_batch(sql),
                Err(e) => Err(e),
            };
            assert!(result.is_err(), "must be denied: {sql}");
        }

        // SELECT-class statements still work…
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        // …including read-only metadata pragmas in both spellings.
        let dv: i64 = conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert!(dv >= 0);
        let cols: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('items')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cols, 3);
        conn.query_row("PRAGMA table_info(items)", [], |r| r.get::<_, String>(1))
            .unwrap();
    }

    #[test]
    fn read_only_flag_backs_up_the_authorizer() {
        // Even if a write slipped past the authorizer, the OS-level
        // read-only open refuses it. Verified via a fresh unguarded
        // read-only handle: the flag alone rejects writes.
        let dir = tempfile::tempdir().unwrap();
        let path = test_db(dir.path());
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        assert!(conn.execute("DELETE FROM items", []).is_err());
    }

    // ----- limits + cancellation -------------------------------------------

    #[test]
    fn run_select_caps_rows_and_reports_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let conn = connect(&approved, Some(&path), &[]).unwrap();
        let limits = QueryLimits {
            max_rows: 2,
            max_bytes: u64::MAX,
        };
        let out = run_select(&conn, "SELECT * FROM items ORDER BY id", &[], limits, None).unwrap();
        assert_eq!(out.rows.len(), 2);
        assert!(out.truncated);
        assert_eq!(out.columns, vec!["id", "name", "price"]);

        let all = run_select(
            &conn,
            "SELECT * FROM items ORDER BY id",
            &[],
            QueryLimits::default(),
            None,
        )
        .unwrap();
        assert_eq!(all.rows.len(), 3);
        assert!(!all.truncated);
        // NULL stays distinct from text; numbers round-trip as text.
        assert_eq!(all.rows[1][1], None);
        assert_eq!(all.rows[0][2].as_deref(), Some("1.5"));
        assert_eq!(all.rows[2][2], None);
    }

    #[test]
    fn run_select_caps_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let conn = connect(&approved, Some(&path), &[]).unwrap();
        let limits = QueryLimits {
            max_rows: usize::MAX,
            max_bytes: 1,
        };
        let out = run_select(&conn, "SELECT * FROM items", &[], limits, None).unwrap();
        assert!(out.truncated);
        assert_eq!(out.rows.len(), 1, "stops right after the budget crosses");
    }

    #[test]
    fn cancelled_token_aborts_between_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let conn = connect(&approved, Some(&path), &[]).unwrap();
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        let result = run_select(
            &conn,
            "SELECT * FROM items",
            &[],
            QueryLimits::default(),
            Some(&ctx.cancel_token()),
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
    }

    #[test]
    fn progress_handler_interrupts_long_statements() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let conn = connect(&approved, Some(&path), &[]).unwrap();
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        install_cancel_handler(&conn, ctx.cancel_token()).unwrap();
        // A cross join big enough that the VM must poll the handler.
        let result: Result<i64, _> = conn
            .query_row(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c LIMIT 1000000)
                 SELECT COUNT(*) FROM c",
                [],
                |r| r.get(0),
            )
            .map_err(AppError::from);
        assert!(matches!(result, Err(AppError::Cancelled)));

        // Clearing the handler makes the connection usable again.
        clear_cancel_handler(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }

    // ----- the document vtab -----------------------------------------------

    #[test]
    fn document_vtab_serves_rows_and_stays_read_only() {
        let approved = ApprovedSources::default();
        let d = doc("name,qty\nAda,3\nBob,7\n");
        let conn = connect(&approved, None, &[("mydoc".into(), Arc::clone(&d))]).unwrap();
        let total: i64 = conn
            .query_row("SELECT SUM(CAST(qty AS INTEGER)) FROM mydoc", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(total, 10);
        let first: String = conn
            .query_row("SELECT name FROM mydoc ORDER BY name LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(first, "Ada");
        // Writes to the vtab are impossible: read-only module + authorizer.
        assert!(conn
            .execute("INSERT INTO mydoc VALUES ('x', '1')", [])
            .is_err());
        assert!(conn.execute("DELETE FROM mydoc", []).is_err());
        // Unexposed names do not exist.
        assert!(conn.prepare("SELECT * FROM otherdoc").is_err());
    }

    #[test]
    fn document_vtab_disambiguates_duplicate_headers() {
        let approved = ApprovedSources::default();
        let d = doc("a,a\n1,2\n");
        let conn = connect(&approved, None, &[("dup".into(), d)]).unwrap();
        let (x, y): (String, String) = conn
            .query_row("SELECT \"a\", \"a (2)\" FROM dup", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((x.as_str(), y.as_str()), ("1", "2"));
    }

    #[test]
    fn snapshot_strategy_is_immune_to_concurrent_edits() {
        let approved = ApprovedSources::default();
        let d = doc("v\nold\n");
        let conn = connect(&approved, None, &[("snap".into(), Arc::clone(&d))]).unwrap();
        // Edit AFTER connect: the snapshot must still serve the old value.
        d.write().unwrap().set_cell(0, 0, "new".into()).unwrap();
        let v: String = conn
            .query_row("SELECT v FROM snap", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "old", "snapshot pins the connect-time revision");
    }

    #[test]
    fn large_cells_force_the_live_strategy_despite_a_low_cell_count() {
        // Few rows, huge cells: under the cell-count gate but over the byte
        // budget. A snapshot would copy hundreds of MB and be immune to
        // edits; the byte-bounded fallback must pick the live strategy, so a
        // mid-query edit aborts on the pinned-revision check.
        let approved = ApprovedSources::default();
        let d = wide_cell_doc();
        assert!(
            d.read().unwrap().n_rows() < SNAPSHOT_MAX_CELLS,
            "the doc must stay under the cell-count gate"
        );
        let conn = connect(&approved, None, &[("wide".into(), Arc::clone(&d))]).unwrap();
        let mut stmt = conn.prepare("SELECT v FROM wide").unwrap();
        let mut rows = stmt.query([]).unwrap();
        for _ in 0..LIVE_WINDOW {
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
        assert!(
            failed,
            "a large-cell document must use the live strategy, not a snapshot"
        );
    }

    #[test]
    fn live_strategy_aborts_when_the_document_changes_mid_query() {
        let approved = ApprovedSources::default();
        let d = big_doc();
        let conn = connect(&approved, None, &[("live".into(), Arc::clone(&d))]).unwrap();

        // Sanity: an undisturbed full scan works and sees every row.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM live", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n as usize, SNAPSHOT_MAX_CELLS + 10);

        // Now edit mid-iteration: step past the first window, edit, resume.
        let mut stmt = conn.prepare("SELECT n FROM live").unwrap();
        let mut rows = stmt.query([]).unwrap();
        for _ in 0..LIVE_WINDOW {
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
                    let msg = e.to_string();
                    assert!(
                        msg.contains("changed while the query was running"),
                        "unexpected error: {msg}"
                    );
                    break;
                }
            }
        }
        assert!(failed, "a mid-query edit must abort the live scan");
    }

    #[test]
    fn file_tables_and_document_vtabs_compose_on_one_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (approved, path) = approved_db(dir.path());
        let d = doc("id,label\n1,one\n3,three\n");
        let conn = connect(&approved, Some(&path), &[("labels".into(), d)]).unwrap();
        // Join a real database table against an exposed document.
        let joined: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items i JOIN labels l ON CAST(l.id AS INTEGER) = i.id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(joined, 2);
    }

    #[test]
    fn value_narrowing_keeps_null_and_flags_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.sqlite");
        let setup = Connection::open(&path).unwrap();
        setup
            .execute_batch("CREATE TABLE b(x); INSERT INTO b VALUES (NULL), (x'0102'), (7);")
            .unwrap();
        drop(setup);
        let approved = ApprovedSources::default();
        approved.approve(&path).unwrap();
        let conn = connect(&approved, Some(&path), &[]).unwrap();
        let out = run_select(
            &conn,
            "SELECT x FROM b ORDER BY rowid",
            &[],
            QueryLimits::default(),
            None,
        )
        .unwrap();
        assert_eq!(out.rows[0][0], None);
        assert_eq!(out.rows[1][0].as_deref(), Some("[BLOB 2 bytes]"));
        assert_eq!(out.rows[2][0].as_deref(), Some("7"));
    }
}
