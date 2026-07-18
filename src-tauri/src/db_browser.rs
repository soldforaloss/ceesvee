//! F35 local database browser.
//!
//! **SQLite databases ONLY this cycle.** DuckDB is deliberately OUT OF
//! SCOPE: its bundled libduckdb C++ build cannot be compiled on the 6 GB
//! MinGW dev machine, so SQLite is the only database source registered here
//! (see [`db_open`], the single entry point where a database file becomes an
//! approved source). Revisit when a slim DuckDB binding or a prebuilt-binary
//! story exists.
//!
//! What lives here:
//! * **Schema browser** — tables + views with columns (declared type /
//!   notnull / default / pk), indexes, foreign keys, WITHOUT ROWID flag,
//!   row-count estimates and bounded preview rows.
//! * **Indexed read-only opens** — a table or view becomes a read-only
//!   document backed by [`DbTableBacking`], which pages windows straight out
//!   of the database (rowid/keyset anchors, never a full copy) through the
//!   same virtual-backing seam the F10 index uses.
//! * **Editable imports** — a bounded copy into an ordinary editable
//!   document, guarded by a memory estimate, with declared SQL types mapped
//!   onto F31 column schemas.
//! * **Refresh detection** — `PRAGMA data_version` (rows) plus a schema hash
//!   (structure) captured at open and compared on demand, both for browser
//!   sessions and for open table documents.
//!
//! Every connection comes from [`crate::safe_query`]: read-only, authorizer
//! guarded, cancellable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::types::Value;
use rusqlite::vtab::escape_double_quote;
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{Manager, State};

use crate::document::{Document, VirtualRows};
use crate::dto::IndexedOpenStart;
use crate::error::{AppError, AppResult};
use crate::job::{JobCtx, JobRegistry};
use crate::parse::{ImportInfo, ParsedFile};
use crate::safe_query::{self, ApprovedSources, QueryLimits};
use crate::schema::{ColumnSchema, LogicalType};
use crate::state::AppState;

/// Preview rows returned by default / at most.
const PREVIEW_ROWS_DEFAULT: usize = 100;
const PREVIEW_ROWS_MAX: usize = 1000;
/// Byte budget for one preview window.
const PREVIEW_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Row-count scans stop here: counts above it are reported as estimates.
const ROW_COUNT_CAP: u64 = 1_000_000;
/// One paging anchor is kept every this many rows, so a window read scans at
/// most `ANCHOR_EVERY + window` rows regardless of table size (~2 MB of
/// anchors for a 1-billion-row rowid table).
const ANCHOR_EVERY: usize = 4096;
/// Rows sampled to extrapolate the in-memory cost of an editable import.
const IMPORT_SAMPLE_ROWS: usize = 1000;
/// Per-cell / per-row bookkeeping overhead for the import estimate (matches
/// `index::estimate`).
const CELL_OVERHEAD: u64 = 40;
const ROW_OVERHEAD: u64 = 32;
/// Cancellation check cadence inside scans.
const CANCEL_EVERY: usize = 1024;

// ---------------------------------------------------------------------------
// Wire DTOs
// ---------------------------------------------------------------------------

/// A registered browser session: the id to address it plus its schema.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbSessionInfo {
    pub session_id: u64,
    pub path: String,
    pub schema: DbSchemaInfo,
}

/// Everything the schema browser shows for one database.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbSchemaInfo {
    pub objects: Vec<DbObjectInfo>,
    /// `PRAGMA data_version` at capture time (per-connection baseline).
    pub data_version: i64,
    /// SHA-256 over the full `sqlite_master` definition set.
    pub schema_hash: String,
}

/// One table or view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbObjectInfo {
    pub name: String,
    /// "table" or "view".
    pub kind: String,
    pub without_rowid: bool,
    pub columns: Vec<DbColumnInfo>,
    /// Primary-key column names in key order (empty for views).
    pub primary_key: Vec<String>,
    pub indexes: Vec<DbIndexInfo>,
    pub foreign_keys: Vec<DbForeignKey>,
    pub row_estimate: u64,
    /// Whether `row_estimate` is an exact count (small objects) or an
    /// estimate (sqlite_stat1 / capped scan).
    pub row_estimate_exact: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbColumnInfo {
    pub name: String,
    /// Declared SQL type, verbatim (may be empty).
    pub decl_type: String,
    pub notnull: bool,
    pub default_value: Option<String>,
    /// 1-based position within the primary key, when part of it.
    pub pk_position: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbIndexInfo {
    pub name: String,
    pub unique: bool,
    /// "c" (CREATE INDEX), "u" (UNIQUE constraint) or "pk".
    pub origin: String,
    pub partial: bool,
    /// Indexed column names; `None` for rowid/expression members.
    pub columns: Vec<Option<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbForeignKey {
    /// Referenced table.
    pub table: String,
    /// Local column names.
    pub columns: Vec<String>,
    /// Referenced column names (`None` = implicit primary key).
    pub ref_columns: Vec<Option<String>>,
    pub on_update: String,
    pub on_delete: String,
}

/// A bounded preview window; cells keep SQL `NULL` distinct.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbPreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
}

/// What changed outside CEESVEE since the baseline was captured.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbRefreshStatus {
    pub rows_changed: bool,
    pub schema_changed: bool,
}

// ---------------------------------------------------------------------------
// Session registry
// ---------------------------------------------------------------------------

/// Open browser sessions, keyed by id. Each session pins ONE long-lived
/// guarded connection: `PRAGMA data_version` only reports changes made by
/// OTHER connections, so the baseline is only meaningful while the observing
/// connection stays open.
#[derive(Default)]
pub struct DbBrowserCache {
    next: AtomicU64,
    sessions: Mutex<HashMap<u64, Arc<DbSession>>>,
}

pub struct DbSession {
    /// Canonical, approved path of the database file.
    path: PathBuf,
    conn: Mutex<Connection>,
    baseline: Mutex<Baseline>,
}

struct Baseline {
    data_version: i64,
    schema_hash: String,
}

impl DbBrowserCache {
    fn register(&self, session: DbSession) -> AppResult<u64> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.sessions
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?
            .insert(id, Arc::new(session));
        Ok(id)
    }

    fn get(&self, id: u64) -> AppResult<Arc<DbSession>> {
        self.sessions
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::invalid(format!("no open database session {id}")))
    }

    fn remove(&self, id: u64) -> bool {
        self.sessions
            .lock()
            .map(|mut s| s.remove(&id).is_some())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Schema reading
// ---------------------------------------------------------------------------

pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", escape_double_quote(name))
}

