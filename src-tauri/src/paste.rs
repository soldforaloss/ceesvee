//! Paste Special (F14): parse clipboard text into a block, apply the
//! selected block transforms (transpose, trim, pattern repeat), and compute
//! a preview — all without touching the document. The actual mutation goes
//! through [`crate::document::Document::paste_special`] as ONE undo step.

use serde::{Deserialize, Serialize};

use crate::delimiter;
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::parse::{parse, ParseSettings};

/// How pasted rows land in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PasteMode {
    /// Write over cells starting at the anchor, growing the grid as needed.
    Overwrite,
    /// Insert the block as NEW rows at the anchor row.
    InsertRows,
}

/// Options for a Paste Special operation (a closed, validated set).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteOptions {
    pub mode: PasteMode,
    /// Swap rows and columns before pasting.
    #[serde(default)]
    pub transpose: bool,
    /// Blank source cells leave the destination cell untouched (overwrite
    /// mode only).
    #[serde(default)]
    pub skip_blanks: bool,
    /// Trim whitespace from every incoming cell.
    #[serde(default)]
    pub trim: bool,
    /// Tile a smaller block across the selected destination rectangle.
    #[serde(default)]
    pub repeat_to_fill: bool,
    /// Use the first pasted row as HEADER names for the target columns.
    #[serde(default)]
    pub first_row_headers: bool,
}

/// What the preview reports before anything mutates.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePreview {
    /// Block dimensions after all transforms.
    pub rows: usize,
    pub cols: usize,
    /// Destination rectangle (display coordinates, before growth).
    pub target_row: usize,
    pub target_col: usize,
    /// Rows/columns the paste would ADD to the document.
    pub added_rows: usize,
    pub added_cols: usize,
    /// Header names that would change (first-row-headers mode).
    pub header_changes: Vec<String>,
    /// First rows of the final block, for the dialog.
    pub sample: Vec<Vec<String>>,
    pub warnings: Vec<String>,
}

/// Parse clipboard text into a rectangular block. Tab-delimited content
/// (Excel) is detected first; otherwise the standard sniffer runs. Quoted
/// fields, embedded newlines, and CRLF are handled by the CSV parser.
pub fn parse_clipboard(text: &str) -> AppResult<Vec<Vec<String>>> {
    if text.is_empty() {
        return Err(AppError::invalid("the clipboard is empty"));
    }
    // Excel and friends always use tabs; prefer them whenever present so a
    // comma inside a cell does not fool the sniffer.
    let delimiter = if text.contains('\t') {
        b'\t'
    } else {
        delimiter::detect(text)
    };
    let parsed = parse(
        text.as_bytes(),
        &ParseSettings {
            delimiter: Some(delimiter),
            encoding: Some(encoding_rs::UTF_8),
        },
    )?;
    if parsed.records.is_empty() {
        return Err(AppError::invalid("the clipboard contains no rows"));
    }
    Ok(parsed.records)
}

/// Apply the pure block transforms in a fixed order: trim, then transpose,
/// then pattern-repeat over the destination selection.
pub fn transform_block(
    mut block: Vec<Vec<String>>,
    options: &PasteOptions,
    selection_rows: usize,
    selection_cols: usize,
) -> Vec<Vec<String>> {
    if options.trim {
        for row in &mut block {
            for cell in row {
                let trimmed = cell.trim();
                if trimmed.len() != cell.len() {
                    *cell = trimmed.to_string();
                }
            }
        }
    }
    if options.transpose {
        block = transpose(block);
    }
    if options.repeat_to_fill && !block.is_empty() {
        let block_rows = block.len();
        let block_cols = block.iter().map(Vec::len).max().unwrap_or(0);
        // Only tile when the selection is strictly larger in some dimension
        // AND divisible-ish tiling makes sense; partial tiles are cut off.
        let out_rows = selection_rows.max(block_rows);
        let out_cols = selection_cols.max(block_cols);
        if out_rows > block_rows || out_cols > block_cols {
            let mut tiled = Vec::with_capacity(out_rows);
            for r in 0..out_rows {
                let src_row = &block[r % block_rows];
                let mut row = Vec::with_capacity(out_cols);
                for c in 0..out_cols {
                    row.push(src_row.get(c % block_cols).cloned().unwrap_or_default());
                }
                tiled.push(row);
            }
            block = tiled;
        }
    }
    block
}

fn transpose(block: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let rows = block.len();
    let cols = block.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = vec![vec![String::new(); rows]; cols];
    for (r, row) in block.into_iter().enumerate() {
        for (c, cell) in row.into_iter().enumerate() {
            out[c][r] = cell;
        }
    }
    out
}

