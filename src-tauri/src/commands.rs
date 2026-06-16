//! The Tauri command surface. The front end drives every interaction through
//! these; heavy file I/O and parsing run off the UI thread.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use tauri::State;

use crate::document::Document;
use crate::dto::{
    CellRect, ColumnSummary, DocumentMeta, ExportOptions, FilterGroup, FindMatch, FindOptions,
    OpenOptions, ReplaceResult, RowsResponse, SelectionStats, SortKey,
};
use crate::error::{AppError, AppResult};
use crate::parse::{parse, ParseSettings, ParsedFile};
use crate::state::{AppState, PendingFiles};
use crate::{encoding, export, filter as filter_mod, find as find_mod, util};

type Db<'a> = State<'a, Mutex<AppState>>;

fn lock<'g>(state: &'g Db<'_>) -> AppResult<MutexGuard<'g, AppState>> {
    state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))
}

/// Translate a visible (display) row index to its absolute index, erroring if
/// out of range. Identity when no filter is active.
fn abs_row(doc: &Document, display: usize) -> AppResult<usize> {
    doc.display_to_abs(display)
        .ok_or_else(|| AppError::invalid("row index out of range"))
}

/// Like [`abs_row`] but allows a display index at the end (for append/insert).
fn abs_insert_row(doc: &Document, display: usize) -> AppResult<usize> {
    doc.display_to_abs_insert(display)
        .ok_or_else(|| AppError::invalid("row index out of range"))
}

/// Heuristic: treat the first row as a header when none of its cells is numeric.
fn looks_like_header(records: &[Vec<String>]) -> bool {
    match records.first() {
        None => false,
        Some(first) => first.iter().all(|cell| {
            let trimmed = cell.trim();
            trimmed.is_empty() || trimmed.parse::<f64>().is_err()
        }),
    }
}

// ----- open / new / close ------------------------------------------------

#[tauri::command]
pub async fn open_file(
    path: String,
    options: Option<OpenOptions>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let options = options.unwrap_or_default();
    let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
    let opt_enc = options.encoding.as_deref().map(encoding::from_name);
    let forced_header = options.has_header_row;
    let read_path = PathBuf::from(&path);

    let parsed = tauri::async_runtime::spawn_blocking(move || -> AppResult<ParsedFile> {
        let bytes = std::fs::read(&read_path)?;
        let settings = ParseSettings {
            delimiter: opt_delim,
            encoding: opt_enc,
        };
        parse(&bytes, &settings)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))??;

    let has_header = forced_header.unwrap_or_else(|| looks_like_header(&parsed.records));

    let mut guard = lock(&state)?;
    let id = guard.alloc_id();
    let doc = Document::from_parsed(id, Some(PathBuf::from(&path)), parsed, has_header);
    let meta = doc.meta();
    guard.insert(doc);
    Ok(meta)
}

/// Re-read the document's file with new delimiter/encoding/header overrides.
#[tauri::command]
pub async fn reparse(doc_id: u64, options: OpenOptions, state: Db<'_>) -> AppResult<DocumentMeta> {
    let (path, current_header) = {
        let guard = lock(&state)?;
        let doc = guard.get(doc_id)?;
        let path = doc
            .path
            .clone()
            .ok_or_else(|| AppError::invalid("document has no file to reparse"))?;
        (path, doc.has_header_row())
    };

    let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
    let opt_enc = options.encoding.as_deref().map(encoding::from_name);
    let forced_header = options.has_header_row.or(Some(current_header));
    let read_path = path.clone();

    let parsed = tauri::async_runtime::spawn_blocking(move || -> AppResult<ParsedFile> {
        let bytes = std::fs::read(&read_path)?;
        let settings = ParseSettings {
            delimiter: opt_delim,
            encoding: opt_enc,
        };
        parse(&bytes, &settings)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))??;

    let has_header = forced_header.unwrap_or_else(|| looks_like_header(&parsed.records));

    let mut guard = lock(&state)?;
    let doc = Document::from_parsed(doc_id, Some(path), parsed, has_header);
    let meta = doc.meta();
    *guard.get_mut(doc_id)? = doc;
    Ok(meta)
}

#[tauri::command]
pub fn new_document(
    rows: Option<usize>,
    cols: Option<usize>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let id = guard.alloc_id();
    let doc = Document::new_empty(id, cols.unwrap_or(4), rows.unwrap_or(50));
    let meta = doc.meta();
    guard.insert(doc);
    Ok(meta)
}

#[tauri::command]
pub fn close_document(doc_id: u64, state: Db<'_>) -> AppResult<()> {
    lock(&state)?.remove(doc_id);
    Ok(())
}

#[tauri::command]
pub fn get_meta(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    Ok(lock(&state)?.get(doc_id)?.meta())
}

#[tauri::command]
pub fn list_encodings() -> Vec<String> {
    encoding::SUPPORTED.iter().map(|s| s.to_string()).collect()
}

/// Drain and return any files passed at launch (e.g. via "Open with CEESVEE").
#[tauri::command]
pub fn take_pending_files(pending: State<'_, PendingFiles>) -> Vec<String> {
    pending
        .0
        .lock()
        .map(|mut files| std::mem::take(&mut *files))
        .unwrap_or_default()
}

// ----- windowed reads ----------------------------------------------------

#[tauri::command]
pub fn get_rows(doc_id: u64, start: usize, count: usize, state: Db<'_>) -> AppResult<RowsResponse> {
    Ok(lock(&state)?.get(doc_id)?.get_rows(start, count))
}