fn lock_conn(session: &DbSession) -> AppResult<std::sync::MutexGuard<'_, Connection>> {
    session
        .conn
        .lock()
        .map_err(|_| AppError::Other("internal database lock error".into()))
}

fn data_version(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("PRAGMA data_version", [], |r| r.get(0))?)
}

/// SHA-256 over every object definition in `sqlite_master`, so ANY schema
/// change — new/dropped objects, ALTERed columns, index changes — moves it.
fn schema_hash(conn: &Connection) -> AppResult<String> {
    let mut hasher = Sha256::new();
    let mut stmt = conn.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master
         ORDER BY type, name",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        for i in 0..4 {
            hasher.update(row.get::<_, String>(i)?.as_bytes());
            hasher.update([0u8]);
        }
        hasher.update([0xFFu8]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Hash of ONE object's definition + resolved columns, for the per-document
/// refresh probe (a change to an unrelated table must not flag it).
fn object_schema_hash(conn: &Connection, name: &str) -> AppResult<String> {
    let mut hasher = Sha256::new();
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(ignore_no_rows)?
        .flatten();
    hasher.update(sql.unwrap_or_default().as_bytes());
    let mut stmt = conn.prepare(
        "SELECT name, type, \"notnull\", COALESCE(dflt_value, ''), pk
         FROM pragma_table_info(?1) ORDER BY cid",
    )?;
    let mut rows = stmt.query([name])?;
    while let Some(row) = rows.next()? {
        hasher.update(row.get::<_, String>(0)?.as_bytes());
        hasher.update([0u8]);
        hasher.update(row.get::<_, String>(1)?.as_bytes());
        hasher.update([0u8]);
        hasher.update(row.get::<_, i64>(2)?.to_le_bytes());
        hasher.update(row.get::<_, String>(3)?.as_bytes());
        hasher.update(row.get::<_, i64>(4)?.to_le_bytes());
        hasher.update([0xFFu8]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn ignore_no_rows<T>(e: rusqlite::Error) -> Result<Option<T>, rusqlite::Error> {
    match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    }
}

/// Whether `name` is a WITHOUT ROWID table (`pragma_table_list.wr`).
pub(crate) fn is_without_rowid(conn: &Connection, name: &str) -> AppResult<bool> {
    Ok(conn
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .map(Some)
        .or_else(ignore_no_rows)?
        .map(|wr| wr != 0)
        .unwrap_or(false))
}

/// Look `name` up in `sqlite_master`; errors when it is not a table/view.
/// Every command resolves user-supplied names through this before quoting
/// them into SQL, so arbitrary identifiers never reach a statement.
fn resolve_object(conn: &Connection, name: &str) -> AppResult<(String, String)> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT name, type FROM sqlite_master
             WHERE type IN ('table', 'view') AND name = ?1 AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(ignore_no_rows)?;
    found.ok_or_else(|| AppError::invalid(format!("no table or view named '{name}'")))
}

fn read_schema_info(conn: &Connection) -> AppResult<DbSchemaInfo> {
    let mut objects = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT name, type FROM sqlite_master
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
         ORDER BY type, name",
    )?;
    let names: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (name, kind) in names {
        objects.push(read_object_info(conn, &name, &kind)?);
    }
    Ok(DbSchemaInfo {
        objects,
        data_version: data_version(conn)?,
        schema_hash: schema_hash(conn)?,
    })
}

fn read_object_info(conn: &Connection, name: &str, kind: &str) -> AppResult<DbObjectInfo> {
    // Columns + primary key.
    let mut columns = Vec::new();
    let mut pk: Vec<(i64, String)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk
         FROM pragma_table_info(?1) ORDER BY cid",
    )?;
    let mut rows = stmt.query([name])?;
    while let Some(row) = rows.next()? {
        let col_name: String = row.get(0)?;
        let pk_pos: i64 = row.get(4)?;
        if pk_pos > 0 {
            pk.push((pk_pos, col_name.clone()));
        }
        columns.push(DbColumnInfo {
            name: col_name,
            decl_type: row.get(1)?,
            notnull: row.get::<_, i64>(2)? != 0,
            default_value: row.get(3)?,
            pk_position: (pk_pos > 0).then_some(pk_pos as u32),
        });
    }
    drop(rows);
    drop(stmt);
    pk.sort();
    let primary_key: Vec<String> = pk.into_iter().map(|(_, n)| n).collect();

    // WITHOUT ROWID flag (tables only; `pragma_table_list.wr`).
    let without_rowid = kind == "table" && is_without_rowid(conn, name)?;

    // Indexes.
    let mut indexes = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1) ORDER BY seq",
    )?;
    let mut rows = stmt.query([name])?;
    let mut index_heads: Vec<(String, bool, String, bool)> = Vec::new();
    while let Some(row) = rows.next()? {
        index_heads.push((
            row.get(0)?,
            row.get::<_, i64>(1)? != 0,
            row.get(2)?,
            row.get::<_, i64>(3)? != 0,
        ));
    }
    drop(rows);
    drop(stmt);
    for (idx_name, unique, origin, partial) in index_heads {
        let mut stmt = conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
        let cols: Vec<Option<String>> = stmt
            .query_map([&idx_name], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        indexes.push(DbIndexInfo {
            name: idx_name,
            unique,
            origin,
            partial,
            columns: cols,
        });
    }

    // Foreign keys, grouped by constraint id.
    let mut foreign_keys: Vec<DbForeignKey> = Vec::new();
    let mut last_id: Option<i64> = None;
    let mut stmt = conn.prepare(
        "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete
         FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
    )?;
    let mut rows = stmt.query([name])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        if last_id != Some(id) {
            foreign_keys.push(DbForeignKey {
                table: row.get(1)?,
                columns: Vec::new(),
                ref_columns: Vec::new(),
                on_update: row.get(4)?,
                on_delete: row.get(5)?,
            });
            last_id = Some(id);
        }
        let fk = foreign_keys.last_mut().expect("pushed above");
        fk.columns.push(row.get(2)?);
        fk.ref_columns.push(row.get(3)?);
    }
    drop(rows);
    drop(stmt);

    let (row_estimate, row_estimate_exact) = row_estimate(conn, name, kind)?;
    Ok(DbObjectInfo {
        name: name.to_string(),
        kind: kind.to_string(),
        without_rowid,
        columns,
        primary_key,
        indexes,
        foreign_keys,
        row_estimate,
        row_estimate_exact,
    })
}

