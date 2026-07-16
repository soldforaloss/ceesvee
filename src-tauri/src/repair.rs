//! Missing-value repair (F29): controlled, previewed methods for filling,
//! normalizing, or removing missing data. A closed operation set — no
//! expressions. Statistics are computed over the selected scope only,
//! invalid numeric cells are ignored for math but counted for the report,
//! and every application is ONE undo step that restores the exact original
//! representations (including null tokens) on undo.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;

/// Examples included in a preview.
const EXAMPLE_LIMIT: usize = 20;

/// The closed set of repair operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RepairOp {
    /// Replace configured null tokens (exact match on the trimmed cell)
    /// with a true blank. Undo restores the exact original tokens.
    NormalizeNullTokens { tokens: Vec<String> },
    /// Fill blank cells with a constant.
    FillConstant { value: String },
    /// Copy the last non-blank value downward. With `group_columns`, the
    /// carried value resets whenever the group key changes — a fill NEVER
    /// crosses a group boundary.
    FillForward { group_columns: Vec<usize> },
    /// Copy the next non-blank value upward (same grouping semantics).
    FillBackward { group_columns: Vec<usize> },
    /// Fill blanks with the scope mean of the column's numeric values.
    FillMean,
    /// Fill blanks with the scope median.
    FillMedian,
    /// Fill blanks with the most frequent non-blank value; ties resolve to
    /// the lexicographically smallest candidate (documented, deterministic).
    FillMode,
    /// Linear interpolation between the surrounding known numeric values
    /// (by absolute row distance). Leading/trailing blanks stay untouched
    /// unless `extrapolate`, which extends the nearest known value.
    Interpolate {
        #[serde(default)]
        extrapolate: bool,
    },
    /// Remove rows where at least `threshold` (0..=1) of the target columns
    /// are blank. Requires explicit confirmation in the UI.
    RemoveRows { threshold: f64 },
    /// Remove target columns where at least `threshold` of scope rows are
    /// blank. Requires explicit confirmation in the UI.
    RemoveColumns { threshold: f64 },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairSpec {
    pub op: RepairOp,
    /// Target columns (the cells examined and repaired).
    pub columns: Vec<usize>,
    /// Which rows participate (all / visible / selected). Rows outside the
    /// scope are never modified and never contribute to statistics.
    pub scope: ExportScope,
}

/// One before/after example for the preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairExample {
    pub row: usize,
    pub col: usize,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairPreview {
    pub revision: u64,
    pub cells_affected: usize,
    pub rows_removed: usize,
    pub columns_removed: usize,
    /// (column, computed fill value) for the statistical fills.
    pub fill_values: Vec<(usize, String)>,
    /// Non-blank cells that could not be read as numbers (ignored by the
    /// statistics, but reported so the user knows they exist).
    pub invalid_numeric: usize,
    /// First before/after examples (cell operations only).
    pub examples: Vec<RepairExample>,
}

/// Everything a repair would do, computed without mutating.
pub struct RepairComputed {
    pub changes: Vec<(usize, usize, String)>,
    pub remove_rows: Vec<usize>,
    pub remove_columns: Vec<usize>,
    pub preview: RepairPreview,
}

fn validate(doc: &Document, spec: &RepairSpec) -> AppResult<()> {
    if spec.columns.is_empty() {
        return Err(AppError::invalid("pick at least one target column"));
    }
    if let Some(&bad) = spec.columns.iter().find(|&&c| c >= doc.n_cols()) {
        return Err(AppError::invalid(format!("column {bad} is out of range")));
    }
    match &spec.op {
        RepairOp::NormalizeNullTokens { tokens } => {
            if tokens.iter().all(|t| t.trim().is_empty()) {
                return Err(AppError::invalid("add at least one null token"));
            }
        }
        RepairOp::FillForward { group_columns } | RepairOp::FillBackward { group_columns } => {
            if let Some(&bad) = group_columns.iter().find(|&&c| c >= doc.n_cols()) {
                return Err(AppError::invalid(format!(
                    "grouping column {bad} is out of range"
                )));
            }
        }
        RepairOp::RemoveRows { threshold } | RepairOp::RemoveColumns { threshold }
            if !threshold.is_finite() || !(0.0..=1.0).contains(threshold) =>
        {
            return Err(AppError::invalid("threshold must be between 0 and 1"));
        }
        _ => {}
    }
    Ok(())
}

