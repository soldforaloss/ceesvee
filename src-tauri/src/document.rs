//! The in-memory, mutable document model: headers, data rows, dirty tracking and
//! a command-pattern undo/redo stack.
//!
//! Invariants maintained at all times:
//! * every row in `rows` has exactly `headers.len()` cells (the grid is
//!   rectangular);
//! * `headers.len()` is the authoritative column count.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::analyze;
use crate::dto::{
    CellRect, ColumnKind, ColumnSummary, DocumentMeta, NumericSummary, RowsResponse,
    SelectionStats, SortKey,
};
use crate::error::{AppError, AppResult};
use crate::parse::ParsedFile;

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
#[derive(Debug, Clone)]
struct CellEdit {
    row: usize,
    col: usize,
    old: String,
    new: String,
}

/// A removed column, captured for undo.
#[derive(Debug, Clone)]
struct RemovedColumn {
    index: usize,
    header: String,
    values: Vec<String>,
}

/// A single reversible edit. Structural ops capture exactly what they need to
/// undo without snapshotting the whole document.
#[derive(Debug, Clone)]
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
    /// `undo_stack.len()` at the last save; the document is dirty when it differs.
    saved_marker: usize,
    /// Absolute indices of the rows matching the active filter, in order; `None`
    /// when unfiltered. A non-undoable view: recomputed on `set_filter` and
    /// cleared by structural mutations (handled in the command layer).
    filter_view: Option<Vec<usize>>,
    /// Monotonically increasing revision, bumped on every change that could
    /// invalidate a deferred operation: data and structural mutations,
    /// undo/redo, header-mode toggles and filter-view changes. Previews and
    /// long-running results carry the revision they were computed against and
    /// are rejected when it no longer matches (see [`Document::check_revision`]).
    revision: u64,
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
            saved_marker: 0,
            filter_view: None,
            revision: 1,
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
            saved_marker: 0,
            filter_view: None,
            revision: 1,
        }
    }

    // ----- accessors -------------------------------------------------------

    pub fn n_cols(&self) -> usize {
        self.headers.len()
    }

    pub fn n_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    pub fn has_header_row(&self) -> bool {
        self.has_header_row
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

    // ----- row filter view -------------------------------------------------

    /// Visible row count: the filtered count when a filter is active, else the
    /// full row count.
    pub fn visible_len(&self) -> usize {
        self.filter_view
            .as_ref()
            .map(Vec::len)
            .unwrap_or(self.rows.len())
    }

    /// The active filter's matching absolute row indices, in order, if any.
    pub fn filter_view(&self) -> Option<&[usize]> {
        self.filter_view.as_deref()
    }

    /// Replace the active filter with a precomputed view (absolute row indices).
    pub fn set_filter(&mut self, view: Vec<usize>) {
        self.filter_view = Some(view);
        // The visible-row set is an input to scoped previews, so changing it
        // must invalidate them.
        self.revision += 1;
    }

    pub fn clear_filter(&mut self) {
        if self.filter_view.take().is_some() {
            self.revision += 1;
        }
    }

    /// Translate a visible (display) row index to its absolute index. Identity
    /// when unfiltered; `None` if the display index is past the visible range.
    pub fn display_to_abs(&self, display: usize) -> Option<usize> {
        match &self.filter_view {
            Some(view) => view.get(display).copied(),
            None => (display < self.rows.len()).then_some(display),
        }
    }

    /// Like [`Document::display_to_abs`], but a display index equal to the
    /// visible length maps to the end of the document so a paste/insert at the
    /// bottom can append.
    pub fn display_to_abs_insert(&self, display: usize) -> Option<usize> {
        if display == self.visible_len() {
            return Some(self.rows.len());
        }
        self.display_to_abs(display)
    }

    pub fn is_dirty(&self) -> bool {
        self.undo_stack.len() != self.saved_marker
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
    pub fn get_rows(&self, start: usize, count: usize) -> RowsResponse {
        let visible = self.visible_len();
        let start = start.min(visible);
        let end = start.saturating_add(count).min(visible);
        // Map a display-row index to its absolute index (identity when unfiltered).
        let abs_at = |display: usize| -> usize {
            match &self.filter_view {
                Some(view) => view[display],
                None => display,
            }
        };
        let rows: Vec<Vec<String>> = (start..end).map(|d| self.rows[abs_at(d)].clone()).collect();
        let dirty: Vec<Vec<bool>> = (start..end)
            .map(|d| {
                let r = abs_at(d);
                (0..self.headers.len())
                    .map(|c| self.dirty_cells.contains(&(r, c)))
                    .collect()
            })
            .collect();
        RowsResponse { start, rows, dirty }
    }

    /// Aggregate numeric statistics over a rectangular selection (data-row
    /// coordinates). Computed in Rust so it scales to any selection size.
    pub fn selection_stats(&self, rect: CellRect) -> SelectionStats {
        let row_end = rect.y.saturating_add(rect.height).min(self.visible_len());
        let col_end = rect.x.saturating_add(rect.width).min(self.headers.len());

        let mut count = 0usize;
        let mut numeric_count = 0usize;
        let mut sum = 0.0f64;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;

        for d in rect.y..row_end {
            // Resolve display row to absolute (identity when unfiltered).
            let r = match &self.filter_view {
                Some(view) => view[d],
                None => d,
            };
            for c in rect.x..col_end {
                count += 1;
                if let Some(n) = analyze::as_number(&self.rows[r][c]) {
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
        }

        let has_numeric = numeric_count > 0;
        SelectionStats {
            count,
            numeric_count,
            sum,
            avg: has_numeric.then(|| sum / numeric_count as f64),
            min: has_numeric.then_some(min),
            max: has_numeric.then_some(max),
        }
    }

    /// Detect the type of, and summarise, every column over all data rows.
    /// Computed in Rust because the front end only caches the visible window;
    /// recomputed on demand (no cache, so it can never go stale after an edit).
    pub fn column_summaries(&self) -> Vec<ColumnSummary> {
        (0..self.headers.len())
            .map(|c| self.column_summary(c))
            .collect()
    }

    /// Type detection + summary for a single column.
    pub fn column_summary(&self, col: usize) -> ColumnSummary {
        let count = self.rows.len();
        let mut nulls = 0usize;
        let mut numeric = 0usize;
        let mut booly = 0usize;
        let mut datey = 0usize;
        let mut unique: HashSet<&str> = HashSet::new();
        let mut sum = 0.0f64;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;

        for row in &self.rows {
            let trimmed = row.get(col).map(|s| s.trim()).unwrap_or("");
            if trimmed.is_empty() {
                nulls += 1;
                continue;
            }
            unique.insert(trimmed);
            if let Some(n) = analyze::as_number(trimmed) {
                numeric += 1;
                sum += n;
                if n < min {
                    min = n;
                }
                if n > max {
                    max = n;
                }
            } else if analyze::is_bool(trimmed) {
                booly += 1;
            } else if analyze::is_date(trimmed) {
                datey += 1;
            }
        }

        // A column takes a non-text kind only when *every* non-empty cell
        // matches it; otherwise it is text (blanks never decide the kind).
        let non_empty = count - nulls;
        let kind = if non_empty == 0 {
            ColumnKind::Text
        } else if numeric == non_empty {
            ColumnKind::Number
        } else if booly == non_empty {
            ColumnKind::Bool
        } else if datey == non_empty {
            ColumnKind::Date
        } else {
            ColumnKind::Text
        };

        let numeric_summary = (numeric > 0).then_some(NumericSummary {
            min,
            max,
            mean: sum / numeric as f64,
        });

        ColumnSummary {
            column: col,
            kind,
            count,
            nulls,
            unique: unique.len(),
            numeric: numeric_summary,
        }
    }

    pub fn meta(&self) -> DocumentMeta {
        let file_name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string());

        DocumentMeta {
            id: self.id,
            path: self.path.as_ref().map(|p| p.to_string_lossy().to_string()),
            file_name,
            row_count: self.visible_len(),
            total_row_count: self.rows.len(),
            filtered: self.filter_view.is_some(),
            col_count: self.headers.len(),
            headers: self.headers.clone(),
            has_header_row: self.has_header_row,
            delimiter: String::from_utf8_lossy(&[self.delimiter]).to_string(),
            encoding: self.encoding_name.clone(),
            had_bom: self.had_bom,
            line_ending: self.line_ending.as_str().to_string(),
            dirty: self.is_dirty(),
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            revision: self.revision,
        }
    }

    /// Mark the current state as saved (clears the dirty indicator and the
    /// dirty-cell highlights). `path` updates on Save As.
    pub fn mark_saved(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            self.path = Some(p);
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
        for &(row, col, _) in &changes {
            self.check_cell(row, col)?;
        }
        if let Some(op) = self.op_set_cells(changes) {
            self.register(op);
        }
        Ok(())
    }

    pub fn insert_rows(&mut self, at: usize, count: usize) -> AppResult<()> {
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
        if at > self.headers.len() {
            return Err(AppError::invalid("column index out of range"));
        }
        let op = self.op_insert_column(at, name);
        self.register(op);
        Ok(())
    }

    pub fn delete_columns(&mut self, mut indices: Vec<usize>) -> AppResult<()> {
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

    /// Sort rows by one or more keys. Empty `keys` is a no-op.
    pub fn sort(&mut self, keys: &[SortKey]) -> AppResult<()> {
        if keys.is_empty() || self.rows.len() < 2 {
            return Ok(());
        }
        for key in keys {
            if key.column >= self.headers.len() {
                return Err(AppError::invalid("sort column out of range"));
            }
        }

        let mut order: Vec<u32> = (0..self.rows.len() as u32).collect();
        order.sort_by(|&a, &b| {
            crate::sort::compare_rows(&self.rows[a as usize], &self.rows[b as usize], keys)
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
    pub fn set_header_mode(&mut self, has_header: bool) {
        if has_header == self.has_header_row {
            return;
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
        self.dirty_cells.clear();
        self.saved_marker = usize::MAX;
        self.revision += 1;
    }

    pub fn undo(&mut self) -> AppResult<()> {
        let op = self.undo_stack.pop().ok_or(AppError::NothingToUndo)?;
        self.revert(&op);
        self.redo_stack.push(op);
        self.revision += 1;
        Ok(())
    }

    pub fn redo(&mut self) -> AppResult<()> {
        let op = self.redo_stack.pop().ok_or(AppError::NothingToRedo)?;
        self.apply(&op);
        self.undo_stack.push(op);
        self.revision += 1;
        Ok(())
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
        self.undo_stack.push(op);
        self.redo_stack.clear();
        self.revision += 1;
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

    fn op_insert_column(&mut self, at: usize, name: String) -> EditOp {
        let op = EditOp::InsertColumn { at, name };
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
            EditOp::InsertColumn { at, name } => {
                self.headers.insert(*at, name.clone());
                for row in &mut self.rows {
                    row.insert(*at, String::new());
                }
                self.remap_dirty_cols_inserted(*at, 1);
            }
            EditOp::DeleteColumns { removed } => {
                let indices: Vec<usize> = removed.iter().map(|c| c.index).collect();
                for &i in indices.iter().rev() {
                    self.headers.remove(i);
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
                for row in &mut self.rows {
                    row.remove(*at);
                }
                self.remap_dirty_cols_removed(&[*at]);
            }
            EditOp::DeleteColumns { removed } => {
                for col in removed.iter() {
                    self.headers.insert(col.index, col.header.clone());
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
        d.set_header_mode(false);
        assert_eq!(d.n_rows(), 2);
        assert_eq!(d.cell(0, 0), "a");
        d.set_header_mode(true);
        assert_eq!(d.headers(), &["a", "b"]);
        assert_eq!(d.n_rows(), 1);
    }

    #[test]
    fn dirty_cell_follows_sort() {
        let mut d = doc_from("n\n3\n1\n2", true);
        d.set_cell(0, 0, "9".into()).unwrap(); // row with value 3 -> 9
        let win = d.get_rows(0, 3);
        assert!(win.dirty[0][0]);
        d.sort(&[SortKey {
            column: 0,
            descending: false,
        }])
        .unwrap();
        // "9" sorts last; its dirty flag should travel with it.
        let win = d.get_rows(0, 3);
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
        let stats = d.selection_stats(CellRect {
            x: 0,
            y: 0,
            width: 2,
            height: 3,
        });
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
        let stats = d.selection_stats(CellRect {
            x: 0,
            y: 0,
            width: 10,
            height: 100,
        });
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
        assert_bumps(&mut d, "set_header_mode", |d| d.set_header_mode(false));
        assert_bumps(&mut d, "set_filter", |d| d.set_filter(vec![0]));
        assert_bumps(&mut d, "clear_filter", |d| d.clear_filter());
    }

    #[test]
    fn revision_unchanged_by_noops_and_saves() {
        let mut d = doc_from("a\n1", true);
        let r = d.revision();
        // Writing the identical value registers no edit.
        d.set_cell(0, 0, "1".into()).unwrap();
        assert_eq!(d.revision(), r);
        // Clearing a filter that isn't set changes nothing.
        d.clear_filter();
        assert_eq!(d.revision(), r);
        // Toggling the header mode to its current value changes nothing.
        d.set_header_mode(true);
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
    fn set_revision_continues_sequence_across_reparse() {
        let mut replacement = doc_from("a\n9", true);
        replacement.set_revision(41);
        assert_eq!(replacement.revision(), 41);
        replacement.set_cell(0, 0, "8".into()).unwrap();
        assert_eq!(replacement.revision(), 42);
    }
}