/// Compute the preview for a paste at `anchor` (absolute coordinates).
pub fn preview(
    doc: &Document,
    block: &[Vec<String>],
    options: &PasteOptions,
    anchor_row: usize,
    anchor_col: usize,
) -> PastePreview {
    let mut warnings = Vec::new();
    let data_rows = if options.first_row_headers && !block.is_empty() {
        &block[1..]
    } else {
        block
    };
    let rows = data_rows.len();
    let cols = block.iter().map(Vec::len).max().unwrap_or(0);

    let widths: std::collections::HashSet<usize> = block.iter().map(Vec::len).collect();
    if widths.len() > 1 {
        warnings.push("Rows have differing widths; shorter rows pad with blanks".to_string());
    }

    let (added_rows, added_cols) = match options.mode {
        PasteMode::Overwrite => (
            (anchor_row + rows).saturating_sub(doc.n_rows()),
            (anchor_col + cols).saturating_sub(doc.n_cols()),
        ),
        PasteMode::InsertRows => (rows, (anchor_col + cols).saturating_sub(doc.n_cols())),
    };
    if options.skip_blanks && options.mode == PasteMode::InsertRows {
        warnings.push("Skip blanks has no effect when inserting new rows".to_string());
    }

    let mut header_changes = Vec::new();
    if options.first_row_headers {
        if let Some(first) = block.first() {
            for (i, name) in first.iter().enumerate() {
                let col = anchor_col + i;
                let current = doc.headers().get(col);
                if current.map(String::as_str) != Some(name.as_str()) {
                    header_changes.push(format!(
                        "{} → {}",
                        current
                            .cloned()
                            .unwrap_or_else(|| format!("Column {}", col + 1)),
                        name
                    ));
                }
            }
        }
    }

    PastePreview {
        rows,
        cols,
        target_row: anchor_row,
        target_col: anchor_col,
        added_rows,
        added_cols,
        header_changes,
        sample: data_rows.iter().take(10).cloned().collect(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse as parse_csv, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse_csv(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn options(mode: PasteMode) -> PasteOptions {
        PasteOptions {
            mode,
            transpose: false,
            skip_blanks: false,
            trim: false,
            repeat_to_fill: false,
            first_row_headers: false,
        }
    }

    #[test]
    fn clipboard_prefers_tabs_and_handles_quotes() {
        let block = parse_clipboard("a\tb\n\"x\ty\"\tz").unwrap();
        assert_eq!(block[0], vec!["a", "b"]);
        assert_eq!(block[1], vec!["x\ty", "z"]);
        let csv = parse_clipboard("a,b\n1,2").unwrap();
        assert_eq!(csv[1], vec!["1", "2"]);
        assert!(parse_clipboard("").is_err());
    }

    #[test]
    fn transpose_swaps_dimensions() {
        let block = vec![
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["1".to_string(), "2".to_string(), "3".to_string()],
        ];
        let mut opts = options(PasteMode::Overwrite);
        opts.transpose = true;
        let out = transform_block(block, &opts, 0, 0);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], vec!["a", "1"]);
        assert_eq!(out[2], vec!["c", "3"]);
    }

    #[test]
    fn repeat_tiles_a_smaller_pattern_over_the_selection() {
        let block = vec![vec!["x".to_string()], vec!["y".to_string()]];
        let mut opts = options(PasteMode::Overwrite);
        opts.repeat_to_fill = true;
        let out = transform_block(block, &opts, 5, 2);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0], vec!["x", "x"]);
        assert_eq!(out[1], vec!["y", "y"]);
        assert_eq!(out[4], vec!["x", "x"]);
    }

    #[test]
    fn trim_strips_incoming_whitespace_only() {
        let block = vec![vec!["  padded  ".to_string(), "clean".to_string()]];
        let mut opts = options(PasteMode::Overwrite);
        opts.trim = true;
        let out = transform_block(block, &opts, 0, 0);
        assert_eq!(out[0], vec!["padded", "clean"]);
    }

    #[test]
    fn preview_reports_growth_and_header_changes_without_mutation() {
        let d = doc("a,b\n1,2\n3,4");
        let revision = d.revision();
        let block = vec![
            vec!["NewA".to_string(), "NewB".to_string(), "NewC".to_string()],
            vec!["x".to_string(), "y".to_string(), "z".to_string()],
            vec!["p".to_string(), "q".to_string(), "r".to_string()],
        ];
        let mut opts = options(PasteMode::Overwrite);
        opts.first_row_headers = true;
        let p = preview(&d, &block, &opts, 1, 0);
        assert_eq!(p.rows, 2); // header row consumed
        assert_eq!(p.cols, 3);
        assert_eq!(p.added_rows, 1); // rows 1..3 exist? doc has 2 data rows; anchor 1 + 2 rows = 3 -> +1
        assert_eq!(p.added_cols, 1);
        assert_eq!(p.header_changes.len(), 3);
        assert_eq!(d.revision(), revision, "preview never mutates");
    }

    #[test]
    fn insert_mode_preview_counts_all_rows_as_added() {
        let d = doc("a\n1");
        let block = vec![vec!["x".to_string()], vec!["y".to_string()]];
        let p = preview(&d, &block, &options(PasteMode::InsertRows), 0, 0);
        assert_eq!(p.added_rows, 2);
        assert_eq!(p.added_cols, 0);
    }
}
