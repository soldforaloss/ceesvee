//! Serializable data-transfer objects exchanged with the front end.
//!
//! Everything crossing the IPC boundary uses `camelCase` to match JS
//! conventions. Delimiters, encodings, line endings and quote styles are passed
//! as strings to keep the wire format simple and forward-compatible.

use serde::{Deserialize, Serialize};

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
}

/// Overrides supplied when (re)opening a file. Any `None` field is auto-detected.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenOptions {
    pub delimiter: Option<String>,
    pub encoding: Option<String>,
    pub has_header_row: Option<bool>,
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
#[derive(Debug, Clone, Copy, Deserialize)]
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

/// Options controlling how a document is serialized on save.
#[derive(Debug, Clone, Deserialize)]
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
}

fn default_true() -> bool {
    true
}
