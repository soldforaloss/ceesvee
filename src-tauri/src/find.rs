//! Find and replace over a document's data cells.
//!
//! Both plain and regex modes are implemented on top of the `regex` crate: in
//! plain mode the query is escaped, so the same matching/replacement engine
//! serves both. Search can be scoped to a rectangular selection.

use regex::{Regex, RegexBuilder};

use crate::document::Document;
use crate::dto::{FindMatch, FindOptions};
use crate::error::{AppError, AppResult};

/// Compile the user's query into a [`Regex`], honouring case sensitivity,
/// whole-cell anchoring and plain-vs-regex mode.
fn build_regex(opts: &FindOptions) -> AppResult<Regex> {
    if opts.query.is_empty() {
        return Err(AppError::invalid("search query is empty"));
    }
    let base = if opts.regex {
        opts.query.clone()
    } else {
        regex::escape(&opts.query)
    };
    let pattern = if opts.whole_cell {
        format!("^(?:{base})$")
    } else {
        base
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!opts.case_sensitive)
        .build()
        .map_err(|e| AppError::invalid(format!("invalid regular expression: {e}")))
}

/// Bounds (row/col ranges) to scan, derived from an optional selection. Row
/// bounds are in DISPLAY (visible) coordinates so they respect an active filter.
fn scan_bounds(doc: &Document, opts: &FindOptions) -> (usize, usize, usize, usize) {
    let visible = doc.visible_len();
    match opts.selection {
        Some(rect) => {
            let row_start = rect.y.min(visible);
            let row_end = rect.y.saturating_add(rect.height).min(visible);
            let col_start = rect.x.min(doc.n_cols());
            let col_end = rect.x.saturating_add(rect.width).min(doc.n_cols());
            (row_start, row_end, col_start, col_end)
        }
        None => (0, visible, 0, doc.n_cols()),
    }
}

/// Map a visible (display) row index to its absolute index (identity unfiltered).
fn abs(view: Option<&[usize]>, display: usize) -> usize {
    match view {
        Some(v) => v[display],
        None => display,
    }
}

/// Return every matching cell. Rows are reported in DISPLAY coordinates (what
/// the grid sees), so a find under an active filter scrolls to the right cell.
pub fn find(doc: &Document, opts: &FindOptions) -> AppResult<Vec<FindMatch>> {
    let re = build_regex(opts)?;
    let (row_start, row_end, col_start, col_end) = scan_bounds(doc, opts);
    let rows = doc.rows();
    let view = doc.filter_view();

    let mut matches = Vec::new();
    for disp in row_start..row_end {
        let row = &rows[abs(view, disp)];
        for (c, cell) in row.iter().enumerate().take(col_end).skip(col_start) {
            if re.is_match(cell) {
                matches.push(FindMatch { row: disp, col: c });
            }
        }
    }
    Ok(matches)
}

/// Compute the cell changes for a replace-all. Returns `(row, col, new_value)`
/// tuples for cells whose value actually changes; the caller applies them as a
/// single undoable batch.
pub fn replace_all(
    doc: &Document,
    opts: &FindOptions,
    replacement: &str,
) -> AppResult<Vec<(usize, usize, String)>> {
    let re = build_regex(opts)?;
    let (row_start, row_end, col_start, col_end) = scan_bounds(doc, opts);
    let rows = doc.rows();
    let view = doc.filter_view();

    // Changes are returned in ABSOLUTE coordinates because the caller applies
    // them via `Document::set_cells`, which works in absolute space.
    let mut changes = Vec::new();
    for disp in row_start..row_end {
        let r = abs(view, disp);
        let row = &rows[r];
        for (c, cell) in row.iter().enumerate().take(col_end).skip(col_start) {
            if !re.is_match(cell) {
                continue;
            }
            // In plain mode, treat the replacement literally ($ is not a group
            // reference). In regex mode, allow `$1` / `${name}` expansion.
            let new_value = if opts.regex {
                re.replace_all(cell, replacement).into_owned()
            } else {
                re.replace_all(cell, regex::NoExpand(replacement))
                    .into_owned()
            };
            if &new_value != cell {
                changes.push((r, c, new_value));
            }
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::CellRect;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn opts(query: &str) -> FindOptions {
        FindOptions {
            query: query.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn plain_case_insensitive_substring() {
        let d = doc("a,b\nHello,world\nfoo,HELLO");
        let m = find(&d, &opts("hello")).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn case_sensitive_matches_exact_case() {
        let d = doc("a,b\nHello,hello");
        let o = FindOptions {
            query: "hello".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let m = find(&d, &o).unwrap();
        assert_eq!(m, vec![FindMatch { row: 0, col: 1 }]);
    }

    #[test]
    fn whole_cell_only() {
        let d = doc("a\ncat\ncatalog");
        let o = FindOptions {
            query: "cat".into(),
            whole_cell: true,
            ..Default::default()
        };
        let m = find(&d, &o).unwrap();
        assert_eq!(m, vec![FindMatch { row: 0, col: 0 }]);
    }

    #[test]
    fn regex_mode() {
        let d = doc("a\nfoo123\nbar\nbaz9");
        let o = FindOptions {
            query: r"\d+".into(),
            regex: true,
            ..Default::default()
        };
        let m = find(&d, &o).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn replace_plain_is_literal() {
        let d = doc("a\nprice $5\ncost $5");
        let o = opts("$5");
        let changes = replace_all(&d, &o, "$10").unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].2, "price $10");
    }

    #[test]
    fn replace_regex_with_capture() {
        let d = doc("a\n2026-01-02");
        let o = FindOptions {
            query: r"(\d{4})-(\d{2})-(\d{2})".into(),
            regex: true,
            ..Default::default()
        };
        let changes = replace_all(&d, &o, "$3/$2/$1").unwrap();
        assert_eq!(changes[0].2, "02/01/2026");
    }

    #[test]
    fn scoped_to_selection() {
        let d = doc("a,b,c\nx,x,x\nx,x,x");
        let o = FindOptions {
            query: "x".into(),
            selection: Some(CellRect {
                x: 0,
                y: 0,
                width: 1,
                height: 2,
            }),
            ..Default::default()
        };
        let m = find(&d, &o).unwrap();
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|hit| hit.col == 0));
    }
}
