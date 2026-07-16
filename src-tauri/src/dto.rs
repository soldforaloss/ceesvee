//! Serializable data-transfer objects exchanged with the front end.
//!
//! Everything crossing the IPC boundary uses `camelCase` to match JS
//! conventions. Delimiters, encodings, line endings and quote styles are passed
//! as strings to keep the wire format simple and forward-compatible.

use serde::{Deserialize, Serialize};

use crate::parse::RaggedSample;

/// Metadata describing an open document. Returned by `open_file` and refreshed
/// by structural commands.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMeta {
    pub id: u64,
    pub path: Option<String>,
    pub file_name: String,
    /// Visible row count (the filtered count when a filter is active).
    pub row_count: usize,
    /// Total data rows, ignoring any active filter.
    pub total_row_count: usize,
    /// Whether a row filter is currently applied.
    pub filtered: bool,
    pub col_count: usize,
    pub headers: Vec<String>,
    pub has_header_row: bool,
    /// The delimiter as a one-character string (e.g. "," or "\t").
    pub delimiter: String,
    /// Canonical encoding name (e.g. "UTF-8").
    pub encoding: String,
    pub had_bom: bool,
    /// "lf" or "crlf".
    pub line_ending: String,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    /// Monotonically increasing revision, bumped on every mutation. Previews
    /// and deferred operations echo this back as `expectedRevision` and are
    /// rejected when the document has moved on.
    pub revision: u64,
    /// Row storage: "editable" (in memory) or "indexedReadOnly" (F10).
    pub backing: String,
}

/// Overrides supplied when (re)opening a file. Any `None` field is auto-detected.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenOptions {
    pub delimiter: Option<String>,
    pub encoding: Option<String>,
    pub has_header_row: Option<bool>,
    /// Open fully in memory even when the size estimate recommends asking
    /// (the "Open editable" choice in the F10 decision dialog).
    #[serde(default)]
    pub force_in_memory: bool,
}

/// Handles returned by `start_open_indexed`: the job to watch and the
/// document id it will register on success.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedOpenStart {
    pub job_id: u64,
    pub doc_id: u64,
}

/// A window of rows plus a parallel dirty-flag matrix for highlighting.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowsResponse {
    pub start: usize,
    pub rows: Vec<Vec<String>>,
    pub dirty: Vec<Vec<bool>>,
}

/// One key in a multi-column sort.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortKey {
    pub column: usize,
    #[serde(default)]
    pub descending: bool,
}

/// A rectangular cell region (used to scope find/replace to a selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Options for find / replace.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindOptions {
    pub query: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_cell: bool,
    #[serde(default)]
    pub selection: Option<CellRect>,
    /// Stop after this many matches (indexed documents paginate find so a
    /// multi-GB scan cannot materialise millions of hits). `None` = no cap.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// A single find hit, in data-row coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindMatch {
    pub row: usize,
    pub col: usize,
}

/// Result of a replace-all: how many cells changed, plus refreshed metadata.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceResult {
    pub replaced: usize,
    pub meta: DocumentMeta,
}

/// Aggregate statistics over a selected cell range, for the status bar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionStats {
    pub count: usize,
    pub numeric_count: usize,
    pub sum: f64,
    pub avg: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// The detected data type of a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnKind {
    Number,
    Date,
    Bool,
    Text,
}

/// Numeric aggregates for a column; present only when it has numeric cells.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericSummary {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

/// Per-column type detection and summary, computed over all data rows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSummary {
    pub column: usize,
    pub kind: ColumnKind,
    /// Total data rows (equals the document's row count).
    pub count: usize,
    /// Empty/blank cells in this column.
    pub nulls: usize,
    /// Number of distinct non-empty values.
    pub unique: usize,
    /// Numeric aggregates over the numeric cells, if any.
    pub numeric: Option<NumericSummary>,
}

/// Comparison operator for a single filter condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Equals,
    NotEquals,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Gt,
    Gte,
    Lt,
    Lte,
    IsEmpty,
    NotEmpty,
    Regex,
}

/// How sibling filter nodes are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conjunction {
    And,
    Or,
}

/// A single leaf condition: `column <op> value`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCondition {
    pub column: usize,
    pub op: FilterOp,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub case_sensitive: bool,
}

/// A group of filter nodes combined with a conjunction (supports nesting).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterGroup {
    pub conjunction: Conjunction,
    pub nodes: Vec<FilterNode>,
}

/// A node in the filter tree: a leaf condition or a nested group. Tagged by a
/// `type` field on the wire ("condition" / "group").
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FilterNode {
    Condition(FilterCondition),
    Group(FilterGroup),
}

/// Identity snapshot of the backing file, used to detect edits made outside
/// CEESVEE. Captured at open/reparse/save time and compared against the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFingerprint {
    pub size: u64,
    pub modified_at_ms: u64,
}