#[tauri::command]
pub fn selection_stats(doc_id: u64, rect: CellRect, state: Db<'_>) -> AppResult<SelectionStats> {
    Ok(lock(&state)?.get(doc_id)?.selection_stats(rect))
}

#[tauri::command]
pub fn column_summaries(doc_id: u64, state: Db<'_>) -> AppResult<Vec<ColumnSummary>> {
    Ok(lock(&state)?.get(doc_id)?.column_summaries())
}

// ----- cell editing ------------------------------------------------------

#[tauri::command]
pub fn set_cell(
    doc_id: u64,
    row: usize,
    col: usize,
    value: String,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let abs = abs_row(doc, row)?;
    doc.set_cell(abs, col, value)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn set_cells(
    doc_id: u64,
    changes: Vec<(usize, usize, String)>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let mut translated = Vec::with_capacity(changes.len());
    for (row, col, value) in changes {
        translated.push((abs_row(doc, row)?, col, value));
    }
    doc.set_cells(translated)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn paste(
    doc_id: u64,
    anchor_row: usize,
    anchor_col: usize,
    block: Vec<Vec<String>>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    // Pasting can grow/reshape the grid, so it drops any active filter and
    // operates on the absolute anchor position.
    let abs = abs_insert_row(doc, anchor_row)?;
    doc.clear_filter();
    doc.paste(abs, anchor_col, block)?;
    Ok(doc.meta())
}

// ----- row operations ----------------------------------------------------

#[tauri::command]
pub fn insert_rows(doc_id: u64, at: usize, count: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let abs = abs_insert_row(doc, at)?;
    doc.clear_filter();
    doc.insert_rows(abs, count)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn delete_rows(doc_id: u64, indices: Vec<usize>, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let mut abs = Vec::with_capacity(indices.len());
    for d in indices {
        abs.push(abs_row(doc, d)?);
    }
    doc.clear_filter();
    doc.delete_rows(abs)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn move_row(doc_id: u64, from: usize, to: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let from_abs = abs_row(doc, from)?;
    let to_abs = abs_row(doc, to)?;
    doc.clear_filter();
    doc.move_row(from_abs, to_abs)?;
    Ok(doc.meta())
}

// ----- column operations -------------------------------------------------

#[tauri::command]
pub fn insert_column(
    doc_id: u64,
    at: usize,
    name: String,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    // Column structure shifts the indices a filter references, so drop it.
    doc.clear_filter();
    doc.insert_column(at, name)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn delete_columns(doc_id: u64, indices: Vec<usize>, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    doc.clear_filter();
    doc.delete_columns(indices)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn rename_column(
    doc_id: u64,
    col: usize,
    name: String,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    doc.rename_column(col, name)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn move_column(doc_id: u64, from: usize, to: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    doc.clear_filter();
    doc.move_column(from, to)?;
    Ok(doc.meta())
}

// ----- analysis ----------------------------------------------------------

#[tauri::command]
pub fn sort(doc_id: u64, keys: Vec<SortKey>, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    // Sorting reorders all rows, invalidating a filter view; drop it.
    doc.clear_filter();
    doc.sort(&keys)?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn set_header_mode(doc_id: u64, has_header: bool, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    // Re-interpreting the header row shifts all row indices; drop any filter.
    doc.clear_filter();
    doc.set_header_mode(has_header);
    Ok(doc.meta())
}

#[tauri::command]
pub fn find(doc_id: u64, options: FindOptions, state: Db<'_>) -> AppResult<Vec<FindMatch>> {
    let guard = lock(&state)?;
    let doc = guard.get(doc_id)?;
    find_mod::find(doc, &options)
}

#[tauri::command]
pub fn replace_all(
    doc_id: u64,
    options: FindOptions,
    replacement: String,
    state: Db<'_>,
) -> AppResult<ReplaceResult> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let changes = find_mod::replace_all(doc, &options, &replacement)?;
    let replaced = changes.len();
    doc.set_cells(changes)?;
    Ok(ReplaceResult {
        replaced,
        meta: doc.meta(),
    })
}

// ----- filtering ---------------------------------------------------------

#[tauri::command]
pub fn set_filter(doc_id: u64, spec: FilterGroup, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    let view = filter_mod::matching_rows(doc, &spec)?;
    doc.set_filter(view);
    Ok(doc.meta())
}

#[tauri::command]
pub fn clear_filter(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    doc.clear_filter();
    Ok(doc.meta())
}

// ----- history -----------------------------------------------------------

#[tauri::command]
pub fn undo(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    // Undo/redo may reinstate rows the filter view doesn't account for, so the
    // view is dropped to keep coordinates consistent.
    doc.clear_filter();
    doc.undo()?;
    Ok(doc.meta())
}

#[tauri::command]
pub fn redo(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    doc.clear_filter();
    doc.redo()?;
    Ok(doc.meta())
}

// ----- save --------------------------------------------------------------

#[tauri::command]
pub async fn save(
    doc_id: u64,
    path: String,
    options: ExportOptions,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    // Serialize while briefly holding the lock (CPU work, no await inside).
    let bytes = {
        let guard = lock(&state)?;
        let doc = guard.get(doc_id)?;
        export::serialize(doc, &options)?
    };

    // Write to disk off the UI thread.
    let write_path = PathBuf::from(&path);
    tauri::async_runtime::spawn_blocking(move || std::fs::write(&write_path, &bytes))
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))??;

    // Record the save point.
    let mut guard = lock(&state)?;
    let doc = guard.get_mut(doc_id)?;
    doc.mark_saved(Some(PathBuf::from(&path)));
    Ok(doc.meta())
}
