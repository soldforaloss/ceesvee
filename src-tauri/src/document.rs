//! The in-memory, mutable document model: headers, data rows, dirty tracking and
//! a command-pattern undo/redo stack.
//!
//! Invariants maintained at all times:
//! * every row in `rows` has exactly `headers.len()` cells (the grid is
//!   rectangular);
//! * `headers.len()` is the authoritative column count.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::analyze;
use crate::dto::{
    CellRect, ColumnKind, ColumnSummary, DocumentMeta, FileFingerprint, NumericSummary,
    RowsResponse, SelectionStats, SortKey,
};
use crate::error::{AppError, AppResult};
use crate::index::{IndexDirGuard, IndexHandle, IndexedFile};
use crate::parse::{ImportInfo, ParsedFile};

/// Rows scanned for [`Document::column_summaries`] on an INDEXED document.
/// Editable documents scan everything (cheap); indexed ones sample so the
/// grid's type badges never trigger a full multi-GB file scan (the F05
/// profiler remains the exact tool).
const INDEXED_SUMMARY_SAMPLE: usize = 100_000;

/// Line-ending style, tracked per document and configurable on export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "lf",
            LineEnding::Crlf => "crlf",
        }
    }

    pub fn parse(s: &str) -> LineEnding {
        if s.eq_ignore_ascii_case("crlf") {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    }
}

/// One captured cell change for undo.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CellEdit {
    row: usize,
    col: usize,
    old: String,
    new: String,
}

/// Positional column IDs for a fresh document: "c0".."cN-1". Deterministic so
/// a saved named view (F12) still resolves after the same file is reopened.
fn positional_column_ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("c{i}")).collect()
}

/// A removed column, captured for undo.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RemovedColumn {
    index: usize,
    header: String,
    values: Vec<String>,
    /// Stable logical column ID (F12), restored verbatim on undo so named
    /// views referencing the column survive a delete + undo round-trip.
    #[serde(default)]
    id: String,
}

/// A single reversible edit. Structural ops capture exactly what they need to
/// undo without snapshotting the whole document. Serializable so the F16
/// crash-recovery journal can persist and replay operations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum EditOp {
    SetCells(Vec<CellEdit>),
    InsertRows {
        at: usize,
        count: usize,
    },
    /// Rows removed, ascending by original index.
    DeleteRows {
        removed: Vec<(usize, Vec<String>)>,
    },
    MoveRow {
        from: usize,
        to: usize,
    },
    InsertColumn {
        at: usize,
        name: String,
        /// Stable logical column ID (F12), assigned once at op creation so
        /// undo/redo (and journal replay) reproduce the same identity.
        #[serde(default)]
        id: String,
    },
    /// Columns removed, ascending by original index.
    DeleteColumns {
        removed: Vec<RemovedColumn>,
    },
    RenameColumn {
        col: usize,
        old: String,
        new: String,
    },
    MoveColumn {
        from: usize,
        to: usize,
    },
    /// `order[new_position] = old_position`.
    SortRows {
        order: Vec<u32>,
    },
    /// A group applied/reverted atomically (e.g. a paste that grows the grid).
    Composite(Vec<EditOp>),
    /// A selective revert (F15): applying this op REVERTS the wrapped op,
    /// and reverting it re-applies the wrapped op — so reverts live on the
    /// ordinary undo stack and are themselves undoable.
    Inverse(Box<EditOp>),
}

/// Identity + timestamp for one undo-stack operation (F15), kept in
/// lockstep with the stacks.
#[derive(Debug, Clone, Copy)]
struct OpMeta {
    id: u64,
    epoch_secs: u64,
}

/// One cell's before/after in a change summary (F15).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellChange {
    pub row: usize,
    pub col: usize,
    pub old: String,
    pub new: String,
}

/// One unsaved operation, summarised for the Changes panel (F15).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSummary {
    /// Stable id, valid while the operation stays on the undo stack.
    pub id: u64,
    pub epoch_secs: u64,
    pub kind: String,
    pub cells_affected: usize,
    /// Bounded cell-level sample with old and new values.
    pub sample: Vec<CellChange>,
    pub structural: bool,
    /// Whether this op can be selectively reverted right now.
    pub revertible: bool,
    pub blocked_reason: Option<String>,
}

/// Single-pass accumulator behind [`Document::column_summaries`].
struct SummaryAccumulator {
    nulls: usize,
    numeric: usize,
    booly: usize,
    datey: usize,
    unique: HashSet<String>,
    sum: f64,
    min: f64,
    max: f64,
}

impl Default for SummaryAccumulator {
    fn default() -> Self {
        SummaryAccumulator {
            nulls: 0,
            numeric: 0,
            booly: 0,
            datey: 0,
            unique: HashSet::new(),
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }
}

impl SummaryAccumulator {
    fn record(&mut self, cell: &str) {
        let trimmed = cell.trim();
        if trimmed.is_empty() {
            self.nulls += 1;
            return;
        }
        if !self.unique.contains(trimmed) {
            self.unique.insert(trimmed.to_string());
        }
        if let Some(n) = analyze::as_number(trimmed) {
            self.numeric += 1;
            self.sum += n;
            if n < self.min {
                self.min = n;
            }
            if n > self.max {
                self.max = n;
            }
        } else if analyze::is_bool(trimmed) {
            self.booly += 1;
        } else if analyze::is_date(trimmed) {
            self.datey += 1;
        }
    }

    fn into_summary(self, col: usize, count: usize, sampled: bool) -> ColumnSummary {
        // A column takes a non-text kind only when *every* non-empty cell
        // matches it; otherwise it is text (blanks never decide the kind).
        let non_empty = count - self.nulls;
        let kind = if non_empty == 0 {
            ColumnKind::Text
        } else if self.numeric == non_empty {
            ColumnKind::Number
        } else if self.booly == non_empty {
            ColumnKind::Bool
        } else if self.datey == non_empty {
            ColumnKind::Date
        } else {
            ColumnKind::Text
        };

        let numeric_summary = (self.numeric > 0).then_some(NumericSummary {
            min: self.min,
            max: self.max,
            mean: self.sum / self.numeric as f64,
        });

        ColumnSummary {
            sampled,
            column: col,
            kind,
            count,
            nulls: self.nulls,
            unique: self.unique.len(),
            numeric: numeric_summary,
        }
    }
}

/// Pluggable read-only row provider behind [`Backing::Virtual`] (F35 database
/// tables). Implementations serve bounded window reads out of an external
/// store (a SQLite table paged by rowid/keyset) without ever materialising
/// the full row set; cells narrow the store's missing values (SQL `NULL`) to
/// empty strings, matching the document contract that every cell is present.
pub trait VirtualRows: Send + Sync {
    /// Total data rows, fixed at open time. External growth/shrink is
    /// surfaced through [`VirtualRows::refresh_probe`] and read errors, never
    /// as a silently changing length.
    fn n_rows(&self) -> usize;

    /// Read rows `[start, end)` (the caller clamps to `n_rows`), each padded
    /// to the column count. Must return exactly `end - start` rows or error.
    fn read_rows(&self, start: usize, end: usize) -> AppResult<Vec<Vec<String>>>;