/// Row-count estimate with bounded work: `sqlite_stat1` when ANALYZE ran,
/// otherwise a scan capped at [`ROW_COUNT_CAP`] rows.
pub(crate) fn row_estimate(conn: &Connection, name: &str, kind: &str) -> AppResult<(u64, bool)> {
    if kind == "table" {
        // sqlite_stat1 only exists after ANALYZE; any failure (missing
        // table, NULL stat) quietly falls through to the capped scan.
        let stat: Result<String, _> = conn.query_row(
            "SELECT stat FROM sqlite_stat1 WHERE tbl = ?1 LIMIT 1",
            [name],
            |r| r.get(0),
        );
        if let Ok(stat) = stat {
            if let Some(n) = stat
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
            {
                return Ok((n, false));
            }
        }
    }
    let capped: u64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM (SELECT 1 FROM {} LIMIT {})",
            quote_ident(name),
            ROW_COUNT_CAP + 1
        ),
        [],
        |r| r.get::<_, i64>(0),
    )? as u64;
    if capped > ROW_COUNT_CAP {
        Ok((ROW_COUNT_CAP, false))
    } else {
        Ok((capped, true))
    }
}

// ---------------------------------------------------------------------------
// The table backing (indexed read-only opens)
// ---------------------------------------------------------------------------

/// How windows are addressed within the object.
enum Paging {
    /// Ordinary rowid table: anchors are rowids every [`ANCHOR_EVERY`] rows.
    /// Handles arbitrary rowid gaps — anchors store ACTUAL rowids and reads
    /// seek `rowid >= anchor`, never `rowid = offset`.
    Rowid { anchors: Vec<i64> },
    /// WITHOUT ROWID table: keyset paging over the primary-key columns
    /// (row-value comparison), anchors store the key values.
    Keyset {
        key_cols: Vec<String>,
        anchors: Vec<Vec<Value>>,
    },
    /// Views have no key to seek on: plain LIMIT/OFFSET paging. O(offset)
    /// per window — acceptable for views, which the browser treats as
    /// derived/preview surfaces; tables never use this.
    Offset,
}

/// [`VirtualRows`] provider over one table/view of an approved SQLite file.
/// Holds its own guarded connection open for the document's lifetime (the
/// `data_version` baseline is per-connection). Memory: column names + one
/// anchor per [`ANCHOR_EVERY`] rows — the table itself is never copied.
pub struct DbTableBacking {
    conn: Mutex<Connection>,
    /// Unquoted object name (pragma lookups) and pre-quoted FROM target.
    object: String,
    from: String,
    n_cols: usize,
    n_rows: usize,
    paging: Paging,
    baseline_data_version: i64,
    baseline_object_hash: String,
}

impl DbTableBacking {
    #[cfg(test)]
    fn anchor_count(&self) -> usize {
        match &self.paging {
            Paging::Rowid { anchors } => anchors.len(),
            Paging::Keyset { anchors, .. } => anchors.len(),
            Paging::Offset => 0,
        }
    }

    fn guard_unchanged(&self, conn: &Connection) -> AppResult<()> {
        if data_version(conn)? != self.baseline_data_version {
            return Err(AppError::Other(
                "the database changed on disk; refresh the document".into(),
            ));
        }
        Ok(())
    }
}

impl VirtualRows for DbTableBacking {
    fn n_rows(&self) -> usize {
        self.n_rows
    }

    fn read_rows(&self, start: usize, end: usize) -> AppResult<Vec<Vec<String>>> {
        let end = end.min(self.n_rows);
        if start >= end {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| AppError::Other("internal database lock error".into()))?;
        // Any commit by another connection moves data_version: stale reads
        // fail loudly instead of slicing shifted windows.
        self.guard_unchanged(&conn)?;

        let limit = (end - start) as i64;
        let (sql, params): (String, Vec<Value>) = match &self.paging {
            Paging::Rowid { anchors } => {
                let ai = start / ANCHOR_EVERY;
                let gap = (start - ai * ANCHOR_EVERY) as i64;
                (
                    format!(
                        "SELECT * FROM {} WHERE rowid >= ?1 ORDER BY rowid LIMIT ?2 OFFSET ?3",
                        self.from
                    ),
                    vec![
                        Value::Integer(anchors[ai]),
                        Value::Integer(limit),
                        Value::Integer(gap),
                    ],
                )
            }
            Paging::Keyset { key_cols, anchors } => {
                let ai = start / ANCHOR_EVERY;
                let gap = (start - ai * ANCHOR_EVERY) as i64;
                let keys = key_cols
                    .iter()
                    .map(|k| quote_ident(k))
                    .collect::<Vec<_>>()
                    .join(", ");
                let marks = (1..=key_cols.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut params = anchors[ai].clone();
                params.push(Value::Integer(limit));
                params.push(Value::Integer(gap));
                (
                    format!(
                        "SELECT * FROM {} WHERE ({keys}) >= ({marks}) ORDER BY {keys} \
                         LIMIT ?{} OFFSET ?{}",
                        self.from,
                        key_cols.len() + 1,
                        key_cols.len() + 2,
                    ),
                    params,
                )
            }
            Paging::Offset => (
                format!("SELECT * FROM {} LIMIT ?1 OFFSET ?2", self.from),
                vec![Value::Integer(limit), Value::Integer(start as i64)],
            ),
        };

        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        let mut out: Vec<Vec<String>> = Vec::with_capacity(end - start);
        while let Some(row) = rows.next()? {
            let mut cells = Vec::with_capacity(self.n_cols);
            for i in 0..self.n_cols {
                // SQL NULL narrows to "" — the document contract has no
                // missing slot (documented on Document::from_virtual).
                cells.push(safe_query::value_to_text(row.get_ref(i)?).unwrap_or_default());
            }
            out.push(cells);
        }
        if out.len() != end - start {
            return Err(AppError::Other(
                "the database changed on disk; refresh the document".into(),
            ));
        }
        Ok(out)
    }

    fn refresh_probe(&self) -> AppResult<Option<(bool, bool)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| AppError::Other("internal database lock error".into()))?;
        let rows_changed = data_version(&conn)? != self.baseline_data_version;
        let schema_changed = object_schema_hash(&conn, &self.object)? != self.baseline_object_hash;
        Ok(Some((rows_changed, schema_changed)))
    }
}