/// One setting whose value would change under a proposed reparse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReparseDiff {
    /// Machine-readable field name (e.g. "delimiter", "rowCount").
    pub field: String,
    pub current: String,
    pub proposed: String,
}

/// Non-destructive preview of reopening the source file with new settings.
/// Nothing in the open document changes while one of these exists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReparsePreview {
    /// The first records exactly as parsed (the header row, when one is
    /// detected, is the first entry).
    pub records: Vec<Vec<String>>,
    /// One-character delimiter that was used.
    pub delimiter: String,
    /// Canonical encoding name that was used.
    pub encoding: String,
    pub had_bom: bool,
    /// "lf" or "crlf".
    pub line_ending: String,
    /// Effective header mode (forced by the caller or auto-detected).
    pub has_header_row: bool,
    /// Data rows the reopened document would have (header excluded).
    pub row_count: usize,
    pub col_count: usize,
    /// Import diagnostics of the previewed parse (see F01).
    pub had_decode_errors: bool,
    pub ragged_total: usize,
    pub modal_field_count: usize,
    pub ragged_samples: Vec<RaggedSample>,
    /// Settings/shape that differ from the current interpretation.
    pub differences: Vec<ReparseDiff>,
    /// Document revision this preview was generated against; echoed back to
    /// `apply_reparse`, which rejects the apply when it no longer matches.
    pub expected_revision: u64,
}

/// Result of comparing the stored source fingerprint against the disk file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalChange {
    /// Whether the file on disk no longer matches what this document loaded.
    pub changed: bool,
    /// Whether the file still exists at all.
    pub exists: bool,
    pub disk: Option<FileFingerprint>,
    pub stored: Option<FileFingerprint>,
}

/// What to do with the previous destination file when saving over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupPolicy {
    /// Atomically replace the destination; keep nothing else.
    #[default]
    None,
    /// Keep one `.bak` copy of the prior destination.
    Single,
}

/// Options controlling how a document is serialized on save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    /// One-character delimiter string.
    pub delimiter: String,
    /// Canonical encoding name.
    pub encoding: String,
    /// "minimal" (a.k.a. "necessary") or "always".
    pub quote_style: String,
    /// "lf" or "crlf".
    pub line_ending: String,
    /// Whether to prepend a byte-order mark.
    pub bom: bool,
    /// Whether to write the header row (only meaningful when one exists).
    #[serde(default = "default_true")]
    pub include_headers: bool,
    /// Backup policy for the previous destination file.
    #[serde(default)]
    pub backup: BackupPolicy,
}

fn default_true() -> bool {
    true
}

/// Which slice of the document an export writes (F04). Row/column/rect
/// coordinates arrive in DISPLAY space (what the user sees under the active
/// filter) and are resolved to absolute indices at export time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExportScope {
    All,
    VisibleRows,
    SelectedRows { rows: Vec<usize> },
    SelectedColumns { columns: Vec<usize> },
    SelectedRange { rect: CellRect },
}

/// How to split an export across multiple output files (F04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SplitOptions {
    None,
    MaxRows { rows_per_file: usize },
    ApproximateBytes { max_bytes: u64 },
    GroupByColumn { column: usize },
}

/// Expected shape of a scoped export, shown before writing anything.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeCounts {
    pub rows: usize,
    pub cols: usize,
}

/// One output file recorded in an export manifest.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestOutput {
    pub file_name: String,
    pub rows: usize,
    pub sha256: String,
}

/// Optional JSON manifest written next to a scoped export (F04).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportManifest {
    pub source_file_name: Option<String>,
    pub source_fingerprint: Option<FileFingerprint>,
    pub scope: ExportScope,
    pub split: SplitOptions,
    pub options: ExportOptionsEcho,
    pub outputs: Vec<ManifestOutput>,
}

/// Serialization settings echoed into the manifest.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptionsEcho {
    pub delimiter: String,
    pub encoding: String,
    pub quote_style: String,
    pub line_ending: String,
    pub bom: bool,
    pub include_headers: bool,
}

impl From<&ExportOptions> for ExportOptionsEcho {
    fn from(o: &ExportOptions) -> Self {
        ExportOptionsEcho {
            delimiter: o.delimiter.clone(),
            encoding: o.encoding.clone(),
            quote_style: o.quote_style.clone(),
            line_ending: o.line_ending.clone(),
            bom: o.bom,
            include_headers: o.include_headers,
        }
    }
}

/// One cell (or header) whose text cannot be represented in a target encoding.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodingIncompatibility {
    /// Data-row index; `None` for a header cell.
    pub row: Option<usize>,
    pub col: usize,
    /// Truncated cell text for display.
    pub value: String,
}

/// Result of scanning the document for characters a target encoding cannot
/// represent (nothing is ever substituted silently).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodingCompatibility {
    pub encoding: String,
    pub compatible: bool,
    pub affected_cells: usize,
    /// First affected locations (capped).
    pub samples: Vec<EncodingIncompatibility>,
}