    /// Whether the external store changed since open, as
    /// `(rows_changed, schema_changed)`; `None` when the provider cannot
    /// tell. Never mutates the store.
    fn refresh_probe(&self) -> AppResult<Option<(bool, bool)>> {
        Ok(None)
    }
}

/// Rows fetched per block while visiting a [`Backing::Virtual`] range
/// (matches the F10 index's visit block size).
const VIRTUAL_VISIT_BLOCK: usize = 4096;
/// Scattered virtual reads coalesce indices whose gap is at most this many
/// records into one contiguous read (matches the F10 index).
const VIRTUAL_COALESCE_GAP: usize = 64;

/// How a document's rows are stored (F10, F32, F35).
pub enum Backing {
    /// Fully materialised and mutable (the default).
    Memory,
    /// Streaming, read-only access through a record index; `rows` stays
    /// empty and every mutation fails with [`AppError::ReadOnly`].
    Indexed(IndexHandle),
    /// Read-only windowed access over a columnar file (F32: Parquet / Arrow
    /// IPC). Like `Indexed`, `rows` stays empty and mutations fail; the text
    /// plane renders columnar NULL as an empty cell while the handle keeps
    /// the null-vs-empty distinction for export and conversion.
    Columnar(crate::parquet_arrow::ColumnarHandle),
    /// Read-only rows served by a pluggable provider (F35 database tables).
    /// Like `Indexed`: `rows` stays empty and every mutation fails.
    Virtual(Box<dyn VirtualRows>),
}

/// An open document.
pub struct Document {
    pub id: u64,
    pub path: Option<PathBuf>,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    has_header_row: bool,
    delimiter: u8,
    encoding_name: String,
    had_bom: bool,
    line_ending: LineEnding,
    /// Cells changed since the last save (best-effort, for highlighting).
    dirty_cells: HashSet<(usize, usize)>,
    undo_stack: Vec<EditOp>,
    redo_stack: Vec<EditOp>,
    /// Identity + timestamps for `undo_stack`/`redo_stack`, in lockstep (F15).
    undo_meta: Vec<OpMeta>,
    redo_meta: Vec<OpMeta>,
    next_op_id: u64,
    /// Append-only crash-recovery journal (F16), when journaling is on.
    journal: Option<crate::journal::JournalWriter>,
    /// Read-only follow/tail mode (F19).
    follow: bool,
    /// F19 "only new rows": when set, the filter view is a LIVE range from
    /// this absolute row onward — `append_follow_rows` extends it so newly
    /// appended records stay visible while filtered. Cleared by an ordinary
    /// filter or clear-filter.
    follow_range_from: Option<usize>,
    /// `undo_stack.len()` at the last save; the document is dirty when it differs.
    saved_marker: usize,
    /// Stable logical column IDs (F12), in lockstep with `headers`. Assigned
    /// positionally at parse ("c0".."cN-1"); inserts mint fresh IDs and every
    /// op carries the IDs it touches, so undo/redo restore identities exactly.
    column_ids: Vec<String>,
    /// Next fresh column-ID number (never reused within a session).
    next_column_id: u64,
    /// Row-filter ingredient of the display view: absolute indices of the rows
    /// matching the active filter, in SOURCE order; `None` when unfiltered.
    filter_rows: Option<Vec<usize>>,
    /// Non-destructive sort ingredient of the display view (F12): the keys the
    /// visible rows are ordered by. Never touches source order or the undo
    /// stack; empty = source order.
    view_sort: Vec<SortKey>,
    /// The COMPOSED display→absolute row mapping (filter ∘ view sort); `None`
    /// when both ingredients are off. A non-undoable snapshot: recomputed when
    /// either ingredient changes and cleared by structural mutations (handled
    /// in the command layer).
    filter_view: Option<Vec<usize>>,
    /// Monotonically increasing revision, bumped on every change that could
    /// invalidate a deferred operation: data and structural mutations,
    /// undo/redo, header-mode toggles and filter-view changes. Previews and
    /// long-running results carry the revision they were computed against and
    /// are rejected when it no longer matches (see [`Document::check_revision`]).
    revision: u64,
    /// Per-column: the revision that last changed that column's DATA (cell
    /// edits touch just their columns; row inserts/deletes and column
    /// structure changes touch every column; pure reorderings touch none).
    /// Lets per-column caches (F05 profiles) survive edits to other columns.
    col_revisions: Vec<u64>,
    /// The revision at which the filter view last changed, for caches scoped
    /// to the visible rows.
    filter_revision: u64,
    /// Fidelity information captured when the source file was parsed
    /// (decode damage, ragged records). Refreshed only by a reparse.
    import_info: ImportInfo,
    /// Identity of the backing file as of the last open/reparse/save, used to
    /// detect external modification. `None` for unsaved documents.
    fingerprint: Option<FileFingerprint>,
    /// Row storage: in-memory (editable) or a streaming record index (F10).
    backing: Backing,
    /// F31 explicit schema: logical per-column metadata keyed by stable
    /// column IDs. Lives OUTSIDE the undo stack on purpose — schema edits
    /// (including `displayFormat` changes) are not undoable and never make
    /// the document dirty, because they never touch stored cell text.
    schema: crate::schema::DocumentSchema,
    /// Bumped on every schema mutation so the front end (and later
    /// persistence, F37) can detect schema changes without the document
    /// revision — and therefore previews and the dirty flag — moving.
    schema_revision: u64,
    /// F31 advisory-mode validation issues: edits accepted despite failing
    /// their column's declared type. Bounded
    /// ([`crate::schema::MAX_SCHEMA_ISSUES`]), newest kept.
    schema_issues: Vec<crate::schema::SchemaIssue>,
    /// F38 data dictionary: human documentation per column keyed by stable
    /// column ID. Like the schema it lives OUTSIDE the undo stack and is pure
    /// metadata — editing it never rewrites cells and never makes the document
    /// dirty. Entries survive renames/reorders (keyed by ID) and are kept when
    /// a column is deleted (reported as orphaned until explicitly discarded).
    dictionary: crate::dictionary::Dictionary,
    /// Bumped on every dictionary mutation, independent of both the document
    /// `revision` and `schema_revision`, so documentation edits never move the
    /// dirty flag or invalidate data/schema previews.
    dictionary_revision: u64,
    /// F17: where the document came from when opened out of an archive.
    /// Archive-backed documents have no `path` (no in-place saving); Save As
    /// clears this and turns them into ordinary file documents.
    archive: Option<crate::archive::ArchiveOrigin>,
    /// Keeps the extracted archive entry alive while an INDEXED document
    /// reads it directly (editable documents parse and release the file).
    #[allow(dead_code)] // held for its Drop effect
    archive_guard: Option<IndexDirGuard>,
    /// Display label for documents without a `path` (F35 database tables:
    /// "db.sqlite → table"). Takes precedence over the path-derived name in
    /// [`Document::meta`]; cleared by Save As like the archive origin.
    display_name: Option<String>,
}

impl Document {
    /// Build a document from a freshly parsed file.
    pub fn from_parsed(
        id: u64,
        path: Option<PathBuf>,
        parsed: ParsedFile,
        has_header_row: bool,
    ) -> Document {
        let ParsedFile {
            mut records,
            n_cols,
            delimiter,
            encoding,
            had_bom,
            uses_crlf,
            import,
        } = parsed;

        let (headers, rows) = if has_header_row && !records.is_empty() {
            // The genuine header row is kept verbatim (including blanks) for
            // faithful round-tripping; only its width is normalised.
            let mut headers = records.remove(0);
            headers.resize(n_cols, String::new());
            (headers, records)
        } else {
            // Synthetic labels — never written on export (no header row).
            let headers = (0..n_cols).map(|i| format!("Column {}", i + 1)).collect();
            (headers, records)
        };

        let n_cols_final = headers.len();
        Document {
            id,
            path,
            headers,
            rows,
            has_header_row,
            delimiter,
            encoding_name: encoding.name().to_string(),
            had_bom,
            line_ending: if uses_crlf {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            dirty_cells: HashSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_meta: Vec::new(),
            redo_meta: Vec::new(),
            next_op_id: 0,
            journal: None,
            follow: false,
            follow_range_from: None,
            saved_marker: 0,
            column_ids: positional_column_ids(n_cols_final),
            next_column_id: n_cols_final as u64,
            filter_rows: None,
            view_sort: Vec::new(),
            filter_view: None,
            revision: 1,
            col_revisions: vec![1; n_cols_final],
            filter_revision: 1,
            import_info: import,
            fingerprint: None,
            schema: crate::schema::DocumentSchema::default(),
            schema_revision: 0,
            schema_issues: Vec::new(),
            dictionary: crate::dictionary::Dictionary::default(),
            dictionary_revision: 0,
            backing: Backing::Memory,
            archive: None,
            archive_guard: None,
            display_name: None,
        }
    }

    /// Create an empty in-memory document (File → New).
    pub fn new_empty(id: u64, cols: usize, rows: usize) -> Document {
        let cols = cols.max(1);
        let headers = (0..cols).map(|i| format!("Column {}", i + 1)).collect();
        let data = vec![vec![String::new(); cols]; rows];
        Document {
            id,
            path: None,
            headers,
            rows: data,
            has_header_row: false,
            delimiter: b',',
            encoding_name: "UTF-8".to_string(),
            had_bom: false,
            line_ending: if cfg!(windows) {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            dirty_cells: HashSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_meta: Vec::new(),
            redo_meta: Vec::new(),
            next_op_id: 0,
            journal: None,
            follow: false,
            follow_range_from: None,
            saved_marker: 0,
            column_ids: positional_column_ids(cols),
            next_column_id: cols as u64,
            filter_rows: None,
            view_sort: Vec::new(),
            filter_view: None,
            revision: 1,
            col_revisions: vec![1; cols],
            filter_revision: 1,
            import_info: ImportInfo::default(),
            fingerprint: None,
            schema: crate::schema::DocumentSchema::default(),
            schema_revision: 0,
            schema_issues: Vec::new(),
            dictionary: crate::dictionary::Dictionary::default(),
            dictionary_revision: 0,
            backing: Backing::Memory,
            archive: None,
            archive_guard: None,
            display_name: None,
        }
    }

    /// Build a read-only document over a freshly built record index (F10).
    pub fn from_index(id: u64, path: Option<PathBuf>, indexed: IndexedFile) -> Document {
        let IndexedFile {
            handle,
            headers,
            has_header_row,
            encoding_name,
            had_bom,
            uses_crlf,
            import,
        } = indexed;
        let n_cols = headers.len();
        Document {
            id,
            path,
            headers,
            rows: Vec::new(),
            has_header_row,
            delimiter: handle.delimiter(),
            encoding_name,
            had_bom,
            line_ending: if uses_crlf {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            dirty_cells: HashSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_meta: Vec::new(),
            redo_meta: Vec::new(),
            next_op_id: 0,
            journal: None,
            follow: false,
            follow_range_from: None,
            saved_marker: 0,
            column_ids: positional_column_ids(n_cols),
            next_column_id: n_cols as u64,
            filter_rows: None,
            view_sort: Vec::new(),
            filter_view: None,
            revision: 1,
            col_revisions: vec![1; n_cols],
            filter_revision: 1,
            import_info: import,
            fingerprint: None,
            schema: crate::schema::DocumentSchema::default(),
            schema_revision: 0,
            schema_issues: Vec::new(),
            dictionary: crate::dictionary::Dictionary::default(),
            dictionary_revision: 0,
            backing: Backing::Indexed(handle),
            archive: None,
            archive_guard: None,
            display_name: None,
        }
    }

    /// Build a read-only document over a virtual row provider (F35 database
    /// tables). SQL column names are a real header row; the provider narrows
    /// SQL `NULL` to empty cells (the document contract has no missing slot).
    pub fn from_virtual(
        id: u64,
        display_name: String,
        headers: Vec<String>,
        backing: Box<dyn VirtualRows>,
    ) -> Document {
        let n_cols = headers.len();
        Document {
            id,
            path: None,
            headers,
            rows: Vec::new(),
            has_header_row: true,
            delimiter: b',',
            encoding_name: "UTF-8".to_string(),
            had_bom: false,
            line_ending: if cfg!(windows) {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            dirty_cells: HashSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_meta: Vec::new(),
            redo_meta: Vec::new(),
            next_op_id: 0,
            journal: None,
            follow: false,
            follow_range_from: None,
            saved_marker: 0,
            column_ids: positional_column_ids(n_cols),
            next_column_id: n_cols as u64,
            filter_rows: None,
            view_sort: Vec::new(),
            filter_view: None,
            revision: 1,
            col_revisions: vec![1; n_cols],
            filter_revision: 1,
            import_info: ImportInfo::default(),
            fingerprint: None,
            schema: crate::schema::DocumentSchema::default(),
            schema_revision: 0,
            schema_issues: Vec::new(),
            dictionary: crate::dictionary::Dictionary::default(),
            dictionary_revision: 0,
            backing: Backing::Virtual(backing),
            archive: None,
            archive_guard: None,
            display_name: Some(display_name),
        }
    }

    /// Build a read-only document over an open columnar file (F32:
    /// Parquet / Arrow IPC). Headers are the flattened path-based column
    /// names (always real names — never synthetic), and the generated F31
    /// schemas are attached keyed by the positional column IDs.
    pub fn from_columnar(
        id: u64,
        path: Option<PathBuf>,
        columnar: crate::parquet_arrow::ColumnarFile,
    ) -> Document {
        let crate::parquet_arrow::ColumnarFile {
            handle,
            headers,
            schemas,
        } = columnar;
        let n_cols = headers.len();
        let column_ids = positional_column_ids(n_cols);
        let mut schema = crate::schema::DocumentSchema::default();
        for (i, mut col_schema) in schemas.into_iter().enumerate() {
            col_schema.column_id = column_ids[i].clone();
            schema.set_column(col_schema);
        }
        Document {
            id,
            path,
            headers,
            rows: Vec::new(),
            has_header_row: true,
            delimiter: b',',
            encoding_name: "UTF-8".to_string(),
            had_bom: false,
            line_ending: LineEnding::Lf,
            dirty_cells: HashSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_meta: Vec::new(),
            redo_meta: Vec::new(),
            next_op_id: 0,
            journal: None,
            follow: false,
            follow_range_from: None,
            saved_marker: 0,
            column_ids,
            next_column_id: n_cols as u64,
            filter_rows: None,
            view_sort: Vec::new(),
            filter_view: None,
            revision: 1,
            col_revisions: vec![1; n_cols],
            filter_revision: 1,
            import_info: ImportInfo::default(),
            fingerprint: None,
            schema,
            schema_revision: 0,
            schema_issues: Vec::new(),
            dictionary: crate::dictionary::Dictionary::default(),
            dictionary_revision: 0,
            backing: Backing::Columnar(handle),
            archive: None,
            archive_guard: None,
        }
    }

    // ----- accessors -------------------------------------------------------

    pub fn n_cols(&self) -> usize {
        self.headers.len()
    }

    pub fn n_rows(&self) -> usize {
        match &self.backing {
            Backing::Memory => self.rows.len(),
            Backing::Indexed(handle) => handle.n_data_records(),
            Backing::Columnar(handle) => handle.n_rows(),
            Backing::Virtual(rows) => rows.n_rows(),
        }
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    /// Stable logical column IDs (F12), in lockstep with `headers`. The
    /// identity layer explicit schemas (F31) key their per-column entries on:
    /// IDs survive renames, reorders, inserts, deletes and undo/redo.
    pub fn column_ids(&self) -> &[String] {
        &self.column_ids
    }

    // ----- explicit schema (F31) -------------------------------------------

    /// The document's explicit schema (columns without an entry are plain
    /// text). Metadata only — reading or assigning it never rewrites cells.
    pub fn schema(&self) -> &crate::schema::DocumentSchema {
        &self.schema
    }

    /// Bumped on every schema mutation; independent of [`Document::revision`]
    /// so schema edits never dirty the document or invalidate previews.
    pub fn schema_revision(&self) -> u64 {
        self.schema_revision
    }

    /// The declared schema of the column at POSITION `col`, resolved through
    /// its stable ID (so it survives renames and reorders).
    pub fn column_schema_at(&self, col: usize) -> Option<&crate::schema::ColumnSchema> {
        self.column_ids
            .get(col)
            .and_then(|id| self.schema.column(id))
    }

    /// Cloned per-key schemas, parallel to `keys`, for
    /// [`crate::sort::compare_rows_schema`] (cloned so the comparator can run
    /// while row storage is independently borrowed).
    fn key_schemas(&self, keys: &[SortKey]) -> Vec<Option<crate::schema::ColumnSchema>> {
        keys.iter()
            .map(|k| self.column_schema_at(k.column).cloned())
            .collect()
    }

    /// Replace the whole schema (import / clear).
    pub fn set_document_schema(&mut self, schema: crate::schema::DocumentSchema) {
        self.schema = schema;
        self.schema_revision += 1;
    }

    /// Insert or replace one column's schema entry.
    pub fn set_column_schema(&mut self, schema: crate::schema::ColumnSchema) {
        self.schema.set_column(schema);
        self.schema_revision += 1;
    }

    /// Remove one column's schema entry (back to implicit text). Removing a
    /// missing entry still bumps the schema revision; harmless.
    pub fn remove_column_schema(&mut self, column_id: &str) -> bool {
        let removed = self.schema.columns.remove(column_id).is_some();
        self.schema_revision += 1;
        removed
    }

    /// Carry the schema across a whole-`Document` replacement (reparse /
    /// reindex): entries are keyed by stable IDs, which restart positionally,
    /// so they re-attach to the same positions the original open assigned.
    pub fn inherit_schema(&mut self, prev: &Document) {
        self.schema = prev.schema.clone();
        self.schema_revision = prev.schema_revision + 1;
    }

    /// Record advisory-validation issues (bounded: oldest evicted).
    pub fn record_schema_issues(
        &mut self,
        issues: impl IntoIterator<Item = crate::schema::SchemaIssue>,
    ) {
        self.schema_issues.extend(issues);
        let len = self.schema_issues.len();
        if len > crate::schema::MAX_SCHEMA_ISSUES {
            self.schema_issues
                .drain(..len - crate::schema::MAX_SCHEMA_ISSUES);
        }
    }

    /// The recorded advisory-validation issues, oldest first.
    pub fn schema_issues(&self) -> &[crate::schema::SchemaIssue] {
        &self.schema_issues
    }

    pub fn clear_schema_issues(&mut self) {
        self.schema_issues.clear();
    }

    // ----- data dictionary (F38) -------------------------------------------

    /// The document's data dictionary (per-column documentation keyed by
    /// stable column ID). Reading it never touches cells.
    pub fn dictionary(&self) -> &crate::dictionary::Dictionary {
        &self.dictionary
    }

    /// Bumped on every dictionary mutation; independent of [`Document::revision`]
    /// and [`Document::schema_revision`] so documentation edits never dirty the
    /// document or invalidate data/schema previews.
    pub fn dictionary_revision(&self) -> u64 {
        self.dictionary_revision
    }

    /// Insert or replace one column's dictionary entry.
    pub fn set_dictionary_field(&mut self, field: crate::dictionary::DictionaryField) {
        self.dictionary.set(field);
        self.dictionary_revision += 1;
    }

    /// Remove one column's dictionary entry (e.g. discarding an orphan).
    /// Returns whether an entry was present.
    pub fn remove_dictionary_field(&mut self, column_id: &str) -> bool {
        let removed = self.dictionary.remove(column_id);
        self.dictionary_revision += 1;
        removed
    }

    /// Replace the whole dictionary (import / merge apply / bulk discard).
    pub fn set_dictionary(&mut self, dictionary: crate::dictionary::Dictionary) {
        self.dictionary = dictionary;
        self.dictionary_revision += 1;
    }

    /// Carry the dictionary across a whole-`Document` replacement (reparse /
    /// reindex): entries are keyed by stable IDs, which restart positionally,
    /// so they re-attach to the same columns the original open assigned.
    pub fn inherit_dictionary(&mut self, prev: &Document) {
        self.dictionary = prev.dictionary.clone();
        self.dictionary_revision = prev.dictionary_revision + 1;
    }

    /// Guard a dictionary-dependent deferred operation (F38: a merge/import
    /// resolution prepared against a snapshot): fail with
    /// [`AppError::StaleDictionaryRevision`] when the dictionary moved since
    /// `expected` was captured. Independent of the data and schema revisions.
    pub fn check_dictionary_revision(&self, expected: u64) -> AppResult<()> {
        if self.dictionary_revision == expected {
            Ok(())
        } else {
            Err(AppError::StaleDictionaryRevision {
                expected,
                actual: self.dictionary_revision,
            })
        }
    }

    /// The in-memory row slice. EDITABLE backing only: for indexed documents
    /// this is always empty — mutation paths must gate with
    /// [`Document::ensure_editable`] first, and read paths must go through
    /// [`Document::visit_rows`] / [`Document::visit_rows_at`] instead.
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// Whether the document supports mutation (in-memory backing).
    pub fn is_editable(&self) -> bool {
        matches!(self.backing, Backing::Memory)
    }

    /// Guard for mutation paths: fail with [`AppError::ReadOnly`] on an
    /// indexed document.
    pub fn ensure_editable(&self) -> AppResult<()> {
        if self.follow {
            return Err(AppError::invalid(
                "follow mode is read-only — stop following to edit",
            ));
        }
        if self.is_editable() {
            Ok(())
        } else {
            Err(AppError::ReadOnly)
        }
    }

    // ----- follow mode (F19) -------------------------------------------------

    /// Put the document into read-only follow mode (F19).
    pub fn set_follow(&mut self, follow: bool) {
        self.follow = follow;
    }

    /// Append rows arriving from the follow watcher (F19). Bypasses the
    /// editable gate deliberately — follow documents have no undo history,
    /// and the rows come from the file itself.
    pub fn append_follow_rows(&mut self, rows: Vec<Vec<String>>) {
        let width = self.headers.len();
        for mut row in rows {
            row.resize(width, String::new());
            self.rows.push(row);
        }
        self.revision += 1;
        self.touch_all_columns();
        // A live "only new rows" range keeps following the file: extend the
        // view to include what was just appended. Follow documents are
        // memory-backed, so recomposition cannot fail in practice — if it
        // ever did, dropping the whole view beats leaving a stale one.
        if let Some(from) = self.follow_range_from {
            let rows: Vec<usize> = (from.min(self.rows.len())..self.rows.len()).collect();
            if self.set_filter(rows).is_err() {
                self.clear_row_view();
            }
        }
    }

    /// Mark (or clear) the F19 live "only new rows" range. The caller still
    /// applies the initial filter; appends keep it extended.
    pub fn set_follow_range(&mut self, from: Option<usize>) {
        self.follow_range_from = from;
    }

    /// Wire name of the backing, carried on [`DocumentMeta`]. Columnar (F32)
    /// documents report `indexedReadOnly` DELIBERATELY: every front-end
    /// affordance for the F10 indexed mode (read-only gating, the
    /// convert-to-editable flow) applies to them unchanged.
    pub fn backing_name(&self) -> &'static str {
        match self.backing {
            Backing::Memory => "editable",
            Backing::Indexed(_) | Backing::Columnar(_) => "indexedReadOnly",
            // Database-backed documents (F35) report the same wire name so
            // every existing read-only affordance in the front end applies
            // unchanged; a dedicated meta flag can distinguish them later.
            Backing::Virtual(_) => "indexedReadOnly",
        }
    }

    /// The columnar handle behind an F32 Parquet/Arrow document, when this
    /// document has one. Export and conversion use it to read the `Option`
    /// plane (columnar NULL = `None`, empty string = `Some("")`), which the
    /// rectangular text plane cannot carry.
    pub fn columnar_handle(&self) -> Option<&crate::parquet_arrow::ColumnarHandle> {
        match &self.backing {
            Backing::Columnar(handle) => Some(handle),
            _ => None,
        }
    }

    /// F32: the absolute row ranges a filter scan must visit, when the
    /// columnar backing's row-group statistics prove the skipped groups
    /// cannot match `spec`. `None` = scan everything (non-columnar backings,
    /// no usable statistics, or no eligible conditions).
    pub fn filter_scan_ranges(
        &self,
        spec: &crate::dto::FilterGroup,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        match &self.backing {
            Backing::Columnar(handle) => {
                handle.filter_scan_ranges(spec, &|col| self.column_schema_at(col).cloned())
            }
            _ => None,
        }
    }

    /// Set the tab label used while the document has no path (F35 imports:
    /// "db.sqlite → table"). Save As replaces it with the file name.
    pub fn set_display_name(&mut self, name: impl Into<String>) {
        self.display_name = Some(name.into());
    }

    /// F35: whether a database-backed document's external store changed
    /// since it was opened, as `(rows_changed, schema_changed)`. `None` for
    /// every other backing.
    pub fn virtual_refresh_probe(&self) -> AppResult<Option<(bool, bool)>> {
        match &self.backing {
            Backing::Virtual(rows) => rows.refresh_probe(),
            _ => Ok(None),
        }
    }

    /// Swap an indexed document to fully materialised editable rows (F10
    /// convert-to-editable). The caller streams `rows` out of the index
    /// beforehand; the grid is expected to be rectangular already.
    pub fn make_editable(&mut self, rows: Vec<Vec<String>>) -> AppResult<()> {
        if self.is_editable() {
            return Err(AppError::invalid("document is already editable"));
        }
        debug_assert!(rows.iter().all(|r| r.len() == self.headers.len()));
        self.rows = rows;
        self.backing = Backing::Memory;
        // The row storage changed identity; anything captured against the
        // indexed incarnation must be invalidated.
        self.revision += 1;
        self.touch_all_columns();
        Ok(())
    }

    // ----- row access (both backings) --------------------------------------

    /// Visit data rows `[range)` in order (absolute coordinates). Indexed
    /// documents stream bounded blocks from disk; the borrowed row is only
    /// valid during the callback. Return `Ok(false)` to stop early.
    pub fn visit_rows(
        &self,
        range: std::ops::Range<usize>,
        f: &mut dyn FnMut(usize, &[String]) -> AppResult<bool>,
    ) -> AppResult<()> {
        match &self.backing {
            Backing::Memory => {
                let end = range.end.min(self.rows.len());
                for i in range.start.min(end)..end {
                    if !f(i, &self.rows[i])? {
                        return Ok(());
                    }
                }
                Ok(())
            }
            Backing::Indexed(handle) => handle.visit(range, f),
            Backing::Columnar(handle) => handle.visit(range, f),
            Backing::Virtual(rows) => {
                let end = range.end.min(rows.n_rows());
                let mut start = range.start.min(end);
                while start < end {
                    let block_end = (start + VIRTUAL_VISIT_BLOCK).min(end);
                    let block = rows.read_rows(start, block_end)?;
                    if block.len() != block_end - start {
                        return Err(AppError::Other(
                            "the database changed on disk; refresh the document".into(),
                        ));
                    }
                    for (i, row) in block.iter().enumerate() {
                        if !f(start + i, row)? {
                            return Ok(());
                        }
                    }
                    start = block_end;
                }
                Ok(())
            }
        }
    }

    /// Visit specific absolute rows in CALLER order. Indexed documents
    /// coalesce nearby indices into shared contiguous reads.
    pub fn visit_rows_at(
        &self,
        indices: &[usize],
        f: &mut dyn FnMut(usize, &[String]) -> AppResult<bool>,
    ) -> AppResult<()> {
        match &self.backing {
            Backing::Memory => {
                if let Some(&bad) = indices.iter().find(|&&i| i >= self.rows.len()) {
                    return Err(AppError::invalid(format!("row {bad} is out of range")));
                }
                for &i in indices {
                    if !f(i, &self.rows[i])? {
                        return Ok(());
                    }
                }
                Ok(())
            }
            Backing::Indexed(handle) => handle.visit_at(indices, f),
            Backing::Columnar(handle) => handle.visit_at(indices, f),
            Backing::Virtual(rows) => {
                // Mirror the F10 index: fetch each distinct row once per
                // chunk, coalescing nearby indices into one windowed read,
                // then emit in CALLER order.
                let n = rows.n_rows();
                for chunk in indices.chunks(VIRTUAL_VISIT_BLOCK) {
                    if let Some(&bad) = chunk.iter().find(|&&i| i >= n) {
                        return Err(AppError::invalid(format!("row {bad} is out of range")));
                    }
                    let mut sorted: Vec<usize> = chunk.to_vec();
                    sorted.sort_unstable();
                    sorted.dedup();
                    let mut fetched: HashMap<usize, Vec<String>> =
                        HashMap::with_capacity(sorted.len());
                    let mut run_start = 0usize;
                    while run_start < sorted.len() {
                        let mut run_end = run_start;
                        while run_end + 1 < sorted.len()
                            && sorted[run_end + 1] - sorted[run_end] <= VIRTUAL_COALESCE_GAP
                        {
                            run_end += 1;
                        }
                        let lo = sorted[run_start];
                        let hi = sorted[run_end];
                        let block = rows.read_rows(lo, hi + 1)?;
                        if block.len() != hi + 1 - lo {
                            return Err(AppError::Other(
                                "the database changed on disk; refresh the document".into(),
                            ));
                        }
                        for &want in &sorted[run_start..=run_end] {
                            fetched.insert(want, block[want - lo].clone());
                        }
                        run_start = run_end + 1;
                    }
                    for &i in chunk {
                        let row = fetched.get(&i).expect("fetched above");
                        if !f(i, row)? {
                            return Ok(());
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Owned copies of specific absolute rows, in caller order.
    pub fn fetch_rows(&self, indices: &[usize]) -> AppResult<Vec<Vec<String>>> {
        let mut out = Vec::with_capacity(indices.len());
        self.visit_rows_at(indices, &mut |_, row| {
            out.push(row.to_vec());
            Ok(true)
        })?;
        Ok(out)
    }

    pub fn has_header_row(&self) -> bool {
        self.has_header_row
    }

    /// Canonical name of the encoding the source file was decoded from.
    pub fn encoding_name(&self) -> &str {
        &self.encoding_name
    }

    /// Import-time fidelity information (decode damage, ragged records).
    pub fn import_info(&self) -> &ImportInfo {
        &self.import_info
    }

    /// Identity of the backing file as of the last open/reparse/save.
    pub fn fingerprint(&self) -> Option<FileFingerprint> {
        self.fingerprint
    }

    /// Record the backing file's identity (after an open, reparse or save).
    pub fn set_fingerprint(&mut self, fingerprint: Option<FileFingerprint>) {
        self.fingerprint = fingerprint;
    }

    /// Record that this document was opened out of an archive (F17). The
    /// guard, when present, keeps the extracted entry alive for indexed
    /// documents that read it directly.
    pub fn set_archive_origin(
        &mut self,
        origin: crate::archive::ArchiveOrigin,
        guard: Option<IndexDirGuard>,
    ) {
        self.archive = Some(origin);
        self.archive_guard = guard;
    }

    /// Attach the temp-directory guard backing a DERIVED document (F20–F23:
    /// append/join/group/pivot outputs spilled to disk). The guard's Drop
    /// deletes the directory when the document closes; a Save As clears it
    /// through [`Document::mark_saved`], exactly like archive extractions.
    pub fn set_derived_guard(&mut self, guard: IndexDirGuard) {
        self.archive_guard = Some(guard);
    }

    /// Mark a freshly built DERIVED document as unsaved-with-content: it has
    /// no undo history, but closing it must still warn, and Save routes to
    /// Save As. The first successful save clears this via `mark_saved`.
    pub fn mark_derived_unsaved(&mut self) {
        self.saved_marker = usize::MAX;
    }

    /// Current document revision (see the field docs for what bumps it).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Continue the revision sequence from a previous incarnation of this
    /// document. Used when reparsing replaces the whole `Document` value, so a
    /// preview taken before the swap can never match the new document.
    pub fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// The revision that last changed this column's data. Out-of-range columns
    /// report the document revision (always-invalid, always-safe).
    pub fn column_revision(&self, col: usize) -> u64 {
        self.col_revisions
            .get(col)
            .copied()
            .unwrap_or(self.revision)
    }

    /// The revision at which the filter view last changed.
    pub fn filter_revision(&self) -> u64 {
        self.filter_revision
    }

    /// Guard a deferred operation: fail with [`AppError::StaleRevision`] when
    /// the document has changed since `expected` was captured.
    pub fn check_revision(&self, expected: u64) -> AppResult<()> {
        if self.revision == expected {
            Ok(())
        } else {
            Err(AppError::StaleRevision {
                expected,
                actual: self.revision,
            })
        }
    }

    /// Guard a schema-dependent deferred operation (F31): fail with
    /// [`AppError::StaleSchemaRevision`] when the schema has changed since
    /// `expected` was captured. The document `revision` and this schema
    /// revision move independently, so an operation whose result depends on
    /// the declared schema (conversion, invalid scan) must guard against BOTH.
    pub fn check_schema_revision(&self, expected: u64) -> AppResult<()> {
        if self.schema_revision == expected {
            Ok(())
        } else {
            Err(AppError::StaleSchemaRevision {
                expected,
                actual: self.schema_revision,
            })
        }
    }

    // ----- row view (filter ∘ non-destructive sort) ------------------------

    /// Visible row count: the view's count when one is active, else the
    /// full row count.
    pub fn visible_len(&self) -> usize {
        self.filter_view
            .as_ref()
            .map(Vec::len)
            .unwrap_or_else(|| self.n_rows())
    }

    /// The active view's absolute row indices, in DISPLAY order, if any.
    pub fn filter_view(&self) -> Option<&[usize]> {
        self.filter_view.as_deref()
    }

    /// Replace the active filter with a precomputed row set (absolute indices
    /// in source order). An active view sort (F12) is preserved: the new
    /// visible rows are re-ordered by the same keys.
    pub fn set_filter(&mut self, view: Vec<usize>) -> AppResult<()> {
        self.filter_rows = Some(view);
        self.recompose_row_view()
    }

    /// Drop the filter ingredient (a view sort, if any, stays applied).
    pub fn clear_filter(&mut self) -> AppResult<()> {
        // A cleared filter also ends the F19 live "only new rows" range.
        self.follow_range_from = None;
        if self.filter_rows.take().is_some() {
            self.recompose_row_view()?;
        }
        Ok(())
    }

    /// Set (or clear, with empty `keys`) the non-destructive view sort (F12).
    /// Orders the DISPLAY view only: source rows never move, nothing enters
    /// the undo stack, and the document does not become dirty. Available on
    /// read-only (indexed / follow) documents too.
    pub fn set_view_sort(&mut self, keys: Vec<SortKey>) -> AppResult<()> {
        for key in &keys {
            if key.column >= self.headers.len() {
                return Err(AppError::invalid("sort column out of range"));
            }
        }
        self.view_sort = keys;
        self.recompose_row_view()
    }

    /// Whether a non-destructive view sort is active.
    pub fn view_sorted(&self) -> bool {
        !self.view_sort.is_empty()
    }

    /// Drop BOTH row-view ingredients (filter and view sort). Structural
    /// mutations call this from the command layer: they shift the absolute
    /// indices a snapshot view refers to.
    pub fn clear_row_view(&mut self) {
        // Ending the whole row view also ends the F19 live range.
        self.follow_range_from = None;
        let had_view = self.filter_rows.is_some() || !self.view_sort.is_empty();
        self.filter_rows = None;
        self.view_sort.clear();
        self.filter_view = None;
        if had_view {
            // The visible-row set is an input to scoped previews, so changing
            // it must invalidate them.
            self.revision += 1;
            self.filter_revision = self.revision;
        }
    }

    /// Rebuild the composed display→absolute mapping from the two ingredients
    /// and bump the revision (the visible-row set is an input to scoped
    /// previews). The sort extracts only the key columns — streamed for
    /// indexed documents — and is stable, so ties keep source/filter order.
    fn recompose_row_view(&mut self) -> AppResult<()> {
        let composed = if self.view_sort.is_empty() {
            self.filter_rows.clone()
        } else {
            let base: Vec<usize> = match &self.filter_rows {
                Some(rows) => rows.clone(),
                None => (0..self.n_rows()).collect(),
            };
            // Remap the keys onto the extracted key-column layout.
            let key_cols: Vec<usize> = self.view_sort.iter().map(|k| k.column).collect();
            let local_keys: Vec<SortKey> = self
                .view_sort
                .iter()
                .enumerate()
                .map(|(i, k)| SortKey {
                    column: i,
                    descending: k.descending,
                })
                .collect();
            let mut keyed: Vec<(usize, Vec<String>)> = Vec::with_capacity(base.len());
            self.visit_rows_at(&base, &mut |_, row| {
                let keys: Vec<String> = key_cols
                    .iter()
                    .map(|&c| row.get(c).cloned().unwrap_or_default())
                    .collect();
                keyed.push((0, keys));
                Ok(true)
            })?;
            for (slot, &abs) in keyed.iter_mut().zip(base.iter()) {
                slot.0 = abs;
            }
            // Columns with a declared schema (F31) view-sort in typed order.
            let key_schemas = self.key_schemas(&self.view_sort);
            keyed.sort_by(|a, b| {
                crate::sort::compare_rows_schema(&a.1, &b.1, &local_keys, &key_schemas)
            });
            Some(keyed.into_iter().map(|(abs, _)| abs).collect())
        };
        self.filter_view = composed;
        self.revision += 1;
        self.filter_revision = self.revision;
        Ok(())
    }

    /// Translate a visible (display) row index to its absolute index. Identity
    /// when unfiltered; `None` if the display index is past the visible range.
    pub fn display_to_abs(&self, display: usize) -> Option<usize> {
        match &self.filter_view {
            Some(view) => view.get(display).copied(),
            None => (display < self.n_rows()).then_some(display),
        }
    }

    /// Like [`Document::display_to_abs`], but a display index equal to the
    /// visible length maps to the end of the document so a paste/insert at the
    /// bottom can append.
    pub fn display_to_abs_insert(&self, display: usize) -> Option<usize> {
        if display == self.visible_len() {
            return Some(self.n_rows());
        }
        self.display_to_abs(display)
    }

    pub fn is_dirty(&self) -> bool {
        self.undo_stack.len() != self.saved_marker
    }

    /// Cells changed since the last save, in absolute `(row, col)` coordinates
    /// (best-effort — cleared by a save). Used by conditional highlighting
    /// (F42) for the "changed since save" condition.
    pub fn dirty_cells(&self) -> &HashSet<(usize, usize)> {
        &self.dirty_cells
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    #[cfg(test)]
    fn cell(&self, row: usize, col: usize) -> &str {
        &self.rows[row][col]
    }

    // ----- metadata / windowed reads --------------------------------------

    /// A window of rows plus a parallel dirty-flag matrix.
    pub fn get_rows(&self, start: usize, count: usize) -> AppResult<RowsResponse> {
        let visible = self.visible_len();
        let start = start.min(visible);
        let end = start.saturating_add(count).min(visible);
        // Map a display-row index to its absolute index (identity when unfiltered).
        let abs: Vec<usize> = (start..end)
            .map(|display| match &self.filter_view {
                Some(view) => view[display],
                None => display,
            })
            .collect();
        let rows = self.fetch_rows(&abs)?;
        let dirty: Vec<Vec<bool>> = abs
            .iter()
            .map(|&r| {
                (0..self.headers.len())
                    .map(|c| self.dirty_cells.contains(&(r, c)))
                    .collect()
            })
            .collect();
        Ok(RowsResponse { start, rows, dirty })
    }

    /// Aggregate numeric statistics over a rectangular selection (data-row
    /// coordinates). Computed in Rust so it scales to any selection size.
    pub fn selection_stats(&self, rect: CellRect) -> AppResult<SelectionStats> {
        let row_end = rect.y.saturating_add(rect.height).min(self.visible_len());
        let col_end = rect.x.saturating_add(rect.width).min(self.headers.len());

        let mut count = 0usize;
        let mut numeric_count = 0usize;
        let mut sum = 0.0f64;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;

        if rect.y < row_end {
            let mut per_row = |row: &[String]| {
                for c in rect.x..col_end {
                    count += 1;
                    if let Some(n) = row.get(c).and_then(|cell| analyze::as_number(cell)) {
                        numeric_count += 1;
                        sum += n;
                        if n < min {
                            min = n;
                        }
                        if n > max {
                            max = n;
                        }
                    }
                }
            };
            // Resolve display rows to absolute (identity when unfiltered).
            match &self.filter_view {
                None => self.visit_rows(rect.y..row_end, &mut |_, row| {
                    per_row(row);
                    Ok(true)
                })?,
                Some(view) => self.visit_rows_at(&view[rect.y..row_end], &mut |_, row| {
                    per_row(row);
                    Ok(true)
                })?,
            }
        }

        let has_numeric = numeric_count > 0;
        Ok(SelectionStats {
            count,
            numeric_count,
            sum,
            avg: has_numeric.then(|| sum / numeric_count as f64),
            min: has_numeric.then_some(min),
            max: has_numeric.then_some(max),
        })
    }

    /// Detect the type of, and summarise, every column in ONE pass over the
    /// data. Recomputed on demand (no cache, so it can never go stale after
    /// an edit). Editable documents scan every row; indexed documents scan
    /// the first [`INDEXED_SUMMARY_SAMPLE`] rows so the grid's type badges
    /// never trigger a full multi-GB scan (the F05 profiler is the exact tool).
    pub fn column_summaries(&self) -> AppResult<Vec<ColumnSummary>> {
        let mut accs: Vec<SummaryAccumulator> = (0..self.headers.len())
            .map(|_| SummaryAccumulator::default())
            .collect();
        let scan_end = self.summary_scan_len();
        // Flagged on every summary so the UI can say "first N rows" instead
        // of presenting a sample as whole-document statistics.
        let sampled = scan_end < self.n_rows();
        self.visit_rows(0..scan_end, &mut |_, row| {
            for (c, acc) in accs.iter_mut().enumerate() {
                acc.record(row.get(c).map(String::as_str).unwrap_or(""));
            }
            Ok(true)
        })?;
        Ok(accs
            .into_iter()
            .enumerate()
            .map(|(c, acc)| {
                let mut summary = acc.into_summary(c, scan_end, sampled);
                // A DECLARED logical type (F31) beats the heuristic badge.
                if let Some(schema) = self.column_schema_at(c) {
                    summary.kind = crate::schema::column_kind_of(schema.logical_type);
                }
                summary
            })
            .collect())
    }

    /// How many rows the column summaries scan (everything, or a bounded
    /// sample for the indexed backing).
    fn summary_scan_len(&self) -> usize {
        match self.backing {
            Backing::Memory => self.n_rows(),
            Backing::Indexed(_) | Backing::Columnar(_) | Backing::Virtual(_) => {
                self.n_rows().min(INDEXED_SUMMARY_SAMPLE)
            }
        }
    }

    pub fn meta(&self) -> DocumentMeta {
        let file_name = self
            .display_name
            .clone()
            .or_else(|| {
                self.path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
            })
            .or_else(|| {
                // Archive-backed documents (F17) have no path; show where
                // they came from instead of "Untitled".
                self.archive.as_ref().map(|a| {
                    let archive_name = std::path::Path::new(&a.archive_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| a.archive_path.clone());
                    match &a.entry_name {
                        Some(entry) => {
                            let entry_base = entry.rsplit(['/', '\\']).next().unwrap_or(entry);
                            format!("{archive_name} → {entry_base}")
                        }
                        None => archive_name,
                    }
                })
            })
            .unwrap_or_else(|| "Untitled".to_string());

        DocumentMeta {
            id: self.id,
            path: self.path.as_ref().map(|p| p.to_string_lossy().to_string()),
            file_name,
            row_count: self.visible_len(),
            total_row_count: self.n_rows(),
            filtered: self.filter_rows.is_some(),
            col_count: self.headers.len(),
            headers: self.headers.clone(),
            column_ids: self.column_ids.clone(),
            view_sorted: self.view_sorted(),
            has_header_row: self.has_header_row,
            delimiter: String::from_utf8_lossy(&[self.delimiter]).to_string(),
            encoding: self.encoding_name.clone(),
            had_bom: self.had_bom,
            line_ending: self.line_ending.as_str().to_string(),
            dirty: self.is_dirty(),
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            revision: self.revision,
            backing: self.backing_name().to_string(),
            archive: self.archive.clone(),
            follow: self.follow,
        }
    }

    /// Mark the current state as saved (clears the dirty indicator and the
    /// dirty-cell highlights). `path` updates on Save As.
    pub fn mark_saved(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            self.path = Some(p);
            // Save As turns an archive-backed document into an ordinary file
            // document; the extracted temp (if any) is no longer needed.
            self.archive = None;
            self.archive_guard = None;
            // Same for a database-table label (F35): the file name wins now.
            self.display_name = None;
        }
        self.saved_marker = self.undo_stack.len();
        self.dirty_cells.clear();
    }

    // ----- public edit API -------------------------------------------------

    pub fn set_cell(&mut self, row: usize, col: usize, value: String) -> AppResult<()> {
        self.set_cells(vec![(row, col, value)])
    }

    /// Apply a batch of cell changes as a single undoable action.
    pub fn set_cells(&mut self, changes: Vec<(usize, usize, String)>) -> AppResult<()> {
        self.ensure_editable()?;
        for &(row, col, _) in &changes {
            self.check_cell(row, col)?;
        }
        if let Some(op) = self.op_set_cells(changes) {
            self.register(op);
        }
        Ok(())
    }

    pub fn insert_rows(&mut self, at: usize, count: usize) -> AppResult<()> {
        self.ensure_editable()?;
        if at > self.rows.len() {
            return Err(AppError::invalid("row index out of range"));
        }
        if count == 0 {
            return Ok(());
        }
        let op = self.op_insert_rows(at, count);
        self.register(op);
        Ok(())
    }

    pub fn delete_rows(&mut self, mut indices: Vec<usize>) -> AppResult<()> {
        self.ensure_editable()?;
        indices.sort_unstable();
        indices.dedup();
        if let Some(&max) = indices.last() {
            if max >= self.rows.len() {
                return Err(AppError::invalid("row index out of range"));
            }
        } else {
            return Ok(());
        }
        let op = self.op_delete_rows(&indices);
        self.register(op);
        Ok(())
    }

    pub fn move_row(&mut self, from: usize, to: usize) -> AppResult<()> {
        self.ensure_editable()?;
        let n = self.rows.len();
        if from >= n || to >= n {
            return Err(AppError::invalid("row index out of range"));
        }
        if from == to {
            return Ok(());
        }
        let op = EditOp::MoveRow { from, to };
        self.apply(&op);
        self.register(op);
        Ok(())
    }

    pub fn insert_column(&mut self, at: usize, name: String) -> AppResult<()> {
        self.ensure_editable()?;
        if at > self.headers.len() {
            return Err(AppError::invalid("column index out of range"));
        }
        let op = self.op_insert_column(at, name);
        self.register(op);
        Ok(())
    }

    pub fn delete_columns(&mut self, mut indices: Vec<usize>) -> AppResult<()> {
        self.ensure_editable()?;
        indices.sort_unstable();
        indices.dedup();
        if let Some(&max) = indices.last() {
            if max >= self.headers.len() {
                return Err(AppError::invalid("column index out of range"));
            }
        } else {
            return Ok(());
        }
        if indices.len() >= self.headers.len() {
            return Err(AppError::invalid("cannot delete every column"));
        }
        let op = self.op_delete_columns(&indices);
        self.register(op);
        Ok(())
    }

    pub fn rename_column(&mut self, col: usize, name: String) -> AppResult<()> {
        self.ensure_editable()?;
        if col >= self.headers.len() {
            return Err(AppError::invalid("column index out of range"));
        }
        let old = self.headers[col].clone();
        if old == name {
            return Ok(());
        }
        let op = EditOp::RenameColumn {
            col,
            old,
            new: name,
        };
        self.apply(&op);
        self.register(op);
        Ok(())
    }

    pub fn move_column(&mut self, from: usize, to: usize) -> AppResult<()> {
        self.ensure_editable()?;
        let n = self.headers.len();
        if from >= n || to >= n {
            return Err(AppError::invalid("column index out of range"));
        }
        if from == to {
            return Ok(());
        }
        let op = EditOp::MoveColumn { from, to };
        self.apply(&op);
        self.register(op);
        Ok(())
    }

    /// Paste a rectangular block at an anchor, growing the grid as needed. The
    /// whole operation (any growth plus the writes) is a single undo step.
    pub fn paste(
        &mut self,
        anchor_row: usize,
        anchor_col: usize,
        block: Vec<Vec<String>>,
    ) -> AppResult<()> {
        self.ensure_editable()?;
        if block.is_empty() {
            return Ok(());
        }
        if anchor_col >= self.headers.len() {
            return Err(AppError::invalid("column index out of range"));
        }
        // `== len` is allowed so a paste can append at the end.
        if anchor_row > self.rows.len() {
            return Err(AppError::invalid("row index out of range"));
        }
        let block_rows = block.len();
        let block_cols = block.iter().map(|r| r.len()).max().unwrap_or(0);
        if block_cols == 0 {
            return Ok(());
        }

        let needed_rows = anchor_row
            .saturating_add(block_rows)
            .saturating_sub(self.rows.len());
        let needed_cols = anchor_col
            .saturating_add(block_cols)
            .saturating_sub(self.headers.len());

        let mut sub: Vec<EditOp> = Vec::new();
        if needed_rows > 0 {
            let at = self.rows.len();
            sub.push(self.op_insert_rows(at, needed_rows));
        }
        for _ in 0..needed_cols {
            let at = self.headers.len();
            let name = format!("Column {}", at + 1);
            sub.push(self.op_insert_column(at, name));
        }

        let mut changes: Vec<(usize, usize, String)> = Vec::new();
        for (dr, line) in block.into_iter().enumerate() {
            for (dc, value) in line.into_iter().enumerate() {
                changes.push((anchor_row + dr, anchor_col + dc, value));
            }
        }
        if let Some(op) = self.op_set_cells(changes) {
            sub.push(op);
        }

        match sub.len() {
            0 => {}
            1 => self.register(sub.pop().unwrap()),
            _ => self.register(EditOp::Composite(sub)),
        }
        Ok(())
    }

    /// F14 Paste Special: apply a fully transformed block at the anchor as
    /// ONE undoable operation. `first_row_headers` consumes the block's first
    /// row as header renames for the target columns; `skip_blanks` leaves
    /// destination cells untouched where the source cell is blank (overwrite
    /// mode); `InsertRows` mode splices the block in as brand-new rows.
    pub fn paste_special(
        &mut self,
        anchor_row: usize,
        anchor_col: usize,
        mut block: Vec<Vec<String>>,
        mode: crate::paste::PasteMode,
        skip_blanks: bool,
        first_row_headers: bool,
    ) -> AppResult<()> {
        use crate::paste::PasteMode;
        self.ensure_editable()?;
        if anchor_col >= self.headers.len() {
            return Err(AppError::invalid("column index out of range"));
        }
        if anchor_row > self.rows.len() {
            return Err(AppError::invalid("row index out of range"));
        }
        let header_row = if first_row_headers && !block.is_empty() {
            Some(block.remove(0))
        } else {
            None
        };
        let block_rows = block.len();
        let block_cols = block
            .iter()
            .map(Vec::len)
            .chain(header_row.iter().map(Vec::len))
            .max()
            .unwrap_or(0);
        if block_cols == 0 {
            return Ok(());
        }

        let mut sub: Vec<EditOp> = Vec::new();
        let needed_cols = (anchor_col + block_cols).saturating_sub(self.headers.len());
        for _ in 0..needed_cols {
            let at = self.headers.len();
            sub.push(self.op_insert_column(at, format!("Column {}", at + 1)));
        }

        match mode {
            PasteMode::Overwrite => {
                let needed_rows = (anchor_row + block_rows).saturating_sub(self.rows.len());
                if needed_rows > 0 {
                    let at = self.rows.len();
                    sub.push(self.op_insert_rows(at, needed_rows));
                }
                let mut changes: Vec<(usize, usize, String)> = Vec::new();
                for (dr, line) in block.into_iter().enumerate() {
                    for (dc, value) in line.into_iter().enumerate() {
                        if skip_blanks && value.trim().is_empty() {
                            continue;
                        }
                        changes.push((anchor_row + dr, anchor_col + dc, value));
                    }
                }
                if let Some(op) = self.op_set_cells(changes) {
                    sub.push(op);
                }
            }
            PasteMode::InsertRows => {
                if block_rows > 0 {
                    sub.push(self.op_insert_rows(anchor_row, block_rows));
                    let mut changes: Vec<(usize, usize, String)> = Vec::new();
                    for (dr, line) in block.into_iter().enumerate() {
                        for (dc, value) in line.into_iter().enumerate() {
                            if value.is_empty() {
                                continue; // fresh rows are already blank
                            }
                            changes.push((anchor_row + dr, anchor_col + dc, value));
                        }
                    }
                    if let Some(op) = self.op_set_cells(changes) {
                        sub.push(op);
                    }
                }
            }
        }

        if let Some(names) = header_row {
            for (i, name) in names.into_iter().enumerate() {
                let col = anchor_col + i;
                if col < self.headers.len() && !name.is_empty() && self.headers[col] != name {
                    let op = EditOp::RenameColumn {
                        col,
                        old: self.headers[col].clone(),
                        new: name,
                    };
                    self.apply(&op);
                    sub.push(op);
                }
            }
        }

        match sub.len() {
            0 => {}
            1 => self.register(sub.pop().expect("checked length")),
            _ => self.register(EditOp::Composite(sub)),
        }
        Ok(())
    }

    /// Replace a set of columns with freshly filled ones in ONE undoable
    /// operation (the split/merge transforms): removes `remove`, inserts the
    /// new columns at `insert_at` (a position in the post-removal layout) and
    /// fills their values. The grid stays rectangular throughout, and a single
    /// undo restores headers, values and structure.
    pub fn replace_columns(
        &mut self,
        mut remove: Vec<usize>,
        insert_at: usize,
        new_columns: Vec<(String, Vec<String>)>,
    ) -> AppResult<()> {
        self.ensure_editable()?;
        remove.sort_unstable();
        remove.dedup();
        if let Some(&max) = remove.last() {
            if max >= self.headers.len() {
                return Err(AppError::invalid("column index out of range"));
            }
        }
        if remove.len() >= self.headers.len() && new_columns.is_empty() {
            return Err(AppError::invalid("cannot delete every column"));
        }
        let n_rows = self.rows.len();
        for (_, values) in &new_columns {
            if values.len() != n_rows {
                return Err(AppError::invalid("replacement column has wrong row count"));
            }
        }
        if insert_at > self.headers.len() - remove.len() {
            return Err(AppError::invalid("column index out of range"));
        }

        let mut sub: Vec<EditOp> = Vec::new();
        if !remove.is_empty() {
            sub.push(self.op_delete_columns(&remove));
        }
        let mut changes: Vec<(usize, usize, String)> = Vec::new();
        for (i, (header, values)) in new_columns.into_iter().enumerate() {
            let at = insert_at + i;
            sub.push(self.op_insert_column(at, header));
            for (r, value) in values.into_iter().enumerate() {
                if !value.is_empty() {
                    changes.push((r, at, value));
                }
            }
        }
        if let Some(op) = self.op_set_cells(changes) {
            sub.push(op);
        }
        match sub.len() {
            0 => {}
            1 => self.register(sub.pop().expect("checked length")),
            _ => self.register(EditOp::Composite(sub)),
        }
        Ok(())
    }

    /// Sort rows by one or more keys. Empty `keys` is a no-op.
    pub fn sort(&mut self, keys: &[SortKey]) -> AppResult<()> {
        self.ensure_editable()?;
        if keys.is_empty() || self.rows.len() < 2 {
            return Ok(());
        }
        for key in keys {
            if key.column >= self.headers.len() {
                return Err(AppError::invalid("sort column out of range"));
            }
        }

        // Columns with a declared schema (F31) sort in typed order.
        let key_schemas = self.key_schemas(keys);
        let mut order: Vec<u32> = (0..self.rows.len() as u32).collect();
        order.sort_by(|&a, &b| {
            crate::sort::compare_rows_schema(
                &self.rows[a as usize],
                &self.rows[b as usize],
                keys,
                &key_schemas,
            )
        });

        // No-op if already sorted.
        if order.iter().enumerate().all(|(i, &o)| i as u32 == o) {
            return Ok(());
        }

        let op = EditOp::SortRows { order };
        self.apply(&op);
        self.register(op);
        Ok(())
    }

    /// Toggle whether the first row is treated as a header. This re-interprets
    /// the data, so it clears the undo history and dirty highlights.
    pub fn set_header_mode(&mut self, has_header: bool) -> AppResult<()> {
        self.ensure_editable()?;
        if has_header == self.has_header_row {
            return Ok(());
        }
        if has_header {
            if !self.rows.is_empty() {
                self.headers = self.rows.remove(0);
            }
            self.has_header_row = true;
        } else {
            let demoted = std::mem::take(&mut self.headers);
            let cols = demoted.len();
            self.rows.insert(0, demoted);
            self.headers = (0..cols).map(|i| format!("Column {}", i + 1)).collect();
            self.has_header_row = false;
        }
        // Re-interpretation invalidates index-based history. Force the dirty
        // indicator on (saved_marker can never equal the empty stack length).
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_meta.clear();
        self.redo_meta.clear();
        // A journal against the OLD interpretation must not survive: its
        // replay would target coordinates that no longer exist.
        if let Some(journal) = self.journal.take() {
            journal.delete();
        }
        self.dirty_cells.clear();
        self.saved_marker = usize::MAX;
        self.revision += 1;
        self.touch_all_columns();
        Ok(())
    }

    // ----- change inspector / selective revert (F15) ------------------------

    /// The first undo-stack index that is NOT covered by the last save.
    fn unsaved_start(&self) -> usize {
        if self.saved_marker == usize::MAX {
            0
        } else {
            self.saved_marker.min(self.undo_stack.len())
        }
    }

    /// True when the user undid PAST the last save: the document is dirty,
    /// but the saved state lives ahead in the REDO branch (redo returns to
    /// it) — there are no "changes since save" to list, and the panel must
    /// say so instead of showing an empty, misleading list.
    pub fn saved_in_redo_branch(&self) -> bool {
        self.saved_marker != usize::MAX && self.saved_marker > self.undo_stack.len()
    }

    /// Whether an op touches only cell VALUES (never structure). Reverting
    /// anything is only safe while every LATER op is cell-only, because
    /// cell ops never move rows or columns.
    fn op_is_cell_only(op: &EditOp) -> bool {
        match op {
            EditOp::SetCells(_) => true,
            EditOp::Composite(ops) => ops.iter().all(Self::op_is_cell_only),
            EditOp::Inverse(inner) => Self::op_is_cell_only(inner),
            _ => false,
        }
    }

    fn ops_after_are_cell_only(&self, index: usize) -> bool {
        self.undo_stack[index + 1..]
            .iter()
            .all(Self::op_is_cell_only)
    }

    /// Rows/columns whose EXISTENCE is created by APPLYING `op` (recursing
    /// composites; an `Inverse` flips direction, so reverting a delete also
    /// counts as inserting). Coordinates are in the op's own final layout —
    /// valid against the current document while every later op is cell-only.
    fn op_inserted_coords(
        op: &EditOp,
        rows: &mut std::collections::HashSet<usize>,
        cols: &mut std::collections::HashSet<usize>,
        reverted: bool,
    ) {
        match op {
            EditOp::InsertRows { at, count } if !reverted => rows.extend(*at..*at + *count),
            EditOp::InsertColumn { at, .. } if !reverted => {
                cols.insert(*at);
            }
            EditOp::DeleteRows { removed } if reverted => {
                rows.extend(removed.iter().map(|(i, _)| *i));
            }
            EditOp::DeleteColumns { removed } if reverted => {
                cols.extend(removed.iter().map(|c| c.index));
            }
            EditOp::Composite(ops) => {
                for sub in ops {
                    Self::op_inserted_coords(sub, rows, cols, reverted);
                }
            }
            EditOp::Inverse(inner) => Self::op_inserted_coords(inner, rows, cols, !reverted),
            _ => {}
        }
    }

    /// Why reverting the op at `index` is not allowed right now (`None` =
    /// allowed). Beyond the later-structural gate, an op that INSERTED
    /// structure cannot be inverse-reverted while the CURRENT contents of
    /// that structure differ from what the op itself left there — undoing
    /// the revert re-applies the op, which restores the op's own values and
    /// would silently lose the later edits. (Plain single inserts are
    /// exempt: they revert as capturing deletes instead.) The check is
    /// state-based on purpose: an edit made and then reverted back leaves
    /// matching contents, so it never blocks.
    fn revert_block_reason(&self, index: usize) -> Option<String> {
        if !self.ops_after_are_cell_only(index) {
            return Some(
                "later structural changes depend on this one — use Revert all".to_string(),
            );
        }
        if matches!(
            self.undo_stack[index],
            EditOp::InsertRows { .. } | EditOp::InsertColumn { .. }
        ) {
            return None; // capture-as-delete keeps later edits recoverable
        }
        let mut ins_rows = std::collections::HashSet::new();
        let mut ins_cols = std::collections::HashSet::new();
        Self::op_inserted_coords(&self.undo_stack[index], &mut ins_rows, &mut ins_cols, false);
        if ins_rows.is_empty() && ins_cols.is_empty() {
            return None;
        }
        // What the op's own re-apply would put inside the inserted
        // structure: its trailing cell edits, blank otherwise.
        let mut own_edits = Vec::new();
        Self::collect_cell_edits(&self.undo_stack[index], false, &mut own_edits);
        let mut own: std::collections::HashMap<(usize, usize), &str> =
            std::collections::HashMap::new();
        for e in &own_edits {
            own.insert((e.row, e.col), e.new.as_str()); // in-order: last wins
        }
        let cell = |r: usize, c: usize| -> &str {
            self.rows
                .get(r)
                .and_then(|row| row.get(c))
                .map(String::as_str)
                .unwrap_or("")
        };
        let matches_own =
            |r: usize, c: usize| -> bool { cell(r, c) == own.get(&(r, c)).copied().unwrap_or("") };
        let clean = ins_rows
            .iter()
            .all(|&r| (0..self.headers.len()).all(|c| matches_own(r, c)))
            && ins_cols
                .iter()
                .all(|&c| (0..self.rows.len()).all(|r| matches_own(r, c)));
        if !clean {
            return Some(
                "the content inside the structure this change added was edited \
                 afterwards — revert those cells first, or use Revert all"
                    .to_string(),
            );
        }
        None
    }

    /// Collect an op's cell edits (recursing into composites; an Inverse
    /// swaps old/new, because applying it restores the OLD values).
    fn collect_cell_edits(op: &EditOp, swapped: bool, out: &mut Vec<CellChange>) {
        match op {
            EditOp::SetCells(edits) => {
                for e in edits {
                    let (old, new) = if swapped {
                        (e.new.clone(), e.old.clone())
                    } else {
                        (e.old.clone(), e.new.clone())
                    };
                    out.push(CellChange {
                        row: e.row,
                        col: e.col,
                        old,
                        new,
                    });
                }
            }
            EditOp::Composite(ops) => {
                for sub in ops {
                    Self::collect_cell_edits(sub, swapped, out);
                }
            }
            EditOp::Inverse(inner) => Self::collect_cell_edits(inner, !swapped, out),
            _ => {}
        }
    }

    fn op_kind(op: &EditOp) -> &'static str {
        match op {
            EditOp::SetCells(_) => "cellEdits",
            EditOp::InsertRows { .. } => "insertRows",
            EditOp::DeleteRows { .. } => "deleteRows",
            EditOp::MoveRow { .. } => "moveRow",
            EditOp::InsertColumn { .. } => "insertColumn",
            EditOp::DeleteColumns { .. } => "deleteColumns",
            EditOp::RenameColumn { .. } => "renameColumn",
            EditOp::MoveColumn { .. } => "moveColumn",
            EditOp::SortRows { .. } => "sortRows",
            EditOp::Composite(_) => "composite",
            EditOp::Inverse(_) => "revert",
        }
    }

    /// Everything changed since the last successful save, oldest first.
    /// Saving clears this list by moving `saved_marker` to the stack top.
    pub fn changes_since_save(&self) -> Vec<ChangeSummary> {
        const SAMPLE: usize = 10;
        let start = self.unsaved_start();
        self.undo_stack[start..]
            .iter()
            .zip(self.undo_meta[start.min(self.undo_meta.len())..].iter())
            .enumerate()
            .map(|(offset, (op, meta))| {
                let index = start + offset;
                let mut cells = Vec::new();
                Self::collect_cell_edits(op, false, &mut cells);
                let structural = !Self::op_is_cell_only(op);
                let blocked_reason = self.revert_block_reason(index);
                ChangeSummary {
                    id: meta.id,
                    epoch_secs: meta.epoch_secs,
                    kind: Self::op_kind(op).to_string(),
                    cells_affected: cells.len(),
                    sample: cells.into_iter().take(SAMPLE).collect(),
                    structural,
                    revertible: blocked_reason.is_none(),
                    blocked_reason,
                }
            })
            .collect()
    }

    fn unsaved_index_of(&self, op_id: u64) -> AppResult<usize> {
        let start = self.unsaved_start();
        self.undo_meta
            .iter()
            .position(|m| m.id == op_id)
            .filter(|&i| i >= start)
            .ok_or_else(|| AppError::invalid("that change is no longer in the unsaved history"))
    }

    /// Revert ONE whole operation as a NEW, undoable operation. Allowed only
    /// while every later operation is cell-only (so the inverse's row and
    /// column coordinates are still valid) and never at the cost of later
    /// edits living inside inserted structure (see `revert_block_reason`).
    pub fn revert_stack_op(&mut self, op_id: u64) -> AppResult<()> {
        self.ensure_editable()?;
        let index = self.unsaved_index_of(op_id)?;
        if let Some(reason) = self.revert_block_reason(index) {
            return Err(AppError::invalid(reason));
        }
        match self.undo_stack[index].clone() {
            // Reverting a plain insert REMOVES structure that later cell
            // edits may have filled. Register the revert as an ordinary
            // DELETE op: it captures the CURRENT contents, so undoing the
            // revert restores the later edits exactly instead of blank rows.
            EditOp::InsertRows { at, count } => {
                let rows: Vec<usize> = (at..at + count).collect();
                let op = self.op_delete_rows(&rows);
                self.register(op);
            }
            EditOp::InsertColumn { at, .. } => {
                let op = self.op_delete_columns(&[at]);
                self.register(op);
            }
            target => {
                let inverse = EditOp::Inverse(Box::new(target));
                self.apply(&inverse);
                self.register(inverse);
            }
        }
        Ok(())
    }

    /// Revert SPECIFIC cells of one cell-edit operation (back to that
    /// operation's old values) as an ordinary, undoable cell edit.
    pub fn revert_cells_of_op(&mut self, op_id: u64, cells: &[(usize, usize)]) -> AppResult<()> {
        self.ensure_editable()?;
        let index = self.unsaved_index_of(op_id)?;
        if !self.ops_after_are_cell_only(index) {
            return Err(AppError::invalid(
                "later structural changes moved these cells — use Revert all",
            ));
        }
        let mut edits = Vec::new();
        Self::collect_cell_edits(&self.undo_stack[index], false, &mut edits);
        let changes: Vec<(usize, usize, String)> = edits
            .into_iter()
            .filter(|e| cells.contains(&(e.row, e.col)))
            .map(|e| (e.row, e.col, e.old))
            .collect();
        if changes.is_empty() {
            return Err(AppError::invalid("those cells are not part of that change"));
        }
        self.set_cells(changes)
    }

    /// Revert every unsaved cell edit in one column back to its value at
    /// the last save. Allowed only while the whole unsaved range is
    /// cell-only.
    pub fn revert_column_edits(&mut self, col: usize) -> AppResult<()> {
        self.ensure_editable()?;
        if col >= self.headers.len() {
            return Err(AppError::invalid("column out of range"));
        }
        let start = self.unsaved_start();
        if !self.undo_stack[start..].iter().all(Self::op_is_cell_only) {
            return Err(AppError::invalid(
                "structural changes exist since the last save — use Revert all",
            ));
        }
        // Oldest-first: the first old value we see per row is the value at
        // the last save.
        let mut oldest: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
        for op in &self.undo_stack[start..] {
            let mut edits = Vec::new();
            Self::collect_cell_edits(op, false, &mut edits);
            for e in edits {
                if e.col == col {
                    oldest.entry(e.row).or_insert(e.old);
                }
            }
        }
        if oldest.is_empty() {
            return Err(AppError::invalid("no unsaved edits in that column"));
        }
        let mut changes: Vec<(usize, usize, String)> = oldest
            .into_iter()
            .filter(|(row, _)| *row < self.rows.len())
            .map(|(row, old)| (row, col, old))
            .collect();
        changes.sort_by_key(|(row, _, _)| *row);
        self.set_cells(changes)
    }

    /// Revert EVERYTHING since the last save as ONE undoable operation —
    /// the sequential inverse of every unsaved op, newest first. Always
    /// safe: each inverse applies to exactly the state its op produced.
    pub fn revert_all_changes(&mut self) -> AppResult<()> {
        self.ensure_editable()?;
        let start = self.unsaved_start();
        if start >= self.undo_stack.len() {
            return Err(AppError::invalid("nothing to revert"));
        }
        let inverses: Vec<EditOp> = self.undo_stack[start..]
            .iter()
            .rev()
            .map(|op| EditOp::Inverse(Box::new(op.clone())))
            .collect();
        let composite = EditOp::Composite(inverses);
        self.apply(&composite);
        self.register(composite);
        Ok(())
    }

    pub fn undo(&mut self) -> AppResult<()> {
        let op = self.undo_stack.pop().ok_or(AppError::NothingToUndo)?;
        if let Some(meta) = self.undo_meta.pop() {
            self.redo_meta.push(meta);
        }
        self.revert(&op);
        self.revision += 1;
        self.stamp_touched(&op);
        self.redo_stack.push(op);
        if let Some(journal) = &mut self.journal {
            journal.append(&crate::journal::JournalRecord::Undo);
        }
        Ok(())
    }

    pub fn redo(&mut self) -> AppResult<()> {
        let op = self.redo_stack.pop().ok_or(AppError::NothingToRedo)?;
        if let Some(meta) = self.redo_meta.pop() {
            self.undo_meta.push(meta);
        }
        self.apply(&op);
        self.revision += 1;
        self.stamp_touched(&op);
        self.undo_stack.push(op);
        if let Some(journal) = &mut self.journal {
            journal.append(&crate::journal::JournalRecord::Redo);
        }
        Ok(())
    }

    // ----- crash-recovery journaling (F16) ----------------------------------

    /// Attach a journal; every subsequent operation is appended to it.
    pub fn attach_journal(&mut self, journal: crate::journal::JournalWriter) {
        self.journal = Some(journal);
    }

    /// Detach the journal (close/reparse paths delete the file afterwards).
    pub fn take_journal(&mut self) -> Option<crate::journal::JournalWriter> {
        self.journal.take()
    }

    /// The header describing this document's CURRENT baseline.
    pub fn journal_header(&self) -> crate::journal::JournalHeader {
        crate::journal::JournalHeader {
            version: crate::journal::JOURNAL_VERSION,
            path: self
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            fingerprint: self.fingerprint,
            delimiter: (self.delimiter as char).to_string(),
            encoding: self.encoding_name.clone(),
            has_header_row: self.has_header_row,
            base_revision: self.revision,
        }
    }

    /// After a successful save the journal restarts against the new
    /// baseline (this is also its compaction step).
    pub fn reset_journal_baseline(&mut self) {
        let header = self.journal_header();
        if let Some(journal) = &mut self.journal {
            let _ = journal.reset(&header);
        }
    }

    /// Bounds-check an op against simulated dimensions, advancing them the
    /// way applying the op would. Keeps a corrupt or mismatched journal
    /// from panicking the replay.
    fn op_bounds_ok(op: &EditOp, rows: &mut usize, cols: &mut usize, inverse: bool) -> bool {
        match op {
            EditOp::SetCells(edits) => edits.iter().all(|e| e.row < *rows && e.col < *cols),
            EditOp::InsertRows { at, count } => {
                if inverse {
                    if *at + *count > *rows {
                        return false;
                    }
                    *rows -= count;
                } else {
                    if *at > *rows {
                        return false;
                    }
                    *rows += count;
                }
                true
            }
            EditOp::DeleteRows { removed } => {
                // The captured rows must still FIT the document: replaying
                // (or undoing) a delete whose rows have a different width
                // would re-insert ragged rows and break the rectangular
                // invariant — e.g. a journal recovered against a source
                // whose column count changed.
                if removed.iter().any(|(_, row)| row.len() != *cols) {
                    return false;
                }
                if inverse {
                    if removed.iter().any(|(i, _)| *i > *rows) {
                        return false;
                    }
                    *rows += removed.len();
                } else {
                    if removed.iter().any(|(i, _)| *i >= *rows) {
                        return false;
                    }
                    *rows -= removed.len();
                }
                true
            }
            EditOp::MoveRow { from, to } => *from < *rows && *to < *rows,
            EditOp::InsertColumn { at, .. } => {
                if inverse {
                    if *at >= *cols {
                        return false;
                    }
                    *cols -= 1;
                } else {
                    if *at > *cols {
                        return false;
                    }
                    *cols += 1;
                }
                true
            }
            EditOp::DeleteColumns { removed } => {
                if inverse {
                    if removed
                        .iter()
                        .any(|c| c.index > *cols || c.values.len() != *rows)
                    {
                        return false;
                    }
                    *cols += removed.len();
                } else {
                    if removed.iter().any(|c| c.index >= *cols) {
                        return false;
                    }
                    *cols -= removed.len();
                }
                true
            }
            EditOp::RenameColumn { col, .. } => *col < *cols,
            EditOp::MoveColumn { from, to } => *from < *cols && *to < *cols,
            EditOp::SortRows { order } => {
                order.len() == *rows && order.iter().all(|&i| (i as usize) < *rows)
            }
            EditOp::Composite(ops) => {
                if inverse {
                    ops.iter()
                        .rev()
                        .all(|sub| Self::op_bounds_ok(sub, rows, cols, true))
                } else {
                    ops.iter()
                        .all(|sub| Self::op_bounds_ok(sub, rows, cols, false))
                }
            }
            EditOp::Inverse(inner) => Self::op_bounds_ok(inner, rows, cols, !inverse),
        }
    }

    /// Replay journal records (F16 recovery) in their original order,
    /// registering each as an ordinary operation — so the recovered
    /// document is dirty, fully undoable, and journals onward if a journal
    /// is attached. Returns how many operations were applied.
    pub fn replay_journal_records(
        &mut self,
        records: &[crate::journal::JournalRecord],
    ) -> AppResult<usize> {
        self.ensure_editable()?;
        let mut applied = 0usize;
        for record in records {
            match record {
                crate::journal::JournalRecord::Op { op } => {
                    let op: EditOp = serde_json::from_value(op.clone())
                        .map_err(|_| AppError::invalid("a journal operation no longer parses"))?;
                    let mut rows = self.rows.len();
                    let mut cols = self.headers.len();
                    if !Self::op_bounds_ok(&op, &mut rows, &mut cols, false) {
                        return Err(AppError::invalid(
                            "the journal does not match the file — recover as a \
                             copy or discard it",
                        ));
                    }
                    self.apply(&op);
                    self.register(op);
                    applied += 1;
                }
                crate::journal::JournalRecord::Undo => self.undo()?,
                crate::journal::JournalRecord::Redo => self.redo()?,
            }
        }
        Ok(applied)
    }

    // ----- helpers: build + apply a fresh op, returning it (no stack push) --

    fn register(&mut self, op: EditOp) {
        // If the saved state lived in the redo branch we're about to discard,
        // it becomes permanently unreachable — so the document is dirty until the
        // next save. Without this, `undo` (which shortens the stack) followed by a
        // new edit can make `undo_stack.len()` coincide with `saved_marker` again
        // and falsely report a clean document.
        if self.saved_marker > self.undo_stack.len() {
            self.saved_marker = usize::MAX;
        }
        self.revision += 1;
        self.stamp_touched(&op);
        self.next_op_id += 1;
        self.undo_meta.push(OpMeta {
            id: self.next_op_id,
            epoch_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
        if let Some(journal) = &mut self.journal {
            if let Ok(value) = serde_json::to_value(&op) {
                journal.append(&crate::journal::JournalRecord::Op { op: value });
            }
        }
        self.undo_stack.push(op);
        self.redo_stack.clear();
        self.redo_meta.clear();
    }

    /// Record which columns' DATA the operation changed, at the (freshly
    /// bumped) current revision. Pure reorderings (row moves, sorts) leave a
    /// column's value multiset intact, so they deliberately touch nothing —
    /// per-column profiles stay valid across them.
    fn stamp_touched(&mut self, op: &EditOp) {
        self.stamp_op(op);
        // Column-structure ops change the width; keep the vector aligned (any
        // such op also touched every column, so the fill value is fresh).
        if self.col_revisions.len() != self.headers.len() {
            let rev = self.revision;
            self.col_revisions.resize(self.headers.len(), rev);
        }
        // Column IDs are maintained inside apply/revert for every structural
        // op, so they can never drift from the headers.
        debug_assert_eq!(self.column_ids.len(), self.headers.len());
    }

    fn stamp_op(&mut self, op: &EditOp) {
        let rev = self.revision;
        match op {
            EditOp::SetCells(edits) => {
                for e in edits {
                    if let Some(r) = self.col_revisions.get_mut(e.col) {
                        *r = rev;
                    }
                }
            }
            EditOp::RenameColumn { col, .. } => {
                if let Some(r) = self.col_revisions.get_mut(*col) {
                    *r = rev;
                }
            }
            EditOp::MoveRow { .. } | EditOp::SortRows { .. } => {}
            EditOp::InsertRows { .. } | EditOp::DeleteRows { .. } => self.touch_all_columns(),
            EditOp::InsertColumn { .. }
            | EditOp::DeleteColumns { .. }
            | EditOp::MoveColumn { .. } => self.touch_all_columns(),
            EditOp::Composite(ops) => {
                for sub in ops {
                    self.stamp_op(sub);
                }
            }
            EditOp::Inverse(inner) => self.stamp_op(inner),
        }
    }

    fn touch_all_columns(&mut self) {
        let rev = self.revision;
        for r in &mut self.col_revisions {
            *r = rev;
        }
    }

    fn check_cell(&self, row: usize, col: usize) -> AppResult<()> {
        if row >= self.rows.len() || col >= self.headers.len() {
            return Err(AppError::invalid("cell index out of range"));
        }
        Ok(())
    }

    fn op_set_cells(&mut self, changes: Vec<(usize, usize, String)>) -> Option<EditOp> {
        let mut edits: Vec<CellEdit> = Vec::new();
        for (row, col, new) in changes {
            let old = self.rows[row][col].clone();
            if old != new {
                edits.push(CellEdit { row, col, old, new });
            }
        }
        if edits.is_empty() {
            return None;
        }
        let op = EditOp::SetCells(edits);
        self.apply(&op);
        Some(op)
    }

    fn op_insert_rows(&mut self, at: usize, count: usize) -> EditOp {
        let op = EditOp::InsertRows { at, count };
        self.apply(&op);
        op
    }

    fn op_delete_rows(&mut self, indices: &[usize]) -> EditOp {
        let removed: Vec<(usize, Vec<String>)> =
            indices.iter().map(|&i| (i, self.rows[i].clone())).collect();
        let op = EditOp::DeleteRows { removed };
        self.apply(&op);
        op
    }

    /// Mint a fresh stable column ID (F12). Never reused within a session.
    fn fresh_column_id(&mut self) -> String {
        let id = format!("c{}", self.next_column_id);
        self.next_column_id += 1;
        id
    }

    fn op_insert_column(&mut self, at: usize, name: String) -> EditOp {
        let id = self.fresh_column_id();
        let op = EditOp::InsertColumn { at, name, id };
        self.apply(&op);
        op
    }

    fn op_delete_columns(&mut self, indices: &[usize]) -> EditOp {
        let removed: Vec<RemovedColumn> = indices
            .iter()
            .map(|&i| RemovedColumn {
                index: i,
                header: self.headers[i].clone(),
                values: self.rows.iter().map(|r| r[i].clone()).collect(),
                id: self.column_ids[i].clone(),
            })
            .collect();
        let op = EditOp::DeleteColumns { removed };
        self.apply(&op);
        op
    }

    // ----- apply / revert --------------------------------------------------

    fn apply(&mut self, op: &EditOp) {
        match op {
            EditOp::SetCells(edits) => {
                for e in edits {
                    self.rows[e.row][e.col] = e.new.clone();
                    self.dirty_cells.insert((e.row, e.col));
                }
            }
            EditOp::InsertRows { at, count } => {
                let blank = vec![String::new(); self.headers.len()];
                self.rows.splice(at..at, std::iter::repeat_n(blank, *count));
                self.remap_dirty_rows_inserted(*at, *count);
            }
            EditOp::DeleteRows { removed } => {
                let indices: Vec<usize> = removed.iter().map(|(i, _)| *i).collect();
                for &i in indices.iter().rev() {
                    self.rows.remove(i);
                }
                self.remap_dirty_rows_removed(&indices);
            }
            EditOp::MoveRow { from, to } => {
                let row = self.rows.remove(*from);
                self.rows.insert(*to, row);
                self.remap_dirty_row_moved(*from, *to);
            }
            EditOp::InsertColumn { at, name, id } => {
                self.headers.insert(*at, name.clone());
                self.column_ids.insert(*at, id.clone());
                for row in &mut self.rows {
                    row.insert(*at, String::new());
                }
                self.remap_dirty_cols_inserted(*at, 1);
            }
            EditOp::DeleteColumns { removed } => {
                let indices: Vec<usize> = removed.iter().map(|c| c.index).collect();
                for &i in indices.iter().rev() {
                    self.headers.remove(i);
                    self.column_ids.remove(i);
                    for row in &mut self.rows {
                        row.remove(i);
                    }
                }
                self.remap_dirty_cols_removed(&indices);
            }
            EditOp::RenameColumn { col, new, .. } => {
                self.headers[*col] = new.clone();
            }
            EditOp::MoveColumn { from, to } => {
                let header = self.headers.remove(*from);
                self.headers.insert(*to, header);
                let id = self.column_ids.remove(*from);
                self.column_ids.insert(*to, id);
                for row in &mut self.rows {
                    let cell = row.remove(*from);
                    row.insert(*to, cell);
                }
                self.remap_dirty_col_moved(*from, *to);
            }
            EditOp::SortRows { order } => {
                self.reorder_rows(order);
                self.remap_dirty_rows_reordered(order);
            }
            EditOp::Composite(ops) => {
                for sub in ops {
                    self.apply(sub);
                }
            }
            EditOp::Inverse(inner) => self.revert(inner),
        }
    }

    fn revert(&mut self, op: &EditOp) {
        match op {
            EditOp::SetCells(edits) => {
                for e in edits {
                    self.rows[e.row][e.col] = e.old.clone();
                    self.dirty_cells.remove(&(e.row, e.col));
                }
            }
            EditOp::InsertRows { at, count } => {
                for _ in 0..*count {
                    self.rows.remove(*at);
                }
                self.remap_dirty_rows_removed(&(*at..*at + *count).collect::<Vec<_>>());
            }
            EditOp::DeleteRows { removed } => {
                for (i, row) in removed.iter() {
                    self.rows.insert(*i, row.clone());
                }
                let indices: Vec<usize> = removed.iter().map(|(i, _)| *i).collect();
                self.remap_dirty_rows_reinserted(&indices);
            }
            EditOp::MoveRow { from, to } => {
                let row = self.rows.remove(*to);
                self.rows.insert(*from, row);
                self.remap_dirty_row_moved(*to, *from);
            }
            EditOp::InsertColumn { at, .. } => {
                self.headers.remove(*at);
                self.column_ids.remove(*at);
                for row in &mut self.rows {
                    row.remove(*at);
                }
                self.remap_dirty_cols_removed(&[*at]);
            }
            EditOp::DeleteColumns { removed } => {
                for col in removed.iter() {
                    self.headers.insert(col.index, col.header.clone());
                    self.column_ids.insert(col.index, col.id.clone());
                    for (r, row) in self.rows.iter_mut().enumerate() {
                        row.insert(col.index, col.values[r].clone());
                    }
                }
                let indices: Vec<usize> = removed.iter().map(|c| c.index).collect();
                self.remap_dirty_cols_reinserted(&indices);
            }
            EditOp::RenameColumn { col, old, .. } => {
                self.headers[*col] = old.clone();
            }
            EditOp::MoveColumn { from, to } => {
                let header = self.headers.remove(*to);
                self.headers.insert(*from, header);
                let id = self.column_ids.remove(*to);
                self.column_ids.insert(*from, id);
                for row in &mut self.rows {
                    let cell = row.remove(*to);
                    row.insert(*from, cell);
                }
                self.remap_dirty_col_moved(*to, *from);
            }
            EditOp::SortRows { order } => {
                let inverse = invert_permutation(order);
                self.reorder_rows(&inverse);
                self.remap_dirty_rows_reordered(&inverse);
            }
            EditOp::Composite(ops) => {
                for sub in ops.iter().rev() {
                    self.revert(sub);
                }
            }
            EditOp::Inverse(inner) => self.apply(inner),
        }
    }

    fn reorder_rows(&mut self, order: &[u32]) {
        let mut slots: Vec<Option<Vec<String>>> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(Some)
            .collect();
        let mut new_rows: Vec<Vec<String>> = Vec::with_capacity(slots.len());
        for &o in order {
            new_rows.push(
                slots[o as usize]
                    .take()
                    .expect("permutation is a bijection"),
            );
        }
        self.rows = new_rows;
    }

    // ----- dirty-cell remapping (keeps highlights aligned with edits) ------

    fn rebuild_dirty<F>(&mut self, mut f: F)
    where
        F: FnMut(usize, usize) -> Option<(usize, usize)>,
    {
        let old = std::mem::take(&mut self.dirty_cells);
        self.dirty_cells = old.into_iter().filter_map(|(r, c)| f(r, c)).collect();
    }

    fn remap_dirty_rows_inserted(&mut self, at: usize, count: usize) {
        self.rebuild_dirty(|r, c| Some((if r >= at { r + count } else { r }, c)));
    }

    fn remap_dirty_rows_removed(&mut self, removed_sorted: &[usize]) {
        let set: HashSet<usize> = removed_sorted.iter().copied().collect();
        self.rebuild_dirty(|r, c| {
            if set.contains(&r) {
                None
            } else {
                let shift = removed_sorted.iter().filter(|&&i| i < r).count();
                Some((r - shift, c))
            }
        });
    }

    fn remap_dirty_rows_reinserted(&mut self, inserted_sorted: &[usize]) {
        // Surviving rows refill the final positions that are not reinserted, in
        // order; the k-th such position is where post-delete row k lands.
        let total = self.rows.len();
        let inserted: HashSet<usize> = inserted_sorted.iter().copied().collect();
        let final_positions: Vec<usize> = (0..total).filter(|i| !inserted.contains(i)).collect();
        self.rebuild_dirty(move |r, c| final_positions.get(r).map(|&fr| (fr, c)));
    }

    fn remap_dirty_row_moved(&mut self, from: usize, to: usize) {
        self.rebuild_dirty(|r, c| Some((moved_index(r, from, to), c)));
    }

    fn remap_dirty_rows_reordered(&mut self, order: &[u32]) {
        // `order[new] = old`, so the inverse maps old row -> new row.
        let inverse = invert_permutation(order);
        self.rebuild_dirty(|r, c| inverse.get(r).map(|&nr| (nr as usize, c)));
    }

    fn remap_dirty_cols_inserted(&mut self, at: usize, count: usize) {
        self.rebuild_dirty(|r, c| Some((r, if c >= at { c + count } else { c })));
    }

    fn remap_dirty_cols_removed(&mut self, removed_sorted: &[usize]) {
        let set: HashSet<usize> = removed_sorted.iter().copied().collect();
        self.rebuild_dirty(|r, c| {
            if set.contains(&c) {
                None
            } else {
                let shift = removed_sorted.iter().filter(|&&i| i < c).count();
                Some((r, c - shift))
            }
        });
    }

    fn remap_dirty_cols_reinserted(&mut self, inserted_sorted: &[usize]) {
        let total = self.headers.len();
        let inserted: HashSet<usize> = inserted_sorted.iter().copied().collect();
        let final_positions: Vec<usize> = (0..total).filter(|i| !inserted.contains(i)).collect();
        self.rebuild_dirty(move |r, c| final_positions.get(c).map(|&fc| (r, fc)));
    }

    fn remap_dirty_col_moved(&mut self, from: usize, to: usize) {
        self.rebuild_dirty(|r, c| Some((r, moved_index(c, from, to))));
    }
}

/// Where index `i` lands after moving the element at `from` to `to`.
fn moved_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if from > to && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

/// Invert a permutation where `order[new] = old`, yielding `inverse[old] = new`.
fn invert_permutation(order: &[u32]) -> Vec<u32> {
    let mut inverse = vec![0u32; order.len()];
    for (new_pos, &old_pos) in order.iter().enumerate() {
        inverse[old_pos as usize] = new_pos as u32;
    }
    inverse
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    #[test]
    fn header_split() {
        let d = doc_from("name,age\nAda,36\nBob,40", true);
        assert_eq!(d.headers(), &["name", "age"]);
        assert_eq!(d.n_rows(), 2);
        assert_eq!(d.cell(0, 0), "Ada");
    }

    #[test]
    fn synthetic_headers_without_header_row() {
        let d = doc_from("1,2,3\n4,5,6", false);
        assert_eq!(d.headers(), &["Column 1", "Column 2", "Column 3"]);
        assert_eq!(d.n_rows(), 2);
    }

    #[test]
    fn set_cell_and_undo_redo() {
        let mut d = doc_from("a,b\n1,2", true);
        assert!(!d.is_dirty());
        d.set_cell(0, 0, "X".into()).unwrap();
        assert_eq!(d.cell(0, 0), "X");
        assert!(d.is_dirty());
        d.undo().unwrap();
        assert_eq!(d.cell(0, 0), "1");
        assert!(!d.is_dirty());
        d.redo().unwrap();
        assert_eq!(d.cell(0, 0), "X");
        assert!(d.is_dirty());
    }

    #[test]
    fn insert_and_delete_rows_undo() {
        let mut d = doc_from("a\n1\n2\n3", true);
        d.insert_rows(1, 2).unwrap();
        assert_eq!(d.n_rows(), 5);
        assert_eq!(d.cell(1, 0), "");
        d.undo().unwrap();
        assert_eq!(d.n_rows(), 3);
        assert_eq!(d.cell(1, 0), "2");

        d.delete_rows(vec![0, 2]).unwrap();
        assert_eq!(d.n_rows(), 1);
        assert_eq!(d.cell(0, 0), "2");
        d.undo().unwrap();
        assert_eq!(d.n_rows(), 3);
        assert_eq!(d.cell(0, 0), "1");
        assert_eq!(d.cell(2, 0), "3");
    }

    #[test]
    fn move_row_round_trips() {
        let mut d = doc_from("a\n1\n2\n3\n4", true);
        d.move_row(0, 2).unwrap();
        assert_eq!(d.cell(0, 0), "2");
        assert_eq!(d.cell(2, 0), "1");
        d.undo().unwrap();
        assert_eq!(d.cell(0, 0), "1");
        assert_eq!(d.cell(3, 0), "4");
    }

    #[test]
    fn column_ops_undo() {
        let mut d = doc_from("a,b\n1,2\n3,4", true);
        d.insert_column(1, "mid".into()).unwrap();
        assert_eq!(d.headers(), &["a", "mid", "b"]);
        assert_eq!(d.cell(0, 1), "");
        assert_eq!(d.cell(0, 2), "2");
        d.undo().unwrap();
        assert_eq!(d.headers(), &["a", "b"]);
        assert_eq!(d.cell(0, 1), "2");

        d.delete_columns(vec![0]).unwrap();
        assert_eq!(d.headers(), &["b"]);
        assert_eq!(d.cell(0, 0), "2");
        d.undo().unwrap();
        assert_eq!(d.headers(), &["a", "b"]);
        assert_eq!(d.cell(0, 0), "1");
    }

    #[test]
    fn rename_and_move_column() {
        let mut d = doc_from("a,b,c\n1,2,3", true);
        d.rename_column(1, "B".into()).unwrap();
        assert_eq!(d.headers(), &["a", "B", "c"]);
        d.move_column(0, 2).unwrap();
        assert_eq!(d.headers(), &["B", "c", "a"]);
        assert_eq!(d.cell(0, 2), "1");
        d.undo().unwrap();
        assert_eq!(d.headers(), &["a", "B", "c"]);
        assert_eq!(d.cell(0, 0), "1");
    }

    #[test]
    fn paste_grows_and_is_single_undo() {
        let mut d = doc_from("a,b\n1,2", true);
        let block = vec![
            vec!["x".to_string(), "y".to_string(), "z".to_string()],
            vec!["p".to_string(), "q".to_string(), "r".to_string()],
        ];
        d.paste(0, 0, block).unwrap();
        assert_eq!(d.n_rows(), 2);
        assert_eq!(d.n_cols(), 3);
        assert_eq!(d.cell(0, 2), "z");
        assert_eq!(d.cell(1, 0), "p");
        // One Ctrl+Z reverts the whole paste, including the grown column.
        d.undo().unwrap();
        assert_eq!(d.n_cols(), 2);
        assert_eq!(d.cell(0, 0), "1");
    }

    #[test]
    fn sort_and_undo() {
        let mut d = doc_from("n\n3\n1\n2", true);
        d.sort(&[SortKey {
            column: 0,
            descending: false,
        }])
        .unwrap();
        assert_eq!(d.cell(0, 0), "1");
        assert_eq!(d.cell(2, 0), "3");
        d.undo().unwrap();
        assert_eq!(d.cell(0, 0), "3");
        assert_eq!(d.cell(1, 0), "1");
    }

    #[test]
    fn header_toggle_round_trip() {
        let mut d = doc_from("a,b\n1,2", true);
        assert_eq!(d.headers(), &["a", "b"]);
        assert_eq!(d.n_rows(), 1);
        d.set_header_mode(false).unwrap();
        assert_eq!(d.n_rows(), 2);
        assert_eq!(d.cell(0, 0), "a");
        d.set_header_mode(true).unwrap();
        assert_eq!(d.headers(), &["a", "b"]);
        assert_eq!(d.n_rows(), 1);
    }

    #[test]
    fn dirty_cell_follows_sort() {
        let mut d = doc_from("n\n3\n1\n2", true);
        d.set_cell(0, 0, "9".into()).unwrap(); // row with value 3 -> 9
        let win = d.get_rows(0, 3).unwrap();
        assert!(win.dirty[0][0]);
        d.sort(&[SortKey {
            column: 0,
            descending: false,
        }])
        .unwrap();
        // "9" sorts last; its dirty flag should travel with it.
        let win = d.get_rows(0, 3).unwrap();
        assert!(!win.dirty[0][0]);
        assert!(win.dirty[2][0]);
    }

    #[test]
    fn dirty_survives_save_undo_then_new_edit() {
        // Regression: save -> undo -> a *new* edit must remain dirty, because the
        // saved state lived in the redo branch that the new edit discards.
        let mut d = doc_from("a\n1", true);
        d.set_cell(0, 0, "2".into()).unwrap();
        d.set_cell(0, 0, "3".into()).unwrap();
        d.mark_saved(None); // saved at "3"
        assert!(!d.is_dirty());
        d.undo().unwrap(); // back to "2"
        assert!(d.is_dirty());
        d.set_cell(0, 0, "9".into()).unwrap(); // diverge; saved "3" now unreachable
        assert_eq!(d.cell(0, 0), "9");
        assert!(
            d.is_dirty(),
            "document differs from the saved file but reported clean"
        );
    }

    #[test]
    fn selection_stats_numeric_and_text() {
        let d = doc_from("a,b\n10,x\n20,y\n30,z", true);
        // The whole 3x2 selection: 6 cells, 3 numeric (10/20/30).
        let stats = d
            .selection_stats(CellRect {
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            })
            .unwrap();
        assert_eq!(stats.count, 6);
        assert_eq!(stats.numeric_count, 3);
        assert_eq!(stats.sum, 60.0);
        assert_eq!(stats.avg, Some(20.0));
        assert_eq!(stats.min, Some(10.0));
        assert_eq!(stats.max, Some(30.0));
    }

    #[test]
    fn selection_stats_clamps_out_of_range() {
        let d = doc_from("a\n1\n2", true);
        let stats = d
            .selection_stats(CellRect {
                x: 0,
                y: 0,
                width: 10,
                height: 100,
            })
            .unwrap();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.sum, 3.0);
    }

    /// Assert that running `mutate` strictly increases the revision.
    fn assert_bumps(d: &mut Document, what: &str, mutate: impl FnOnce(&mut Document)) {
        let before = d.revision();
        mutate(d);
        assert!(d.revision() > before, "{what} must bump the revision");
    }

    #[test]
    fn revision_bumps_on_every_mutation_kind() {
        let mut d = doc_from("a,b\n1,2\n3,4\n5,6", true);
        assert_bumps(&mut d, "set_cell", |d| {
            d.set_cell(0, 0, "X".into()).unwrap()
        });
        assert_bumps(&mut d, "insert_rows", |d| d.insert_rows(0, 1).unwrap());
        assert_bumps(&mut d, "delete_rows", |d| d.delete_rows(vec![0]).unwrap());
        assert_bumps(&mut d, "move_row", |d| d.move_row(0, 1).unwrap());
        assert_bumps(&mut d, "insert_column", |d| {
            d.insert_column(0, "new".into()).unwrap()
        });
        assert_bumps(&mut d, "delete_columns", |d| {
            d.delete_columns(vec![0]).unwrap()
        });
        assert_bumps(&mut d, "rename_column", |d| {
            d.rename_column(0, "renamed".into()).unwrap()
        });
        assert_bumps(&mut d, "move_column", |d| d.move_column(0, 1).unwrap());
        assert_bumps(&mut d, "paste", |d| {
            d.paste(0, 0, vec![vec!["p".into()]]).unwrap()
        });
        assert_bumps(&mut d, "sort", |d| {
            d.sort(&[SortKey {
                column: 0,
                descending: true,
            }])
            .unwrap()
        });
        assert_bumps(&mut d, "undo", |d| d.undo().unwrap());
        assert_bumps(&mut d, "redo", |d| d.redo().unwrap());
        assert_bumps(&mut d, "set_header_mode", |d| {
            d.set_header_mode(false).unwrap()
        });
        assert_bumps(&mut d, "set_filter", |d| d.set_filter(vec![0]).unwrap());
        assert_bumps(&mut d, "clear_filter", |d| d.clear_filter().unwrap());
        assert_bumps(&mut d, "set_view_sort", |d| {
            d.set_view_sort(vec![SortKey {
                column: 0,
                descending: true,
            }])
            .unwrap()
        });
    }

    #[test]
    fn follow_range_filter_extends_on_append() {
        // F19 "only new rows": the range is LIVE — records appended by the
        // watcher become visible without reapplying the filter.
        let mut d = doc_from("a\n1\n2", true);
        d.set_follow(true);
        d.set_follow_range(Some(2));
        d.set_filter(Vec::new()).unwrap(); // rows 0..2 are old; nothing new yet
        assert_eq!(d.visible_len(), 0);

        d.append_follow_rows(vec![vec!["3".into()]]);
        assert_eq!(d.visible_len(), 1, "the appended row entered the range");
        assert_eq!(d.get_rows(0, 10).unwrap().rows[0][0], "3");

        // Clearing the filter ends the live range.
        d.clear_filter().unwrap();
        d.append_follow_rows(vec![vec!["4".into()]]);
        assert_eq!(d.visible_len(), 4);
    }

    #[test]
    fn revision_unchanged_by_noops_and_saves() {
        let mut d = doc_from("a\n1", true);
        let r = d.revision();
        // Writing the identical value registers no edit.
        d.set_cell(0, 0, "1".into()).unwrap();
        assert_eq!(d.revision(), r);
        // Clearing a filter that isn't set changes nothing.
        d.clear_filter().unwrap();
        assert_eq!(d.revision(), r);
        // Toggling the header mode to its current value changes nothing.
        d.set_header_mode(true).unwrap();
        assert_eq!(d.revision(), r);
        // Reads and save markers don't count as mutations.
        let _ = d.get_rows(0, 10);
        d.mark_saved(None);
        assert_eq!(d.revision(), r);
    }

    #[test]
    fn check_revision_guards_stale_operations() {
        let mut d = doc_from("a\n1", true);
        let captured = d.revision();
        assert!(d.check_revision(captured).is_ok());
        d.set_cell(0, 0, "2".into()).unwrap();
        assert!(matches!(
            d.check_revision(captured),
            Err(AppError::StaleRevision { .. })
        ));
        assert!(d.check_revision(d.revision()).is_ok());
    }

    #[test]
    fn replace_columns_is_one_undo_and_stays_rectangular() {
        let mut d = doc_from("full name,age\nAda Lovelace,36\nBob Ray,40", true);
        // Split "full name" into two columns in a single operation.
        d.replace_columns(
            vec![0],
            0,
            vec![
                ("first".into(), vec!["Ada".into(), "Bob".into()]),
                ("last".into(), vec!["Lovelace".into(), "Ray".into()]),
            ],
        )
        .unwrap();
        assert_eq!(d.headers(), &["first", "last", "age"]);
        assert_eq!(d.n_cols(), 3);
        assert_eq!(d.cell(0, 0), "Ada");
        assert_eq!(d.cell(1, 1), "Ray");
        assert_eq!(d.cell(0, 2), "36");
        assert!(d.rows().iter().all(|r| r.len() == 3), "rectangular");

        // ONE undo restores the original headers, values and structure.
        d.undo().unwrap();
        assert_eq!(d.headers(), &["full name", "age"]);
        assert_eq!(d.cell(0, 0), "Ada Lovelace");
        assert_eq!(d.cell(1, 0), "Bob Ray");
        assert!(!d.can_undo(), "split+fill was a single operation");
    }

    #[test]
    fn column_revisions_track_only_touched_columns() {
        let mut d = doc_from("a,b\n1,2\n3,4", true);
        let base_b = d.column_revision(1);

        // Editing column A must not invalidate column B's revision.
        d.set_cell(0, 0, "X".into()).unwrap();
        assert_eq!(d.column_revision(0), d.revision());
        assert_eq!(d.column_revision(1), base_b);

        // Row structure changes touch every column.
        d.insert_rows(0, 1).unwrap();
        assert_eq!(d.column_revision(0), d.revision());
        assert_eq!(d.column_revision(1), d.revision());

        // Pure reorderings keep each column's value multiset: no touch.
        let before = (d.column_revision(0), d.column_revision(1), d.revision());
        d.sort(&[SortKey {
            column: 0,
            descending: true,
        }])
        .unwrap();
        assert!(d.revision() > before.2, "sort still bumps the doc revision");
        assert_eq!(d.column_revision(0), before.0);
        assert_eq!(d.column_revision(1), before.1);

        // Undo of a cell edit re-touches exactly that column.
        d.undo().unwrap(); // undo sort (touches nothing)
        let after_sort_undo = d.column_revision(0);
        d.undo().unwrap(); // undo insert_rows (touches all)
        assert!(d.column_revision(0) > after_sort_undo);

        // Column structure changes touch all and keep the vector aligned.
        d.insert_column(0, "new".into()).unwrap();
        assert_eq!(d.column_revision(2), d.revision());
        assert_eq!(d.column_revision(0), d.revision());
    }

    #[test]
    fn filter_revision_tracks_view_changes_only() {
        let mut d = doc_from("a\n1\n2", true);
        let f0 = d.filter_revision();
        d.set_cell(0, 0, "9".into()).unwrap();
        assert_eq!(
            d.filter_revision(),
            f0,
            "cell edits leave the filter revision"
        );
        d.set_filter(vec![0]).unwrap();
        assert_eq!(d.filter_revision(), d.revision());
        let f1 = d.filter_revision();
        d.clear_filter().unwrap();
        assert!(d.filter_revision() > f1);
    }

    #[test]
    fn paste_special_is_one_undo_step_in_both_modes() {
        use crate::paste::PasteMode;
        // Overwrite with growth, skip-blanks, and a header rename: one undo
        // restores EVERYTHING (values, structure, header).
        let mut d = doc_from("a,b\n1,2\n3,4", true);
        let block = vec![
            vec!["NewB".to_string(), "NewC".to_string()],
            vec!["x".to_string(), String::new()],
            vec![String::new(), "z".to_string()],
        ];
        d.paste_special(1, 1, block, PasteMode::Overwrite, true, true)
            .unwrap();
        assert_eq!(d.headers(), &["a", "NewB", "NewC"]);
        assert_eq!(d.cell(1, 1), "x");
        assert_eq!(d.cell(1, 2), ""); // skipped blank left the (new) cell empty
        assert_eq!(d.cell(2, 2), "z");
        assert_eq!(d.n_rows(), 3); // grew by one row
        d.undo().unwrap();
        assert_eq!(d.headers(), &["a", "b"]);
        assert_eq!(d.n_rows(), 2);
        assert_eq!(d.n_cols(), 2);
        assert_eq!(d.cell(1, 1), "4");
        assert!(!d.can_undo(), "single composite operation");

        // InsertRows mode splices new rows and undoes as one step.
        let mut d = doc_from("a\n1\n2", true);
        let block = vec![vec!["x".to_string()], vec!["y".to_string()]];
        d.paste_special(1, 0, block, PasteMode::InsertRows, false, false)
            .unwrap();
        assert_eq!(d.n_rows(), 4);
        assert_eq!(d.cell(1, 0), "x");
        assert_eq!(d.cell(2, 0), "y");
        assert_eq!(d.cell(3, 0), "2");
        d.undo().unwrap();
        assert_eq!(d.n_rows(), 2);
        assert!(!d.can_undo());
    }

    #[test]
    fn skip_blanks_preserves_existing_destination_values() {
        use crate::paste::PasteMode;
        let mut d = doc_from("a,b\nkeep,old", true);
        let block = vec![vec![String::new(), "new".to_string()]];
        d.paste_special(0, 0, block, PasteMode::Overwrite, true, false)
            .unwrap();
        assert_eq!(d.cell(0, 0), "keep", "blank source cell skipped");
        assert_eq!(d.cell(0, 1), "new");
    }

    #[test]
    fn set_revision_continues_sequence_across_reparse() {
        let mut replacement = doc_from("a\n9", true);
        replacement.set_revision(41);
        assert_eq!(replacement.revision(), 41);
        replacement.set_cell(0, 0, "8".into()).unwrap();
        assert_eq!(replacement.revision(), 42);
    }
}

/// F10: an indexed document must behave exactly like the same file opened
/// editable for every read path, and refuse every mutation.
#[cfg(test)]
mod indexed_tests {
    use super::*;
    use crate::index::{build_index, IndexSettings};
    use crate::parse::{parse, ParseSettings};

    /// Build (editable, indexed) documents over the same bytes, plus the temp
    /// root to clean up.
    fn golden_pair(csv: &str) -> (Document, Document, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "ceesvee-doc-indexed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("data.csv");
        std::fs::write(&source, csv.as_bytes()).unwrap();

        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        let indexed_file = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings::default(),
            &mut |_| Ok(()),
        )
        .unwrap();
        let has_header = indexed_file.has_header_row;
        let editable = Document::from_parsed(1, Some(source.clone()), parsed, has_header);
        let indexed = Document::from_index(2, Some(source), indexed_file);
        (editable, indexed, root)
    }

    const SAMPLE: &str = "name,qty,price\nApple,3,1.50\n\"Doe, Jane\",7,2.00\napricot,,0.75\n\"multi\nline\",9,3.25\n";

    #[test]
    fn reads_match_editable_document() {
        let (editable, indexed, root) = golden_pair(SAMPLE);
        assert_eq!(indexed.backing_name(), "indexedReadOnly");
        assert!(!indexed.is_editable());
        assert_eq!(indexed.n_rows(), editable.n_rows());
        assert_eq!(indexed.n_cols(), editable.n_cols());
        assert_eq!(indexed.headers(), editable.headers());
        assert_eq!(indexed.meta().backing, "indexedReadOnly");
        assert_eq!(editable.meta().backing, "editable");

        let e = editable.get_rows(0, 100).unwrap();
        let i = indexed.get_rows(0, 100).unwrap();
        assert_eq!(e.rows, i.rows);
        assert!(i.dirty.iter().flatten().all(|&d| !d));

        // fetch_rows in arbitrary order.
        let want = [3usize, 0, 2];
        assert_eq!(
            indexed.fetch_rows(&want).unwrap(),
            editable.fetch_rows(&want).unwrap()
        );

        // Selection stats over the numeric columns.
        let rect = CellRect {
            x: 1,
            y: 0,
            width: 2,
            height: 4,
        };
        let es = editable.selection_stats(rect).unwrap();
        let is = indexed.selection_stats(rect).unwrap();
        assert_eq!(es.numeric_count, is.numeric_count);
        assert_eq!(es.sum, is.sum);

        // Column summaries (kind + unique counts).
        let ec = editable.column_summaries().unwrap();
        let ic = indexed.column_summaries().unwrap();
        assert_eq!(ec.len(), ic.len());
        for (a, b) in ec.iter().zip(&ic) {
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.nulls, b.nulls);
            assert_eq!(a.unique, b.unique);
        }
        drop(indexed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn engines_match_editable_document() {
        let (editable, indexed, root) = golden_pair(SAMPLE);

        // find
        let opts = crate::dto::FindOptions {
            query: "ap".into(),
            ..Default::default()
        };
        assert_eq!(
            crate::find::find(&editable, &opts).unwrap(),
            crate::find::find(&indexed, &opts).unwrap()
        );

        // find with a limit stops early.
        let limited = crate::dto::FindOptions {
            query: "a".into(),
            limit: Some(1),
            ..Default::default()
        };
        assert_eq!(crate::find::find(&indexed, &limited).unwrap().len(), 1);

        // filter
        let spec = crate::dto::FilterGroup {
            conjunction: crate::dto::Conjunction::And,
            nodes: vec![crate::dto::FilterNode::Condition(
                crate::dto::FilterCondition {
                    column: 1,
                    op: crate::dto::FilterOp::Gt,
                    value: "4".into(),
                    case_sensitive: false,
                },
            )],
        };
        assert_eq!(
            crate::filter::matching_rows(&editable, &spec).unwrap(),
            crate::filter::matching_rows(&indexed, &spec).unwrap()
        );

        // export bytes are identical
        let opts = crate::dto::ExportOptions {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            quote_style: "minimal".into(),
            line_ending: "lf".into(),
            bom: false,
            include_headers: true,
            backup: Default::default(),
        };
        let mut e_out = Vec::new();
        let mut i_out = Vec::new();
        crate::export::write_document(&editable, &opts, &mut e_out, None).unwrap();
        crate::export::write_document(&indexed, &opts, &mut i_out, None).unwrap();
        assert_eq!(e_out, i_out);

        drop(indexed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn analysis_engines_match_editable_document() {
        let (editable, indexed, root) = golden_pair(SAMPLE);
        let registry = crate::job::JobRegistry::default();

        // Diagnostics: same issue kinds and affected counts.
        let e = crate::diagnostics::scan(&editable, &registry.begin("d", None, |_| {})).unwrap();
        let i = crate::diagnostics::scan(&indexed, &registry.begin("d", None, |_| {})).unwrap();
        let issue_shape = |r: &crate::diagnostics::DiagnosticsReport| {
            r.current
                .iter()
                .map(|x| (x.kind.clone(), x.affected_count))
                .collect::<Vec<_>>()
        };
        assert_eq!(issue_shape(&e), issue_shape(&i));

        // Column profile of the name column: identical counts.
        let opts = crate::profile::ProfileOptions::default();
        let ep = crate::profile::profile_column(
            &editable,
            0,
            crate::profile::ProfileScope::All,
            &opts,
            &registry.begin("p", None, |_| {}),
        )
        .unwrap();
        let ip = crate::profile::profile_column(
            &indexed,
            0,
            crate::profile::ProfileScope::All,
            &opts,
            &registry.begin("p", None, |_| {}),
        )
        .unwrap();
        assert_eq!(ep.row_count, ip.row_count);
        assert_eq!(ep.blank_count, ip.blank_count);
        assert_eq!(ep.distinct_count, ip.distinct_count);
        assert_eq!(ep.top_values.len(), ip.top_values.len());

        // Dedup grouping over the qty column.
        let spec = crate::dedup::DedupSpec {
            key_columns: vec![1],
            trim: true,
            case_insensitive: true,
            collapse_whitespace: false,
            blank_keys_equal: true,
            exclude_blank_keys: false,
        };
        let er =
            crate::dedup::find_duplicates(&editable, &spec, &crate::dto::ExportScope::All, None)
                .unwrap();
        let ir =
            crate::dedup::find_duplicates(&indexed, &spec, &crate::dto::ExportScope::All, None)
                .unwrap();
        assert_eq!(er.group_count, ir.group_count);
        assert_eq!(er.duplicate_rows, ir.duplicate_rows);
        assert_eq!(er.considered_rows, ir.considered_rows);

        // Comparing the editable and indexed documents positionally finds no
        // differences: every record classifies as unchanged.
        let cspec = crate::compare::CompareSpec {
            mode: crate::compare::CompareMode::Positional,
            key_columns: Vec::new(),
            column_mapping: Vec::new(),
            trim: false,
            case_insensitive: false,
            blank_equal: false,
            numeric_equal: false,
            date_equal: false,
        };
        let result = crate::compare::compare(
            &editable,
            &indexed,
            &cspec,
            &registry.begin("c", None, |_| {}),
        )
        .unwrap();
        use crate::compare::DiffStatus;
        let not_unchanged = [
            DiffStatus::Added,
            DiffStatus::Removed,
            DiffStatus::Changed,
            DiffStatus::Conflict,
        ];
        let (_, differing) = crate::compare::results_page(
            &result,
            &editable,
            &indexed,
            0,
            100,
            Some(&not_unchanged),
        )
        .unwrap();
        assert_eq!(differing, 0, "identical data must classify as unchanged");
        let (all, total) =
            crate::compare::results_page(&result, &editable, &indexed, 0, 100, None).unwrap();
        assert_eq!(total, editable.n_rows());
        assert_eq!(all.len(), editable.n_rows());

        drop(indexed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn filtered_reads_work_on_indexed_documents() {
        let (_, mut indexed, root) = golden_pair(SAMPLE);
        indexed.set_filter(vec![1, 3]).unwrap();
        assert_eq!(indexed.visible_len(), 2);
        let win = indexed.get_rows(0, 10).unwrap();
        assert_eq!(win.rows.len(), 2);
        assert_eq!(win.rows[0][0], "Doe, Jane");
        assert_eq!(win.rows[1][0], "multi\nline");
        indexed.clear_filter().unwrap();
        assert_eq!(indexed.visible_len(), 4);
        drop(indexed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn view_sort_works_on_indexed_documents() {
        // The non-destructive sort (F12) is a read-only view, so unlike the
        // destructive sort it must work on indexed documents too.
        let (_, mut indexed, root) = golden_pair(SAMPLE);
        assert!(!indexed.is_editable());
        indexed
            .set_view_sort(vec![SortKey {
                column: 0,
                descending: false,
            }])
            .unwrap();
        let win = indexed.get_rows(0, 10).unwrap();
        let first_col: Vec<&str> = win.rows.iter().map(|r| r[0].as_str()).collect();
        let mut expected = first_col.clone();
        expected.sort();
        assert_eq!(first_col, expected, "display order follows the view sort");
        assert!(indexed.meta().view_sorted);
        assert!(!indexed.meta().filtered, "sort alone is not a filter");
        indexed.clear_row_view();
        drop(indexed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_mutation_is_rejected_read_only() {
        let (_, mut d, root) = golden_pair(SAMPLE);
        let is_read_only = |r: AppResult<()>| matches!(r, Err(AppError::ReadOnly));
        assert!(is_read_only(d.set_cell(0, 0, "x".into())));
        assert!(is_read_only(d.set_cells(vec![(0, 0, "x".into())])));
        assert!(is_read_only(d.insert_rows(0, 1)));
        assert!(is_read_only(d.delete_rows(vec![0])));
        assert!(is_read_only(d.move_row(0, 1)));
        assert!(is_read_only(d.insert_column(0, "new".into())));
        assert!(is_read_only(d.delete_columns(vec![0])));
        assert!(is_read_only(d.rename_column(0, "renamed".into())));
        assert!(is_read_only(d.move_column(0, 1)));
        assert!(is_read_only(d.paste(0, 0, vec![vec!["p".into()]])));
        assert!(is_read_only(d.replace_columns(vec![0], 0, Vec::new())));
        assert!(is_read_only(d.sort(&[SortKey {
            column: 0,
            descending: false,
        }])));
        assert!(is_read_only(d.set_header_mode(false)));
        // Nothing was ever registered, so undo/redo report their usual state.
        assert!(matches!(d.undo(), Err(AppError::NothingToUndo)));
        assert!(matches!(d.redo(), Err(AppError::NothingToRedo)));
        assert!(!d.meta().can_undo);
        assert!(!d.meta().dirty);
        drop(d);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn convert_to_editable_materialises_and_unlocks() {
        let (editable, mut indexed, root) = golden_pair(SAMPLE);
        let revision_before = indexed.revision();

        let n = indexed.n_rows();
        let mut rows = Vec::with_capacity(n);
        indexed
            .visit_rows(0..n, &mut |_, row| {
                rows.push(row.to_vec());
                Ok(true)
            })
            .unwrap();
        indexed.make_editable(rows).unwrap();

        assert!(indexed.is_editable());
        assert_eq!(indexed.backing_name(), "editable");
        assert!(indexed.revision() > revision_before);
        assert_eq!(
            indexed.get_rows(0, 100).unwrap().rows,
            editable.get_rows(0, 100).unwrap().rows
        );
        // Mutations now work.
        indexed.set_cell(0, 0, "edited".into()).unwrap();
        assert_eq!(indexed.get_rows(0, 1).unwrap().rows[0][0], "edited");
        // Double-convert is rejected.
        assert!(indexed.make_editable(Vec::new()).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod f15_tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    #[test]
    fn changes_reflect_the_dirty_state_and_saving_clears_them() {
        let mut d = doc("a,b\n1,2\n3,4\n");
        assert!(d.changes_since_save().is_empty());
        d.set_cells(vec![(0, 0, "X".into())]).unwrap();
        d.insert_rows(2, 1).unwrap();
        let changes = d.changes_since_save();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].kind, "cellEdits");
        assert_eq!(changes[0].sample[0].old, "1");
        assert_eq!(changes[0].sample[0].new, "X");
        assert!(changes[1].structural);
        assert!(changes[0].id < changes[1].id, "stable, ordered ids");

        d.mark_saved(None);
        assert!(d.changes_since_save().is_empty(), "saving clears the list");
    }

    #[test]
    fn reverting_an_insert_keeps_later_edits_recoverable() {
        // Insert a row, edit INSIDE it, revert the insert, then undo the
        // revert: the edit must come back — the revert is a capturing
        // delete, never a blank re-insert.
        let mut d = doc("a\n1\n");
        d.insert_rows(1, 1).unwrap();
        d.set_cells(vec![(1, 0, "typed later".into())]).unwrap();
        let insert_id = d.changes_since_save()[0].id;

        let changes = d.changes_since_save();
        assert!(changes[0].revertible, "plain inserts stay revertible");
        d.revert_stack_op(insert_id).unwrap();
        assert_eq!(d.n_rows(), 1, "the inserted row is gone");

        d.undo().unwrap();
        assert_eq!(
            d.cell(1, 0),
            "typed later",
            "undoing the revert restores the later edit, not a blank row"
        );
    }

    #[test]
    fn composite_insert_revert_is_blocked_while_edits_live_inside() {
        // A paste that GREW the grid is a composite containing an insert;
        // a later edit inside the added column blocks the inverse revert
        // (which could not restore that edit on undo).
        let mut d = doc("a\n1\n");
        d.paste(0, 0, vec![vec!["p1".into(), "p2".into()]]).unwrap();
        d.set_cells(vec![(0, 1, "later".into())]).unwrap();

        let changes = d.changes_since_save();
        assert_eq!(changes[0].kind, "composite");
        assert!(!changes[0].revertible);
        assert!(d.revert_stack_op(changes[0].id).is_err());

        // Reverting the inner edit first unblocks the composite.
        let edit_id = changes[1].id;
        d.revert_cells_of_op(edit_id, &[(0, 1)]).unwrap();
        let refreshed = d.changes_since_save();
        assert!(refreshed[0].revertible, "no live edits inside any more");
        d.revert_stack_op(refreshed[0].id).unwrap();
        assert_eq!(d.n_cols(), 1);
    }

    #[test]
    fn undoing_past_the_save_reports_the_redo_branch_state() {
        let mut d = doc("a\n1\n");
        d.set_cells(vec![(0, 0, "X".into())]).unwrap();
        d.mark_saved(None);
        assert!(!d.saved_in_redo_branch());

        d.undo().unwrap();
        assert!(d.is_dirty(), "current state differs from the saved file");
        assert!(d.changes_since_save().is_empty(), "nothing to list");
        assert!(d.saved_in_redo_branch(), "the save is ahead, in redo");

        d.redo().unwrap();
        assert!(!d.is_dirty());
        assert!(!d.saved_in_redo_branch());
    }

    #[test]
    fn revert_one_cell_leaves_unrelated_edits_alone() {
        let mut d = doc("a,b\n1,2\n");
        d.set_cells(vec![(0, 0, "X".into()), (0, 1, "Y".into())])
            .unwrap();
        let id = d.changes_since_save()[0].id;
        d.revert_cells_of_op(id, &[(0, 0)]).unwrap();
        assert_eq!(d.cell(0, 0), "1", "reverted");
        assert_eq!(d.cell(0, 1), "Y", "unrelated cell untouched");
        // The revert is itself undoable.
        d.undo().unwrap();
        assert_eq!(d.cell(0, 0), "X");
    }

    #[test]
    fn whole_op_revert_is_a_new_undoable_operation() {
        let mut d = doc("a\n1\n2\n");
        d.delete_rows(vec![1]).unwrap();
        assert_eq!(d.n_rows(), 1);
        let id = d.changes_since_save()[0].id;
        d.revert_stack_op(id).unwrap();
        assert_eq!(d.n_rows(), 2, "the deleted row is back");
        assert_eq!(d.cell(1, 0), "2");
        // History keeps both the delete AND the revert.
        assert_eq!(d.changes_since_save().len(), 2);
        d.undo().unwrap();
        assert_eq!(d.n_rows(), 1, "undoing the revert re-deletes");
    }

    #[test]
    fn structural_ops_block_earlier_selective_reverts() {
        let mut d = doc("a,b\n1,2\n");
        d.set_cells(vec![(0, 0, "X".into())]).unwrap();
        let id = d.changes_since_save()[0].id;
        d.insert_rows(1, 1).unwrap(); // structural, AFTER the cell edit
        let changes = d.changes_since_save();
        assert!(!changes[0].revertible);
        assert!(changes[0].blocked_reason.is_some());
        assert!(d.revert_stack_op(id).is_err());
        assert!(d.revert_cells_of_op(id, &[(0, 0)]).is_err());
        // Revert all still works and is one op.
        d.revert_all_changes().unwrap();
        assert_eq!(d.cell(0, 0), "1");
        assert_eq!(d.n_rows(), 1);
        d.undo().unwrap();
        assert_eq!(d.cell(0, 0), "X");
        assert_eq!(d.n_rows(), 2);
    }

    #[test]
    fn revert_column_restores_values_at_last_save() {
        let mut d = doc("a,b\n1,2\n3,4\n");
        d.set_cells(vec![(0, 0, "X".into())]).unwrap();
        d.set_cells(vec![(0, 0, "Y".into()), (1, 0, "Z".into())])
            .unwrap();
        d.set_cells(vec![(0, 1, "W".into())]).unwrap();
        d.revert_column_edits(0).unwrap();
        assert_eq!(d.cell(0, 0), "1", "back to the SAVED value, not X");
        assert_eq!(d.cell(1, 0), "3");
        assert_eq!(d.cell(0, 1), "W", "other columns untouched");
    }

    #[test]
    fn revert_all_after_composite_paste_restores_everything() {
        let mut d = doc("a,b\n1,2\n");
        // A composite: grow + fill (like a paste).
        d.insert_rows(1, 1).unwrap();
        d.set_cells(vec![(1, 0, "p".into()), (1, 1, "q".into())])
            .unwrap();
        d.revert_all_changes().unwrap();
        assert_eq!(d.n_rows(), 1);
        assert_eq!(d.cell(0, 0), "1");
        assert!(d.is_dirty(), "reverting is itself an unsaved change");
    }
}

#[cfg(test)]
mod f16_tests {
    use super::*;
    use crate::journal::{read_journal, JournalWriter};
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    #[test]
    fn replay_rejects_delete_rows_with_a_different_width() {
        // A journal recovered against a source whose COLUMN COUNT changed:
        // the captured DeleteRows rows are 3 wide, the document is 2 wide.
        // Accepting it would let a later Undo re-insert ragged rows.
        let mut d = doc("a,b\n1,2\n3,4\n");
        let op = EditOp::DeleteRows {
            removed: vec![(0, vec!["x".into(), "y".into(), "z".into()])],
        };
        let record = crate::journal::JournalRecord::Op {
            op: serde_json::to_value(&op).unwrap(),
        };
        let err = d.replay_journal_records(&[record]);
        assert!(err.is_err(), "width mismatch must fail replay");
        assert_eq!(d.n_rows(), 2, "nothing was applied");

        // The same shape with the CORRECT width replays fine.
        let ok_op = EditOp::DeleteRows {
            removed: vec![(0, vec!["1".into(), "2".into()])],
        };
        let record = crate::journal::JournalRecord::Op {
            op: serde_json::to_value(&ok_op).unwrap(),
        };
        assert_eq!(d.replay_journal_records(&[record]).unwrap(), 1);
        assert_eq!(d.n_rows(), 1);
        d.undo().unwrap();
        assert_eq!(d.n_rows(), 2, "undo re-inserts a rectangular row");
    }

    #[test]
    fn journaled_edits_replay_in_order_onto_a_fresh_parse() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("t.journal");

        // Session 1: edit with a journal attached, then "crash".
        let mut live = doc("a,b\n1,2\n3,4\n");
        let writer = JournalWriter::create(journal_path.clone(), &live.journal_header()).unwrap();
        live.attach_journal(writer);
        live.set_cells(vec![(0, 0, "X".into())]).unwrap();
        live.insert_rows(2, 1).unwrap();
        live.set_cells(vec![(2, 1, "new".into())]).unwrap();
        live.undo().unwrap(); // journaled as an Undo marker
        drop(live); // crash: no clean close, the journal survives

        // Session 2: recover by replaying onto a fresh parse.
        let (_header, records) = read_journal(&journal_path).unwrap();
        let mut recovered = doc("a,b\n1,2\n3,4\n");
        let applied = recovered.replay_journal_records(&records).unwrap();
        assert_eq!(applied, 3);
        assert_eq!(recovered.cell(0, 0), "X");
        assert_eq!(recovered.n_rows(), 3, "inserted row survives");
        assert_eq!(recovered.cell(2, 1), "", "the undone edit stays undone");
        assert!(recovered.is_dirty(), "recovery produces a dirty document");
        assert!(recovered.can_redo(), "even the redo branch is restored");
    }

    #[test]
    fn mismatched_journals_error_instead_of_panicking() {
        // A journal recorded against a WIDER document.
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("w.journal");
        let mut wide = doc("a,b,c\n1,2,3\n");
        let writer = JournalWriter::create(journal_path.clone(), &wide.journal_header()).unwrap();
        wide.attach_journal(writer);
        wide.set_cells(vec![(0, 2, "X".into())]).unwrap();
        drop(wide);

        let (_h, records) = read_journal(&journal_path).unwrap();
        let mut narrow = doc("a\n1\n");
        let err = match narrow.replay_journal_records(&records) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("out-of-bounds replay must fail"),
        };
        assert!(err.contains("does not match"));
        assert_eq!(narrow.cell(0, 0), "1", "nothing was applied");
    }

    #[test]
    fn saving_resets_the_journal_and_replaying_it_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("s.journal");
        let mut live = doc("a\n1\n");
        let writer = JournalWriter::create(journal_path.clone(), &live.journal_header()).unwrap();
        live.attach_journal(writer);
        live.set_cells(vec![(0, 0, "X".into())]).unwrap();
        live.mark_saved(None);
        live.reset_journal_baseline();
        let (_h, records) = read_journal(&journal_path).unwrap();
        assert!(records.is_empty(), "a clean save leaves an empty journal");
    }
}

#[cfg(test)]
mod f12_tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    fn key(column: usize, descending: bool) -> SortKey {
        SortKey { column, descending }
    }

    #[test]
    fn column_ids_positional_and_stable_through_rename() {
        let mut d = doc_from("name,age\nAda,36", true);
        assert_eq!(d.meta().column_ids, vec!["c0", "c1"]);
        d.rename_column(0, "person".into()).unwrap();
        assert_eq!(
            d.meta().column_ids,
            vec!["c0", "c1"],
            "renaming changes the header text, never the identity"
        );
        d.undo().unwrap();
        assert_eq!(d.meta().column_ids, vec!["c0", "c1"]);
    }

    #[test]
    fn column_ids_track_structure_and_undo_redo_restores_identity() {
        let mut d = doc_from("a,b\n1,2", true);
        d.insert_column(1, "x".into()).unwrap();
        assert_eq!(d.meta().column_ids, vec!["c0", "c2", "c1"]);
        d.move_column(2, 0).unwrap();
        assert_eq!(d.meta().column_ids, vec!["c1", "c0", "c2"]);
        d.delete_columns(vec![0]).unwrap();
        assert_eq!(d.meta().column_ids, vec!["c0", "c2"]);

        // Undo restores the SAME identities, not fresh ones.
        d.undo().unwrap();
        assert_eq!(d.meta().column_ids, vec!["c1", "c0", "c2"]);
        d.undo().unwrap();
        assert_eq!(d.meta().column_ids, vec!["c0", "c2", "c1"]);
        d.undo().unwrap();
        assert_eq!(d.meta().column_ids, vec!["c0", "c1"]);
        // Redo of the insert reproduces the same minted id.
        d.redo().unwrap();
        assert_eq!(d.meta().column_ids, vec!["c0", "c2", "c1"]);
    }

    #[test]
    fn column_ids_survive_replace_columns_round_trip() {
        let mut d = doc_from("full,keep\nAda Lovelace,1", true);
        let before = d.meta().column_ids.clone();
        d.replace_columns(
            vec![0],
            0,
            vec![
                ("first".into(), vec!["Ada".into()]),
                ("last".into(), vec!["Lovelace".into()]),
            ],
        )
        .unwrap();
        let after = d.meta().column_ids.clone();
        assert_eq!(after.len(), 3);
        assert_eq!(after[2], "c1", "untouched column keeps its id");
        assert_ne!(after[0], before[0], "replacement columns get fresh ids");
        d.undo().unwrap();
        assert_eq!(
            d.meta().column_ids,
            before,
            "one undo restores the original identities"
        );
    }

    #[test]
    fn journal_replay_reproduces_column_ids() {
        // The stable id rides inside the serialized op, so crash recovery
        // (F16) rebuilds the same identities a named view was saved against.
        let mut live = doc_from("a,b\n1,2", true);
        live.insert_column(1, "mid".into()).unwrap();
        let op_json = serde_json::to_value(&live.undo_stack[0]).unwrap();

        let mut recovered = doc_from("a,b\n1,2", true);
        recovered
            .replay_journal_records(&[crate::journal::JournalRecord::Op { op: op_json }])
            .unwrap();
        assert_eq!(recovered.meta().column_ids, live.meta().column_ids);
    }

    #[test]
    fn view_sort_orders_display_without_touching_source() {
        let mut d = doc_from("n\n9\n10\n2", true);
        d.set_view_sort(vec![key(0, false)]).unwrap();

        let win = d.get_rows(0, 10).unwrap();
        let shown: Vec<&str> = win.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(shown, vec!["2", "9", "10"], "typed (numeric) order");

        // Source order, undo history and the dirty flag are untouched.
        assert_eq!(d.rows()[0][0], "9");
        assert!(!d.is_dirty());
        assert!(!d.can_undo());
        assert!(d.meta().view_sorted);
        assert!(!d.meta().filtered);

        // Display coordinates translate to the correct absolute rows, so an
        // edit made in a sorted view lands on the right source row.
        assert_eq!(d.display_to_abs(0), Some(2));
        assert_eq!(d.display_to_abs(2), Some(1));
        d.set_cell(d.display_to_abs(0).unwrap(), 0, "3".into())
            .unwrap();
        assert_eq!(d.rows()[2][0], "3", "the edit hit the absolute row");
    }

    #[test]
    fn view_sort_composes_with_filter_and_clears_independently() {
        let mut d = doc_from("v\nb\nd\na\nc", true);
        d.set_filter(vec![0, 1, 3]).unwrap(); // hide "a"
        d.set_view_sort(vec![key(0, true)]).unwrap();
        let shown: Vec<usize> = d.filter_view().unwrap().to_vec();
        assert_eq!(shown, vec![1, 3, 0], "filtered rows in descending order");

        // Re-filtering keeps the sort.
        d.set_filter(vec![0, 2]).unwrap();
        assert_eq!(d.filter_view().unwrap(), &[0, 2], "b > a descending");
        assert!(d.meta().view_sorted);

        // Clearing the filter keeps the sort over all rows.
        d.clear_filter().unwrap();
        assert_eq!(d.filter_view().unwrap(), &[1, 3, 0, 2]);
        assert!(!d.meta().filtered);

        // Clearing the sort with empty keys drops the whole view.
        d.set_view_sort(Vec::new()).unwrap();
        assert!(d.filter_view().is_none());
        assert!(!d.meta().view_sorted);
    }

    #[test]
    fn view_sort_is_stable_for_ties() {
        let mut d = doc_from("k,i\nx,1\nx,2\nx,3", true);
        d.set_view_sort(vec![key(0, false)]).unwrap();
        assert_eq!(
            d.filter_view().unwrap(),
            &[0, 1, 2],
            "equal keys keep source order"
        );
    }

    #[test]
    fn clear_row_view_drops_both_ingredients() {
        let mut d = doc_from("v\n2\n1", true);
        d.set_filter(vec![0]).unwrap();
        d.set_view_sort(vec![key(0, false)]).unwrap();
        let rev = d.revision();
        d.clear_row_view();
        assert!(d.filter_view().is_none());
        assert!(!d.meta().filtered);
        assert!(!d.meta().view_sorted);
        assert_eq!(d.revision(), rev + 1, "one bump for the combined clear");
        // Idempotent: clearing an empty view bumps nothing.
        d.clear_row_view();
        assert_eq!(d.revision(), rev + 1);
    }

    #[test]
    fn view_sort_rejects_out_of_range_column() {
        let mut d = doc_from("a\n1", true);
        assert!(d.set_view_sort(vec![key(5, false)]).is_err());
        assert!(!d.meta().view_sorted);
    }

    // ----- typed sort under a declared schema (F31) -------------------------

    fn declare(d: &mut Document, col: usize, lt: crate::schema::LogicalType) {
        let schema = crate::schema::ColumnSchema::new(
            d.column_ids()[col].clone(),
            d.headers()[col].clone(),
            lt,
        );
        d.set_column_schema(schema);
    }

    #[test]
    fn declared_integer_sorts_beyond_f64_precision() {
        // These two differ only below f64 precision: the heuristic (f64)
        // order ties them, the declared-integer order must not.
        let mut d = doc_from("n\n9007199254740993\n9007199254740992", true);
        declare(&mut d, 0, crate::schema::LogicalType::Integer);
        d.sort(&[key(0, false)]).unwrap();
        assert_eq!(d.cell(0, 0), "9007199254740992");
        assert_eq!(d.cell(1, 0), "9007199254740993");
    }

    #[test]
    fn declared_sort_orders_nullish_first_and_invalid_last() {
        // Second column keeps the empty-cell row from being an empty line
        // (which the parser would skip entirely).
        let mut d = doc_from("n,k\nx,a\n10,b\nNULL,c\n,d\n2,e", true);
        let mut schema = crate::schema::ColumnSchema::new(
            d.column_ids()[0].clone(),
            "n",
            crate::schema::LogicalType::Integer,
        );
        schema.null_tokens = vec!["NULL".to_string()];
        d.set_column_schema(schema);
        d.sort(&[key(0, false)]).unwrap();
        // Documented order: null-ish (empty + null tokens) first — matching
        // how "" sorts before text heuristically — then valid values in
        // typed order, then invalid values last.
        assert_eq!(d.cell(0, 0), "");
        assert_eq!(d.cell(1, 0), "NULL");
        assert_eq!(d.cell(2, 0), "2");
        assert_eq!(d.cell(3, 0), "10");
        assert_eq!(d.cell(4, 0), "x");
    }

    #[test]
    fn declared_date_view_sort_orders_chronologically() {
        // Custom input format: the heuristic would see plain text and sort
        // lexicographically ("02.01.2024" < "31.12.2023" as text).
        let mut d = doc_from("d\n31.12.2023\n02.01.2024", true);
        let mut schema = crate::schema::ColumnSchema::new(
            d.column_ids()[0].clone(),
            "d",
            crate::schema::LogicalType::Date,
        );
        schema.input_formats = Some(vec!["%d.%m.%Y".to_string()]);
        d.set_column_schema(schema);
        d.set_view_sort(vec![key(0, false)]).unwrap();
        assert_eq!(
            d.filter_view().unwrap(),
            &[0, 1],
            "Dec 31 2023 displays before Jan 2 2024"
        );
        assert!(!d.is_dirty(), "view sort stays non-destructive");
    }

    #[test]
    fn schema_survives_undo_of_column_delete() {
        let mut d = doc_from("a,b\n1,x", true);
        let id = d.column_ids()[0].clone();
        declare(&mut d, 0, crate::schema::LogicalType::Integer);
        d.delete_columns(vec![0]).unwrap();
        // The entry lingers keyed by ID while the column is gone…
        assert!(d.column_schema_at(0).map(|s| &s.column_id) != Some(&id));
        d.undo().unwrap();
        // …and resolves again when undo restores the column (same ID).
        assert_eq!(d.column_ids()[0], id);
        assert_eq!(
            d.column_schema_at(0).unwrap().logical_type,
            crate::schema::LogicalType::Integer
        );
    }
}