/// Open `object` in `canonical` (an approved database file) as a
/// [`DbTableBacking`]: one ordered key scan collects the row count and the
/// paging anchors — 8 bytes per [`ANCHOR_EVERY`] rows, never the rows
/// themselves. Returns the backing plus the column headers.
pub(crate) fn build_table_backing(
    canonical: &Path,
    object: &str,
    ctx: Option<&JobCtx>,
) -> AppResult<(Vec<String>, DbTableBacking)> {
    let conn = safe_query::open_guarded(canonical)?;
    if let Some(ctx) = ctx {
        // Long statements abort through the progress handler; the explicit
        // checks between phases catch cancellation on tiny tables too.
        safe_query::install_cancel_handler(&conn, ctx.cancel_token())?;
        ctx.check()?;
    }
    let (name, kind) = resolve_object(&conn, object)?;
    let from = quote_ident(&name);

    // Baselines FIRST: a change committed during the anchor scan must
    // invalidate the first read, not slip under it.
    let baseline_data_version = data_version(&conn)?;
    let baseline_object_hash = object_schema_hash(&conn, &name)?;

    let mut headers: Vec<String> = Vec::new();
    let mut pk: Vec<(i64, String)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT name, pk FROM pragma_table_info(?1) ORDER BY cid")?;
        let mut rows = stmt.query([&name])?;
        while let Some(row) = rows.next()? {
            let col: String = row.get(0)?;
            let pk_pos: i64 = row.get(1)?;
            if pk_pos > 0 {
                pk.push((pk_pos, col.clone()));
            }
            headers.push(col);
        }
    }
    if headers.is_empty() {
        return Err(AppError::invalid(format!(
            "'{name}' has no columns to display"
        )));
    }
    pk.sort();

    let without_rowid = kind == "table" && is_without_rowid(&conn, &name)?;

    if let Some(ctx) = ctx {
        ctx.set_message(format!("Scanning {name}"));
    }

    // One ordered scan over the paging key: exact count + anchors.
    let (n_rows, paging) = if kind == "view" {
        let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {from}"), [], |r| r.get(0))?;
        (n.max(0) as usize, Paging::Offset)
    } else if without_rowid {
        let key_cols: Vec<String> = pk.iter().map(|(_, n)| n.clone()).collect();
        if key_cols.is_empty() {
            // A WITHOUT ROWID table always has a PK; defensive fallback.
            return Err(AppError::invalid(format!(
                "'{name}' has no primary key to page on"
            )));
        }
        let keys = key_cols
            .iter()
            .map(|k| quote_ident(k))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!("SELECT {keys} FROM {from} ORDER BY {keys}"))?;
        let mut rows = stmt.query([])?;
        let mut anchors: Vec<Vec<Value>> = Vec::new();
        let mut count = 0usize;
        while let Some(row) = rows.next()? {
            if count.is_multiple_of(ANCHOR_EVERY) {
                let mut key = Vec::with_capacity(key_cols.len());
                for i in 0..key_cols.len() {
                    key.push(row.get::<_, Value>(i)?);
                }
                anchors.push(key);
            }
            count += 1;
            if count.is_multiple_of(CANCEL_EVERY) {
                if let Some(ctx) = ctx {
                    ctx.advance(CANCEL_EVERY as u64)?;
                }
            }
        }
        (count, Paging::Keyset { key_cols, anchors })
    } else {
        let mut stmt = conn.prepare(&format!("SELECT rowid FROM {from} ORDER BY rowid"))?;
        let mut rows = stmt.query([])?;
        let mut anchors: Vec<i64> = Vec::new();
        let mut count = 0usize;
        while let Some(row) = rows.next()? {
            if count.is_multiple_of(ANCHOR_EVERY) {
                anchors.push(row.get(0)?);
            }
            count += 1;
            if count.is_multiple_of(CANCEL_EVERY) {
                if let Some(ctx) = ctx {
                    ctx.advance(CANCEL_EVERY as u64)?;
                }
            }
        }
        (count, Paging::Rowid { anchors })
    };

    if let Some(ctx) = ctx {
        ctx.check()?;
    }
    // The backing outlives the open job: detach the job's cancel poll so a
    // long-dead flag is never consulted on ordinary reads.
    safe_query::clear_cancel_handler(&conn)?;

    let n_cols = headers.len();
    Ok((
        headers,
        DbTableBacking {
            conn: Mutex::new(conn),
            object: name,
            from,
            n_cols,
            n_rows,
            paging,
            baseline_data_version,
            baseline_object_hash,
        },
    ))
}

// ---------------------------------------------------------------------------
// Editable import
// ---------------------------------------------------------------------------

/// Map a declared SQL column type onto an F31 logical type. Deliberately
/// conservative: only types whose text form CEESVEE parses natively get a
/// non-text schema; SQLite's free-form date/time text stays plain text.
pub(crate) fn logical_type_of_decl(decl: &str) -> LogicalType {
    let upper = decl.to_ascii_uppercase();
    if upper.contains("INT") {
        LogicalType::Integer
    } else if upper.contains("BOOL") {
        LogicalType::Boolean
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        LogicalType::Float
    } else if upper.contains("DEC") || upper.contains("NUMERIC") {
        LogicalType::Decimal
    } else {
        LogicalType::Text
    }
}