fn is_blank(cell: &str) -> bool {
    cell.trim().is_empty()
}

/// Render a computed statistic the way the rest of the app renders numbers.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// Compute the exact effect of a repair. Read-only; the caller commits the
/// result through `set_cells` / `delete_rows` / `delete_columns`.
pub fn compute(doc: &Document, spec: &RepairSpec) -> AppResult<RepairComputed> {
    validate(doc, spec)?;
    let rows = resolve_scope(doc, &spec.scope)?.rows;
    let cols = &spec.columns;

    // Snapshot the scope once: (abs row, target cells, group key cells).
    let group_columns: &[usize] = match &spec.op {
        RepairOp::FillForward { group_columns } | RepairOp::FillBackward { group_columns } => {
            group_columns
        }
        _ => &[],
    };
    let mut cells: Vec<(usize, Vec<String>)> = Vec::with_capacity(rows.len());
    let mut keys: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    doc.visit_rows_at(&rows, &mut |i, row| {
        cells.push((
            i,
            cols.iter()
                .map(|&c| row.get(c).cloned().unwrap_or_default())
                .collect(),
        ));
        keys.push(
            group_columns
                .iter()
                .map(|&c| row.get(c).map(|v| v.trim().to_string()).unwrap_or_default())
                .collect(),
        );
        Ok(true)
    })?;

    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut remove_rows: Vec<usize> = Vec::new();
    let mut remove_columns: Vec<usize> = Vec::new();
    let mut fill_values: Vec<(usize, String)> = Vec::new();
    let mut invalid_numeric = 0usize;

    // Numeric values per target column (for the statistical fills).
    let mut column_numbers = |ci: usize| -> Vec<f64> {
        let mut out = Vec::new();
        for (_, row_cells) in &cells {
            let cell = &row_cells[ci];
            if is_blank(cell) {
                continue;
            }
            match analyze::as_number(cell.trim()) {
                Some(n) => out.push(n),
                None => invalid_numeric += 1,
            }
        }
        out
    };

    match &spec.op {
        RepairOp::NormalizeNullTokens { tokens } => {
            let tokens: Vec<&str> = tokens
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect();
            for (abs, row_cells) in &cells {
                for (ci, cell) in row_cells.iter().enumerate() {
                    if tokens.contains(&cell.trim()) {
                        changes.push((*abs, cols[ci], String::new()));
                    }
                }
            }
        }
        RepairOp::FillConstant { value } => {
            for (abs, row_cells) in &cells {
                for (ci, cell) in row_cells.iter().enumerate() {
                    if is_blank(cell) && !value.is_empty() {
                        changes.push((*abs, cols[ci], value.clone()));
                    }
                }
            }
        }
        RepairOp::FillForward { .. } | RepairOp::FillBackward { .. } => {
            let backward = matches!(spec.op, RepairOp::FillBackward { .. });
            let order: Vec<usize> = if backward {
                (0..cells.len()).rev().collect()
            } else {
                (0..cells.len()).collect()
            };
            let mut carried: Vec<Option<String>> = vec![None; cols.len()];
            let mut prev_key: Option<&Vec<String>> = None;
            for idx in order {
                // A fill never crosses a group boundary.
                if prev_key.is_some_and(|k| k != &keys[idx]) {
                    carried.fill(None);
                }
                prev_key = Some(&keys[idx]);
                let (abs, row_cells) = &cells[idx];
                for (ci, cell) in row_cells.iter().enumerate() {
                    if is_blank(cell) {
                        if let Some(v) = &carried[ci] {
                            changes.push((*abs, cols[ci], v.clone()));
                        }
                    } else {
                        carried[ci] = Some(cell.clone());
                    }
                }
            }
            if backward {
                changes.reverse(); // keep changes in row order for examples
            }
        }
        RepairOp::FillMean | RepairOp::FillMedian => {
            for (ci, &col) in cols.iter().enumerate() {
                let mut numbers = column_numbers(ci);
                if numbers.is_empty() {
                    continue;
                }
                let value = if matches!(spec.op, RepairOp::FillMean) {
                    numbers.iter().sum::<f64>() / numbers.len() as f64
                } else {
                    numbers.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
                    let mid = numbers.len() / 2;
                    if numbers.len() % 2 == 1 {
                        numbers[mid]
                    } else {
                        (numbers[mid - 1] + numbers[mid]) / 2.0
                    }
                };
                let rendered = format_number(value);
                fill_values.push((col, rendered.clone()));
                for (abs, row_cells) in &cells {
                    if is_blank(&row_cells[ci]) {
                        changes.push((*abs, col, rendered.clone()));
                    }
                }
            }
        }
        RepairOp::FillMode => {
            for (ci, &col) in cols.iter().enumerate() {
                let mut counts: HashMap<&str, usize> = HashMap::new();
                for (_, row_cells) in &cells {
                    let t = row_cells[ci].trim();
                    if !t.is_empty() {
                        *counts.entry(t).or_insert(0) += 1;
                    }
                }
                // Most frequent; ties -> lexicographically smallest.
                let mode = counts
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                    .map(|(v, _)| v.to_string());
                let Some(mode) = mode else { continue };
                fill_values.push((col, mode.clone()));
                for (abs, row_cells) in &cells {
                    if is_blank(&row_cells[ci]) {
                        changes.push((*abs, col, mode.clone()));
                    }
                }
            }
        }
        RepairOp::Interpolate { extrapolate } => {
            for (ci, &col) in cols.iter().enumerate() {
                // Known points: (position in scope, value). Invalid numerics
                // are neither targets nor anchors; they are counted only.
                let mut known: Vec<(usize, f64)> = Vec::new();
                for (pos, (_, row_cells)) in cells.iter().enumerate() {
                    let cell = row_cells[ci].trim();
                    if cell.is_empty() {
                        continue;
                    }
                    match analyze::as_number(cell) {
                        Some(n) => known.push((pos, n)),
                        None => invalid_numeric += 1,
                    }
                }
                if known.is_empty() {
                    continue;
                }
                for (pos, (abs, row_cells)) in cells.iter().enumerate() {
                    if !is_blank(&row_cells[ci]) {
                        continue;
                    }
                    let next = known.partition_point(|&(p, _)| p < pos);
                    let value = if next == 0 {
                        // Before the first known value.
                        if *extrapolate {
                            Some(known[0].1)
                        } else {
                            None
                        }
                    } else if next == known.len() {
                        // Past the last known value.
                        if *extrapolate {
                            Some(known[next - 1].1)
                        } else {
                            None
                        }
                    } else {
                        let (x0, y0) = known[next - 1];
                        let (x1, y1) = known[next];
                        let t = (pos - x0) as f64 / (x1 - x0) as f64;
                        Some(y0 + (y1 - y0) * t)
                    };
                    if let Some(v) = value {
                        changes.push((*abs, col, format_number(v)));
                    }
                }
            }
        }
        RepairOp::RemoveRows { threshold } => {
            for (abs, row_cells) in &cells {
                let blank = row_cells.iter().filter(|c| is_blank(c)).count();
                if blank as f64 / cols.len() as f64 >= *threshold {
                    remove_rows.push(*abs);
                }
            }
        }
        RepairOp::RemoveColumns { threshold } => {
            // With no scope rows nothing is "missing": remove nothing.
            if !cells.is_empty() {
                for (ci, &col) in cols.iter().enumerate() {
                    let blank = cells.iter().filter(|(_, r)| is_blank(&r[ci])).count();
                    if blank as f64 / cells.len() as f64 >= *threshold {
                        remove_columns.push(col);
                    }
                }
            }
        }
    }

    // Examples come straight from the change list (before values from the
    // snapshot; changes are ordered by scope row order).
    let by_pos: HashMap<usize, usize> = cells
        .iter()
        .enumerate()
        .map(|(pos, (abs, _))| (*abs, pos))
        .collect();
    let col_pos: HashMap<usize, usize> = cols.iter().enumerate().map(|(i, &c)| (c, i)).collect();
    let examples = changes
        .iter()
        .take(EXAMPLE_LIMIT)
        .map(|(r, c, after)| RepairExample {
            row: *r,
            col: *c,
            before: cells[by_pos[r]].1[col_pos[c]].clone(),
            after: after.clone(),
        })
        .collect();

    let preview = RepairPreview {
        revision: doc.revision(),
        cells_affected: changes.len(),
        rows_removed: remove_rows.len(),
        columns_removed: remove_columns.len(),
        fill_values,
        invalid_numeric,
        examples,
    };
    Ok(RepairComputed {
        changes,
        remove_rows,
        remove_columns,
        preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn spec(op: RepairOp, columns: Vec<usize>) -> RepairSpec {
        RepairSpec {
            op,
            columns,
            scope: ExportScope::All,
        }
    }

    #[test]
    fn null_tokens_normalize_to_blank_and_undo_restores_them() {
        let mut d = doc("a\nNA\nx\nn/a\n-\n");
        let computed = compute(
            &d,
            &spec(
                RepairOp::NormalizeNullTokens {
                    tokens: vec!["NA".into(), "n/a".into(), "-".into()],
                },
                vec![0],
            ),
        )
        .unwrap();
        assert_eq!(computed.preview.cells_affected, 3);
        d.set_cells(computed.changes).unwrap();
        assert_eq!(d.rows()[0][0], "");
        assert_eq!(d.rows()[1][0], "x");
        d.undo().unwrap();
        assert_eq!(d.rows()[0][0], "NA", "undo restores the exact token");
        assert_eq!(d.rows()[2][0], "n/a");
        assert!(!d.can_undo(), "one undo step");
    }

    #[test]
    fn grouped_forward_fill_never_crosses_a_group_boundary() {
        let d = doc("group,value\na,1\na,\nb,\nb,2\nb,\n");
        let computed = compute(
            &d,
            &spec(
                RepairOp::FillForward {
                    group_columns: vec![0],
                },
                vec![1],
            ),
        )
        .unwrap();
        // Row 1 fills from row 0 (same group); row 2 must NOT (group b has no
        // prior value); row 4 fills from row 3.
        assert_eq!(
            computed.changes,
            vec![(1, 1, "1".into()), (4, 1, "2".into())]
        );

        let backward = compute(
            &d,
            &spec(
                RepairOp::FillBackward {
                    group_columns: vec![0],
                },
                vec![1],
            ),
        )
        .unwrap();
        // Backward: row 2 fills from row 3 (group b); row 1 has no later "a".
        assert_eq!(backward.changes, vec![(2, 1, "2".into())]);
    }

    #[test]
    fn mean_median_and_mode_match_fixtures() {
        // Second column keeps blank cells as blank FIELDS (fully empty
        // lines are skipped by the parser).
        let d = doc("n,x\n1,1\n2,1\n,1\n4,1\nabc,1\n");
        let mean = compute(&d, &spec(RepairOp::FillMean, vec![0])).unwrap();
        // (1+2+4)/3 ≈ 2.3333…; "abc" ignored but counted.
        assert_eq!(mean.preview.invalid_numeric, 1);
        assert_eq!(mean.changes.len(), 1);
        assert!(mean.changes[0].2.starts_with("2.33333333333"));

        let median_odd = compute(&d, &spec(RepairOp::FillMedian, vec![0])).unwrap();
        assert_eq!(median_odd.changes[0].2, "2");

        let d_even = doc("n,x\n1,1\n2,1\n4,1\n10,1\n,1\n");
        let median_even = compute(&d_even, &spec(RepairOp::FillMedian, vec![0])).unwrap();
        assert_eq!(median_even.changes[0].2, "3"); // (2+4)/2

        // Mode tie (a×2 vs b×2) resolves to the lexicographically smallest.
        let d_mode = doc("s,x\na,1\nb,1\nb,1\na,1\n,1\n");
        let mode = compute(&d_mode, &spec(RepairOp::FillMode, vec![0])).unwrap();
        assert_eq!(mode.changes[0].2, "a", "tie -> lexicographically smallest");
        assert_eq!(mode.preview.fill_values, vec![(0, "a".to_string())]);
    }

    #[test]
    fn interpolation_never_extrapolates_unless_enabled() {
        let d = doc("n,x\n,1\n10,1\n,1\n,1\n40,1\n,1\n");
        let plain = compute(
            &d,
            &spec(RepairOp::Interpolate { extrapolate: false }, vec![0]),
        )
        .unwrap();
        // Rows 2,3 interpolate between 10 (row 1) and 40 (row 4); the
        // leading row 0 and trailing row 5 stay untouched.
        assert_eq!(
            plain.changes,
            vec![(2, 0, "20".into()), (3, 0, "30".into())]
        );

        let extended = compute(
            &d,
            &spec(RepairOp::Interpolate { extrapolate: true }, vec![0]),
        )
        .unwrap();
        assert_eq!(extended.changes.len(), 4);
        assert!(extended.changes.contains(&(0, 0, "10".into())));
        assert!(extended.changes.contains(&(5, 0, "40".into())));
    }

    #[test]
    fn scoped_repairs_never_touch_rows_outside_the_scope() {
        let d = doc("n,x\n1,1\n,1\n,1\n9,1\n");
        let scoped = RepairSpec {
            op: RepairOp::FillConstant { value: "X".into() },
            columns: vec![0],
            scope: ExportScope::SelectedRows { rows: vec![1] },
        };
        let computed = compute(&d, &scoped).unwrap();
        assert_eq!(computed.changes, vec![(1, 0, "X".into())]);
        // Row 2 is also blank but outside the scope: untouched.
    }

    #[test]
    fn removal_thresholds_pick_the_right_rows_and_columns() {
        let d = doc("a,b,c\n1,,\n1,2,3\n,,\n");
        let rows = compute(
            &d,
            &spec(RepairOp::RemoveRows { threshold: 0.6 }, vec![0, 1, 2]),
        )
        .unwrap();
        // Row 0: 2/3 blank ≥ 0.6 -> removed; row 2: 3/3 -> removed.
        assert_eq!(rows.remove_rows, vec![0, 2]);
        assert_eq!(rows.preview.rows_removed, 2);
        assert!(rows.changes.is_empty());

        let cols = compute(
            &d,
            &spec(RepairOp::RemoveColumns { threshold: 0.6 }, vec![0, 1, 2]),
        )
        .unwrap();
        // b and c are blank in 2/3 rows ≥ 0.6; a only 1/3.
        assert_eq!(cols.remove_columns, vec![1, 2]);
    }

    #[test]
    fn removal_applies_as_one_undo_step() {
        let mut d = doc("a,b\n1,\n2,2\n,\n");
        let computed = compute(
            &d,
            &spec(RepairOp::RemoveRows { threshold: 1.0 }, vec![0, 1]),
        )
        .unwrap();
        assert_eq!(computed.remove_rows, vec![2]);
        d.delete_rows(computed.remove_rows).unwrap();
        assert_eq!(d.n_rows(), 2);
        d.undo().unwrap();
        assert_eq!(d.n_rows(), 3);
        assert!(!d.can_undo());
    }

    #[test]
    fn previews_report_examples_without_mutating() {
        let d = doc("n,x\n,1\n5,1\n,1\n");
        let before = d.rows().to_vec();
        let computed = compute(
            &d,
            &spec(RepairOp::FillConstant { value: "0".into() }, vec![0]),
        )
        .unwrap();
        assert_eq!(computed.preview.cells_affected, 2);
        assert_eq!(computed.preview.examples.len(), 2);
        assert_eq!(computed.preview.examples[0].before, "");
        assert_eq!(computed.preview.examples[0].after, "0");
        assert_eq!(d.rows(), &before[..], "preview never mutates");
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let d = doc("a\n1\n");
        assert!(compute(&d, &spec(RepairOp::FillMean, vec![])).is_err());
        assert!(compute(&d, &spec(RepairOp::FillMean, vec![7])).is_err());
        assert!(compute(&d, &spec(RepairOp::RemoveRows { threshold: 1.5 }, vec![0])).is_err());
        assert!(compute(
            &d,
            &spec(RepairOp::NormalizeNullTokens { tokens: vec![] }, vec![0])
        )
        .is_err());
        assert!(compute(
            &d,
            &spec(
                RepairOp::FillForward {
                    group_columns: vec![9]
                },
                vec![0]
            )
        )
        .is_err());
    }
}
