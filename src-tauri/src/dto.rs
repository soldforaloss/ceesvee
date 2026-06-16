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
    pub row_count: usize,
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