/// Copy `object` into a fully editable in-memory document, refusing when the
/// extrapolated memory cost exceeds `memory_threshold` (pass
/// [`crate::index::MEMORY_DECISION_THRESHOLD`]; `force` overrides). Runs one
/// streaming scan; NULL narrows to "" and declared types become F31 schemas.
pub(crate) fn import_table(
    canonical: &Path,
    object: &str,
    doc_id: u64,
    memory_threshold: u64,
    force: bool,
    ctx: Option<&JobCtx>,
) -> AppResult<Document> {
    let conn = safe_query::open_guarded(canonical)?;
    if let Some(ctx) = ctx {
        // Long statements abort through the progress handler; the explicit
        // checks between phases catch cancellation on tiny tables too.
        safe_query::install_cancel_handler(&conn, ctx.cancel_token())?;
        ctx.check()?;
    }
    let (name, kind) = resolve_object(&conn, object)?;
    let from = quote_ident(&name);

    // Columns + declared types.
    let mut headers: Vec<String> = Vec::new();
    let mut decls: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT name, type FROM pragma_table_info(?1) ORDER BY cid")?;
        let mut rows = stmt.query([&name])?;
        while let Some(row) = rows.next()? {
            headers.push(row.get(0)?);
            decls.push(row.get(1)?);
        }
    }
    if headers.is_empty() {
        return Err(AppError::invalid(format!(
            "'{name}' has no columns to import"
        )));
    }
    let n_cols = headers.len();

    // Memory gate: exact row count (cancellable) + sampled average row cost.
    let n_rows: u64 = conn.query_row(&format!("SELECT COUNT(*) FROM {from}"), [], |r| {
        r.get::<_, i64>(0)
    })? as u64;
    let mut sample_bytes = 0u64;
    let mut sampled = 0u64;
    {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {from} LIMIT {IMPORT_SAMPLE_ROWS}"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            for i in 0..n_cols {
                sample_bytes += safe_query::value_to_text(row.get_ref(i)?)
                    .map_or(0, |s| s.len() as u64)
                    + CELL_OVERHEAD;
            }
            sample_bytes += ROW_OVERHEAD;
            sampled += 1;
        }
    }
    let avg_row = sample_bytes
        .checked_div(sampled)
        .unwrap_or(ROW_OVERHEAD + n_cols as u64 * CELL_OVERHEAD);
    let estimated = n_rows.saturating_mul(avg_row.max(1));
    if !force && estimated > memory_threshold {
        return Err(AppError::invalid(format!(
            "importing '{name}' needs an estimated {} MB of memory, which may exhaust memory — \
             open it read-only instead, or import anyway",
            estimated / (1024 * 1024)
        )));
    }

    if let Some(ctx) = ctx {
        ctx.set_total(n_rows);
        ctx.set_message(format!("Importing {name}"));
    }

    // Stream every row. Ordinary tables order by rowid for a deterministic
    // result; WITHOUT ROWID tables iterate in primary-key order naturally,
    // and views have no rowid to order on.
    let sql = if kind == "table" && !is_without_rowid(&conn, &name)? {
        format!("SELECT * FROM {from} ORDER BY rowid")
    } else {
        format!("SELECT * FROM {from}")
    };
    let mut records: Vec<Vec<String>> = Vec::with_capacity(n_rows as usize + 1);
    records.push(headers.clone());
    {
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            let mut cells = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                cells.push(safe_query::value_to_text(row.get_ref(i)?).unwrap_or_default());
            }
            records.push(cells);
            count += 1;
            if count.is_multiple_of(CANCEL_EVERY as u64) {
                if let Some(ctx) = ctx {
                    ctx.advance(CANCEL_EVERY as u64)?;
                }
            }
        }
    }
    if let Some(ctx) = ctx {
        ctx.check()?;
    }

    let parsed = ParsedFile {
        records,
        n_cols,
        delimiter: b',',
        encoding: encoding_rs::UTF_8,
        had_bom: false,
        uses_crlf: cfg!(windows),
        import: ImportInfo::default(),
    };
    let mut doc = Document::from_parsed(doc_id, None, parsed, true);
    // Declared SQL types become F31 column schemas so a later export back to
    // a database (or CSV with schema) keeps the typing.
    let ids = doc.column_ids().to_vec();
    for (i, decl) in decls.iter().enumerate() {
        let lt = logical_type_of_decl(decl);
        if lt != LogicalType::Text {
            doc.set_column_schema(ColumnSchema::new(ids[i].clone(), headers[i].clone(), lt));
        }
    }
    doc.mark_derived_unsaved();
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn display_label(path: &Path, object: &str) -> String {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    format!("{file} → {object}")
}

pub(crate) fn blocking_err(e: tauri::Error) -> AppError {
    AppError::Other(format!("background task failed: {e}"))
}

/// Open a SQLite database file for browsing. Picking the file IS the user's
/// approval: the path enters the SafeQueryEngine's approved-source registry
/// here, and nowhere else. (SQLite only — see the module docs for why DuckDB
/// is out of scope this cycle.)
#[tauri::command]
pub async fn db_open(path: String, app: tauri::AppHandle) -> AppResult<DbSessionInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let approved = app.state::<ApprovedSources>();
        let canonical = approved.approve(Path::new(&path))?;
        let conn = safe_query::open_guarded(&canonical)?;
        // Reject files that are not SQLite databases up front with a clear
        // message (the first real query would fail anyway).
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| {
            AppError::invalid(format!(
                "'{path}' does not look like a SQLite database: {e}"
            ))
        })?;
        let schema = read_schema_info(&conn)?;
        let baseline = Baseline {
            data_version: schema.data_version,
            schema_hash: schema.schema_hash.clone(),
        };
        let cache = app.state::<DbBrowserCache>();
        let session_id = cache.register(DbSession {
            path: canonical.clone(),
            conn: Mutex::new(conn),
            baseline: Mutex::new(baseline),
        })?;
        Ok(DbSessionInfo {
            session_id,
            path: canonical.to_string_lossy().to_string(),
            schema,
        })
    })
    .await
    .map_err(blocking_err)?
}

/// Close a browser session (drops its pinned connection).
#[tauri::command]
pub fn db_close(session_id: u64, cache: State<'_, DbBrowserCache>) -> AppResult<()> {
    if !cache.remove(session_id) {
        return Err(AppError::invalid(format!(
            "no open database session {session_id}"
        )));
    }
    Ok(())
}

/// Re-read the full schema (after the user accepts a refresh prompt). Also
/// rebases the session's change-detection baseline.
#[tauri::command]
pub async fn db_schema(session_id: u64, app: tauri::AppHandle) -> AppResult<DbSchemaInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let session = app.state::<DbBrowserCache>().get(session_id)?;
        let conn = lock_conn(&session)?;
        let schema = read_schema_info(&conn)?;
        drop(conn);
        let mut baseline = session
            .baseline
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?;
        baseline.data_version = schema.data_version;
        baseline.schema_hash = schema.schema_hash.clone();
        Ok(schema)
    })
    .await
    .map_err(blocking_err)?
}

/// Bounded preview rows for one table or view.
#[tauri::command]
pub async fn db_preview(
    session_id: u64,
    object: String,
    limit: Option<usize>,
    app: tauri::AppHandle,
) -> AppResult<DbPreview> {
    tauri::async_runtime::spawn_blocking(move || {
        let session = app.state::<DbBrowserCache>().get(session_id)?;
        let conn = lock_conn(&session)?;
        let (name, _) = resolve_object(&conn, &object)?;
        let limit = limit.unwrap_or(PREVIEW_ROWS_DEFAULT).min(PREVIEW_ROWS_MAX);
        let out = safe_query::run_select(
            &conn,
            &format!("SELECT * FROM {} LIMIT {}", quote_ident(&name), limit + 1),
            &[],
            QueryLimits {
                max_rows: limit,
                max_bytes: PREVIEW_MAX_BYTES,
            },
            None,
        )?;
        Ok(DbPreview {
            columns: out.columns,
            rows: out.rows,
            truncated: out.truncated,
        })
    })
    .await
    .map_err(blocking_err)?
}

/// Cheap change probe for a browser session: did rows or schema change
/// outside CEESVEE since the baseline? The UI prompts to reload on `true`;
/// accepting calls [`db_schema`], which rebases.
#[tauri::command]
pub async fn db_refresh_probe(
    session_id: u64,
    app: tauri::AppHandle,
) -> AppResult<DbRefreshStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let session = app.state::<DbBrowserCache>().get(session_id)?;
        let conn = lock_conn(&session)?;
        let dv = data_version(&conn)?;
        let hash = schema_hash(&conn)?;
        drop(conn);
        let baseline = session
            .baseline
            .lock()
            .map_err(|_| AppError::Other("internal state lock error".into()))?;
        Ok(DbRefreshStatus {
            rows_changed: dv != baseline.data_version,
            schema_changed: hash != baseline.schema_hash,
        })
    })
    .await
    .map_err(blocking_err)?
}

/// Open a table/view as an indexed READ-ONLY document (background job:
/// progress = scanned rows, cancellable; cancelling drops the connection and
/// leaves the database file untouched and unlocked).
#[tauri::command]
pub async fn start_db_open_table(
    session_id: u64,
    object: String,
    app: tauri::AppHandle,
    state: State<'_, std::sync::Mutex<AppState>>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let session = app.state::<DbBrowserCache>().get(session_id)?;
    let doc_id = state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .alloc_id();
    let ctx = jobs.begin_for_app(&app, "dbOpenTable", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let (headers, backing) = build_table_backing(&session.path, &object, Some(ctx))?;
            let doc = Document::from_virtual(
                doc_id,
                display_label(&session.path, &object),
                headers,
                Box::new(backing),
            );
            let registry = app_for_job.state::<std::sync::Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// Import a table/view into a fully editable document (background job),
/// bounded by the same memory threshold as the F10 convert flow; pass
/// `force` to import anyway.
#[tauri::command]
pub async fn start_db_import_table(
    session_id: u64,
    object: String,
    force: bool,
    app: tauri::AppHandle,
    state: State<'_, std::sync::Mutex<AppState>>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let session = app.state::<DbBrowserCache>().get(session_id)?;
    let doc_id = state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .alloc_id();
    let ctx = jobs.begin_for_app(&app, "dbImportTable", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let mut doc = import_table(
                &session.path,
                &object,
                doc_id,
                crate::index::MEMORY_DECISION_THRESHOLD,
                force,
                Some(ctx),
            )?;
            doc.set_display_name(display_label(&session.path, &object));
            let registry = app_for_job.state::<std::sync::Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// Change probe for an open database-backed DOCUMENT (as opposed to a
/// browser session): compares the backing's pinned `data_version` and
/// object schema hash. The UI prompts to reload; reloading is a fresh
/// [`start_db_open_table`].
#[tauri::command]
pub fn db_doc_refresh_probe(
    doc_id: u64,
    state: State<'_, std::sync::Mutex<AppState>>,
) -> AppResult<DbRefreshStatus> {
    let handle = state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))?
        .doc(doc_id)?;
    let doc = handle
        .read()
        .map_err(|_| AppError::Other("internal document lock error".into()))?;
    match doc.virtual_refresh_probe()? {
        Some((rows_changed, schema_changed)) => Ok(DbRefreshStatus {
            rows_changed,
            schema_changed,
        }),
        None => Err(AppError::invalid(
            "this document is not backed by a database table",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;

    fn make_db(dir: &Path, sql: &str) -> PathBuf {
        let path = dir.join("test.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(sql).unwrap();
        path
    }

    fn rich_db(dir: &Path) -> PathBuf {
        make_db(
            dir,
            "CREATE TABLE customers(
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 tier TEXT DEFAULT 'basic',
                 balance REAL);
             CREATE INDEX idx_name ON customers(name);
             CREATE TABLE orders(
                 id INTEGER PRIMARY KEY,
                 customer_id INTEGER NOT NULL REFERENCES customers(id),
                 total NUMERIC,
                 flag BOOLEAN);
             CREATE TABLE pairs(a TEXT, b INTEGER, note TEXT,
                 PRIMARY KEY (a, b)) WITHOUT ROWID;
             CREATE VIEW big_orders AS SELECT * FROM orders WHERE total > 10;
             INSERT INTO customers VALUES
                 (1, 'Ada', 'gold', 10.5), (2, 'Bob', NULL, NULL);
             INSERT INTO orders VALUES (1, 1, 20, 1), (2, 2, 5, 0);
             INSERT INTO pairs VALUES ('x', 1, 'first'), ('x', 2, 'second'),
                 ('y', 1, 'third');",
        )
    }

    fn backing_for(path: &Path, object: &str) -> (Vec<String>, DbTableBacking) {
        build_table_backing(&std::fs::canonicalize(path).unwrap(), object, None).unwrap()
    }

    // ----- schema browser ---------------------------------------------------

    #[test]
    fn schema_info_lists_objects_columns_keys_and_estimates() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let conn = safe_query::open_guarded(&std::fs::canonicalize(&path).unwrap()).unwrap();
        let info = read_schema_info(&conn).unwrap();

        let names: Vec<&str> = info.objects.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["customers", "orders", "pairs", "big_orders"]);

        let customers = &info.objects[0];
        assert_eq!(customers.kind, "table");
        assert!(!customers.without_rowid);
        assert_eq!(customers.primary_key, vec!["id"]);
        assert_eq!(customers.columns.len(), 4);
        assert_eq!(customers.columns[1].name, "name");
        assert!(customers.columns[1].notnull);
        assert_eq!(
            customers.columns[2].default_value.as_deref(),
            Some("'basic'")
        );
        assert_eq!(customers.columns[0].pk_position, Some(1));
        assert!(customers
            .indexes
            .iter()
            .any(|i| i.name == "idx_name" && !i.unique));
        assert_eq!(customers.row_estimate, 2);
        assert!(customers.row_estimate_exact);

        let orders = &info.objects[1];
        assert_eq!(orders.foreign_keys.len(), 1);
        assert_eq!(orders.foreign_keys[0].table, "customers");
        assert_eq!(orders.foreign_keys[0].columns, vec!["customer_id"]);

        let pairs = &info.objects[2];
        assert!(pairs.without_rowid);
        assert_eq!(pairs.primary_key, vec!["a", "b"]);

        let view = &info.objects[3];
        assert_eq!(view.kind, "view");
        assert_eq!(view.row_estimate, 1);
        assert!(view.row_estimate_exact);

        assert!(!info.schema_hash.is_empty());
    }

    #[test]
    fn preview_is_bounded_and_keeps_null_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let conn = safe_query::open_guarded(&std::fs::canonicalize(&path).unwrap()).unwrap();
        let out = safe_query::run_select(
            &conn,
            "SELECT * FROM customers ORDER BY id LIMIT 2",
            &[],
            QueryLimits {
                max_rows: 1,
                max_bytes: PREVIEW_MAX_BYTES,
            },
            None,
        )
        .unwrap();
        assert_eq!(out.rows.len(), 1);
        assert!(out.truncated);

        let all = safe_query::run_select(
            &conn,
            "SELECT * FROM customers ORDER BY id",
            &[],
            QueryLimits::default(),
            None,
        )
        .unwrap();
        assert_eq!(all.rows[1][2], None, "NULL tier stays None in previews");
    }

    #[test]
    fn resolve_object_refuses_unknown_and_internal_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let conn = safe_query::open_guarded(&std::fs::canonicalize(&path).unwrap()).unwrap();
        assert!(resolve_object(&conn, "customers").is_ok());
        assert!(resolve_object(&conn, "nope").is_err());
        assert!(resolve_object(&conn, "sqlite_master").is_err());
        assert!(resolve_object(&conn, "customers; DROP TABLE customers").is_err());
    }

    // ----- table backing ----------------------------------------------------

    #[test]
    fn rowid_paging_survives_gaps_and_windows_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gaps.sqlite");
        let setup = Connection::open(&path).unwrap();
        setup
            .execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);")
            .unwrap();
        {
            setup.execute_batch("BEGIN").unwrap();
            let mut stmt = setup.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();
            for i in 0..10_000i64 {
                stmt.execute(rusqlite::params![i, format!("v{i}")]).unwrap();
            }
            drop(stmt);
            setup.execute_batch("COMMIT").unwrap();
        }
        // Punch rowid gaps: every third row goes away.
        setup.execute("DELETE FROM t WHERE id % 3 = 0", []).unwrap();
        drop(setup);

        let (headers, backing) = backing_for(&path, "t");
        assert_eq!(headers, vec!["id", "v"]);
        let expected: Vec<i64> = (0..10_000).filter(|i| i % 3 != 0).collect();
        assert_eq!(backing.n_rows(), expected.len());
        // Anchors, not rows: one per ANCHOR_EVERY.
        assert_eq!(
            backing.anchor_count(),
            expected.len().div_ceil(ANCHOR_EVERY)
        );

        // Windows across anchor boundaries return exactly the right rows.
        for (start, len) in [(0usize, 5usize), (4090, 20), (6660, 7), (6663, 100)] {
            let rows = backing.read_rows(start, start + len).unwrap();
            assert_eq!(rows.len(), len.min(expected.len() - start));
            for (k, row) in rows.iter().enumerate() {
                assert_eq!(
                    row[0],
                    expected[start + k].to_string(),
                    "window ({start},{len}) row {k}"
                );
            }
        }
        // Past-the-end and empty windows behave like every other source.
        assert!(backing
            .read_rows(expected.len(), expected.len() + 5)
            .unwrap()
            .is_empty());
        assert!(backing.read_rows(3, 3).unwrap().is_empty());
    }

    #[test]
    fn without_rowid_tables_page_on_their_composite_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wr.sqlite");
        let setup = Connection::open(&path).unwrap();
        setup
            .execute_batch(
                "CREATE TABLE wr(a TEXT, b INTEGER, v TEXT, PRIMARY KEY (a, b)) WITHOUT ROWID;",
            )
            .unwrap();
        {
            setup.execute_batch("BEGIN").unwrap();
            let mut stmt = setup.prepare("INSERT INTO wr VALUES (?1, ?2, ?3)").unwrap();
            for i in 0..9000i64 {
                // Interleaved insert order; the key order is (a, b).
                let a = format!("k{:02}", i % 37);
                stmt.execute(rusqlite::params![a, i, format!("v{i}")])
                    .unwrap();
            }
            drop(stmt);
            setup.execute_batch("COMMIT").unwrap();
        }
        drop(setup);

        let (headers, backing) = backing_for(&path, "wr");
        assert_eq!(headers, vec!["a", "b", "v"]);
        assert_eq!(backing.n_rows(), 9000);
        assert!(matches!(backing.paging, Paging::Keyset { .. }));

        // Read a window straddling an anchor and check against a direct
        // ORDER BY (a, b) scan.
        let reference = {
            let conn = Connection::open(&path).unwrap();
            let mut stmt = conn
                .prepare("SELECT a, b, v FROM wr ORDER BY a, b LIMIT 30 OFFSET 4080")
                .unwrap();
            let rows: Vec<(String, i64, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows
        };
        let window = backing.read_rows(4080, 4110).unwrap();
        for (k, (a, b, v)) in reference.iter().enumerate() {
            assert_eq!(window[k][0], *a);
            assert_eq!(window[k][1], b.to_string());
            assert_eq!(window[k][2], *v);
        }
    }

    #[test]
    fn views_open_with_offset_paging() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let (headers, backing) = backing_for(&path, "big_orders");
        assert_eq!(headers, vec!["id", "customer_id", "total", "flag"]);
        assert!(matches!(backing.paging, Paging::Offset));
        assert_eq!(backing.n_rows(), 1);
        let rows = backing.read_rows(0, 10).unwrap();
        assert_eq!(rows[0][2], "20");
    }

    #[test]
    fn open_table_document_is_read_only_and_nulls_narrow() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let (headers, backing) = backing_for(&path, "customers");
        let mut doc = Document::from_virtual(
            7,
            "test.sqlite → customers".into(),
            headers,
            Box::new(backing),
        );
        assert!(!doc.is_editable());
        assert!(matches!(doc.ensure_editable(), Err(AppError::ReadOnly)));
        assert!(doc.set_cell(0, 0, "x".into()).is_err());
        assert_eq!(doc.meta().backing, "indexedReadOnly");
        assert_eq!(doc.meta().file_name, "test.sqlite → customers");
        assert_eq!(doc.n_rows(), 2);
        let rows = doc.fetch_rows(&[1]).unwrap();
        assert_eq!(rows[0][2], "", "NULL narrows to empty in the grid");
        // The tabular adapter over the document also works (export path).
        let src = crate::tabular::DocumentSource::new(&doc);
        use crate::tabular::TabularSource;
        assert_eq!(src.read_rows(0, 10, None).unwrap().len(), 2);
    }

    // ----- refresh detection ------------------------------------------------

    #[test]
    fn external_row_changes_fail_reads_and_flag_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let (_, backing) = backing_for(&path, "customers");
        assert_eq!(backing.read_rows(0, 2).unwrap().len(), 2);
        assert_eq!(backing.refresh_probe().unwrap(), Some((false, false)));

        // Another connection commits a row.
        let other = Connection::open(&path).unwrap();
        other
            .execute("INSERT INTO customers VALUES (3, 'Eve', 'new', 1.0)", [])
            .unwrap();
        drop(other);

        let err = backing.read_rows(0, 2).unwrap_err();
        assert!(err.to_string().contains("changed on disk"), "{err}");
        assert_eq!(
            backing.refresh_probe().unwrap(),
            Some((true, false)),
            "rows changed, schema did not"
        );
    }

    #[test]
    fn external_schema_changes_flag_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let (_, backing) = backing_for(&path, "customers");

        let other = Connection::open(&path).unwrap();
        other
            .execute("ALTER TABLE customers ADD COLUMN extra TEXT", [])
            .unwrap();
        drop(other);

        assert_eq!(
            backing.refresh_probe().unwrap(),
            Some((true, true)),
            "DDL moves both data_version and the object hash"
        );
    }

    #[test]
    fn session_baseline_detects_and_rebases() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let canonical = std::fs::canonicalize(&path).unwrap();
        let conn = safe_query::open_guarded(&canonical).unwrap();
        let schema = read_schema_info(&conn).unwrap();
        let baseline_dv = schema.data_version;
        let baseline_hash = schema.schema_hash.clone();

        let other = Connection::open(&path).unwrap();
        other
            .execute("DELETE FROM orders WHERE id = 2", [])
            .unwrap();
        drop(other);

        assert_ne!(data_version(&conn).unwrap(), baseline_dv, "rows changed");
        assert_eq!(schema_hash(&conn).unwrap(), baseline_hash, "schema did not");

        // Rebase: a fresh read matches the new state.
        let fresh = read_schema_info(&conn).unwrap();
        assert_eq!(data_version(&conn).unwrap(), fresh.data_version);
    }

    // ----- editable import ----------------------------------------------------

    #[test]
    fn import_produces_an_editable_typed_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let canonical = std::fs::canonicalize(&path).unwrap();
        let doc = import_table(&canonical, "orders", 9, u64::MAX, false, None).unwrap();
        assert!(doc.is_editable());
        assert!(doc.is_dirty(), "imports start unsaved");
        assert_eq!(doc.headers(), &["id", "customer_id", "total", "flag"]);
        assert_eq!(doc.n_rows(), 2);
        assert_eq!(doc.rows()[0], vec!["1", "1", "20", "1"]);

        // Declared SQL types arrived as F31 schemas.
        let ids = doc.column_ids().to_vec();
        let schema = doc.schema();
        assert_eq!(
            schema.column(&ids[0]).map(|c| c.logical_type),
            Some(LogicalType::Integer)
        );
        assert_eq!(
            schema.column(&ids[2]).map(|c| c.logical_type),
            Some(LogicalType::Decimal)
        );
        assert_eq!(
            schema.column(&ids[3]).map(|c| c.logical_type),
            Some(LogicalType::Boolean)
        );
    }

    #[test]
    fn import_is_memory_bounded_and_force_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let canonical = std::fs::canonicalize(&path).unwrap();
        let err = match import_table(&canonical, "customers", 9, 1, false, None) {
            Err(e) => e,
            Ok(_) => panic!("import must refuse over the memory threshold"),
        };
        assert!(
            err.to_string().contains("memory"),
            "refusal names the reason: {err}"
        );
        let doc = import_table(&canonical, "customers", 9, 1, true, None).unwrap();
        assert_eq!(doc.n_rows(), 2, "force imports anyway");
    }

    #[test]
    fn decl_type_mapping_is_conservative() {
        assert_eq!(logical_type_of_decl("INTEGER"), LogicalType::Integer);
        assert_eq!(logical_type_of_decl("int unsigned"), LogicalType::Integer);
        assert_eq!(logical_type_of_decl("BOOLEAN"), LogicalType::Boolean);
        assert_eq!(logical_type_of_decl("REAL"), LogicalType::Float);
        assert_eq!(logical_type_of_decl("double precision"), LogicalType::Float);
        assert_eq!(logical_type_of_decl("NUMERIC(10,2)"), LogicalType::Decimal);
        assert_eq!(logical_type_of_decl("DECIMAL"), LogicalType::Decimal);
        assert_eq!(logical_type_of_decl("TEXT"), LogicalType::Text);
        assert_eq!(logical_type_of_decl("BLOB"), LogicalType::Text);
        assert_eq!(logical_type_of_decl("DATETIME"), LogicalType::Text);
        assert_eq!(logical_type_of_decl(""), LogicalType::Text);
    }

    // ----- cancellation -------------------------------------------------------

    #[test]
    fn cancelled_open_releases_the_file_for_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cancel.sqlite");
        let setup = Connection::open(&path).unwrap();
        setup
            .execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);")
            .unwrap();
        {
            setup.execute_batch("BEGIN").unwrap();
            let mut stmt = setup.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();
            for i in 0..20_000i64 {
                stmt.execute(rusqlite::params![i, format!("value-{i}")])
                    .unwrap();
            }
            drop(stmt);
            setup.execute_batch("COMMIT").unwrap();
        }
        drop(setup);
        let canonical = std::fs::canonicalize(&path).unwrap();

        let registry = JobRegistry::default();
        let ctx = registry.begin("dbOpenTable", None, |_| {});
        registry.cancel(ctx.id);
        let result = build_table_backing(&canonical, "t", Some(&ctx));
        assert!(
            matches!(result, Err(AppError::Cancelled)),
            "the anchor scan observes cancellation"
        );

        // The file is immediately usable — and writable — afterwards.
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("INSERT INTO t VALUES (99999, 'after-cancel')", [])
            .unwrap();
        let n: i64 = writer
            .query_row("SELECT COUNT(*) FROM t WHERE id = 99999", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "database usable after cancellation");
    }

    #[test]
    fn cancelled_import_stops_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = rich_db(dir.path());
        let canonical = std::fs::canonicalize(&path).unwrap();
        let registry = JobRegistry::default();
        let ctx = registry.begin("dbImportTable", None, |_| {});
        registry.cancel(ctx.id);
        let result = import_table(&canonical, "customers", 9, u64::MAX, false, Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("INSERT INTO customers VALUES (7, 'Gia', 'x', 0)", [])
            .unwrap();
    }
}
