//! Pivot, unpivot, and transpose (F23): reshape a document between wide and
//! long forms into a NEW document (the source is never modified). Pivot
//! column order is deterministic (sorted distinct header values), duplicate
//! pivot coordinates are detected and reported, the `none` aggregation
//! refuses multi-value cells, and transposition is size-guarded. Output
//! flows through [`crate::derived::DerivedDocumentBuilder`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::derived::{unique_column_name, DerivedDocumentBuilder};
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;

/// Output columns past this need explicit confirmation (raise the limit).
pub const DEFAULT_MAX_COLUMNS: usize = 1000;

/// Pivot cell aggregation (a closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PivotAgg {
    /// No aggregation: every output cell must hold at most ONE value.
    None,
    Count,
    CountNonBlank,
    Sum,
    Mean,
    Median,
    Min,
    Max,
    First,
    Last,
}

/// The three reshape operations.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ReshapeSpec {
    /// Wide → long: keep `id_columns`, turn each of `value_columns` into an
    /// (attribute, value) row.
    Unpivot {
        id_columns: Vec<usize>,
        value_columns: Vec<usize>,
        attribute_name: String,
        value_name: String,
        #[serde(default)]
        omit_blanks: bool,
        /// Add a 1-based source-row provenance column.
        #[serde(default)]
        add_source_row: bool,
    },
    /// Long → wide: `header_column`'s distinct values become columns.
    Pivot {
        row_keys: Vec<usize>,
        header_column: usize,
        value_column: usize,
        aggregation: PivotAgg,
        /// Refuse past this many output columns (UI confirms by raising it).
        #[serde(default = "default_max_columns")]
        max_columns: usize,
    },
    /// Swap rows and columns (whole document), size-guarded the same way.
    Transpose {
        #[serde(default = "default_max_columns")]
        max_columns: usize,
    },
}

fn default_max_columns() -> usize {
    DEFAULT_MAX_COLUMNS
}

fn validate(doc: &Document, spec: &ReshapeSpec) -> AppResult<()> {
    let n = doc.n_cols();
    match spec {
        ReshapeSpec::Unpivot {
            id_columns,
            value_columns,
            attribute_name,
            value_name,
            ..
        } => {
            if value_columns.is_empty() {
                return Err(AppError::invalid("pick at least one column to unpivot"));
            }
            if let Some(&bad) = id_columns
                .iter()
                .chain(value_columns.iter())
                .find(|&&c| c >= n)
            {
                return Err(AppError::invalid(format!("column {bad} is out of range")));
            }
            if id_columns.iter().any(|c| value_columns.contains(c)) {
                return Err(AppError::invalid(
                    "a column cannot be both an identifier and a value column",
                ));
            }
            if attribute_name.trim().is_empty() || value_name.trim().is_empty() {
                return Err(AppError::invalid(
                    "attribute and value output names are required",
                ));
            }
        }
        ReshapeSpec::Pivot {
            row_keys,
            header_column,
            value_column,
            max_columns,
            ..
        } => {
            if row_keys.is_empty() {
                return Err(AppError::invalid("pick at least one row-key column"));
            }
            let extra = [*header_column, *value_column];
            if let Some(&bad) = row_keys.iter().chain(extra.iter()).find(|&&c| c >= n) {
                return Err(AppError::invalid(format!("column {bad} is out of range")));
            }
            if row_keys.contains(header_column) || row_keys.contains(value_column) {
                return Err(AppError::invalid(
                    "the header/value columns cannot also be row keys",
                ));
            }
            if *max_columns == 0 {
                return Err(AppError::invalid("the column limit must be positive"));
            }
        }
        ReshapeSpec::Transpose { max_columns } => {
            if *max_columns == 0 {
                return Err(AppError::invalid("the column limit must be positive"));
            }
        }
    }
    Ok(())
}

/// A pivot cell accumulator (the reduced aggregate set for cells).
enum Cell {
    None(Option<String>),
    Count(u64),
    CountNonBlank(u64),
    Sum(f64, bool),
    Mean(f64, u64),
    Median(Vec<f64>),
    Min(Option<f64>),
    Max(Option<f64>),
    First(Option<String>),
    Last(Option<String>),
}

impl Cell {
    fn new(agg: PivotAgg) -> Cell {
        match agg {
            PivotAgg::None => Cell::None(None),
            PivotAgg::Count => Cell::Count(0),
            PivotAgg::CountNonBlank => Cell::CountNonBlank(0),
            PivotAgg::Sum => Cell::Sum(0.0, false),
            PivotAgg::Mean => Cell::Mean(0.0, 0),
            PivotAgg::Median => Cell::Median(Vec::new()),
            PivotAgg::Min => Cell::Min(None),
            PivotAgg::Max => Cell::Max(None),
            PivotAgg::First => Cell::First(None),
            PivotAgg::Last => Cell::Last(None),
        }
    }

    /// Returns `Err(())` on a duplicate coordinate under `none`.
    fn feed(&mut self, value: &str) -> Result<(), ()> {
        let trimmed = value.trim();
        let number = || analyze::as_number(trimmed);
        match self {
            Cell::None(slot) => {
                if slot.is_some() {
                    return Err(());
                }
                *slot = Some(value.to_string());
            }
            Cell::Count(n) => *n += 1,
            Cell::CountNonBlank(n) => {
                if !trimmed.is_empty() {
                    *n += 1;
                }
            }
            Cell::Sum(total, any) => {
                if let Some(v) = number() {
                    *total += v;
                    *any = true;
                }
            }
            Cell::Mean(total, count) => {
                if let Some(v) = number() {
                    *total += v;
                    *count += 1;
                }
            }
            Cell::Median(values) => {
                if let Some(v) = number() {
                    values.push(v);
                }
            }
            Cell::Min(m) => {
                if let Some(v) = number() {
                    *m = Some(m.map_or(v, |x: f64| x.min(v)));
                }
            }
            Cell::Max(m) => {
                if let Some(v) = number() {
                    *m = Some(m.map_or(v, |x: f64| x.max(v)));
                }
            }
            Cell::First(slot) => {
                if slot.is_none() {
                    *slot = Some(value.to_string());
                }
            }
            Cell::Last(slot) => *slot = Some(value.to_string()),
        }
        Ok(())
    }

    fn finish(self) -> String {
        match self {
            Cell::None(v) | Cell::First(v) | Cell::Last(v) => v.unwrap_or_default(),
            Cell::Count(n) | Cell::CountNonBlank(n) => n.to_string(),
            Cell::Sum(total, any) => {
                if any {
                    format_number(total)
                } else {
                    String::new()
                }
            }
            Cell::Mean(total, count) => {
                if count > 0 {
                    format_number(total / count as f64)
                } else {
                    String::new()
                }
            }
            Cell::Median(mut values) => {
                if values.is_empty() {
                    String::new()
                } else {
                    values.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
                    let mid = values.len() / 2;
                    format_number(if values.len() % 2 == 1 {
                        values[mid]
                    } else {
                        (values[mid - 1] + values[mid]) / 2.0
                    })
                }
            }
            Cell::Min(v) | Cell::Max(v) => v.map(format_number).unwrap_or_default(),
        }
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReshapePreview {
    pub output_columns: usize,
    pub projected_rows: usize,
    /// Leading output column names (bounded for the UI).
    pub column_sample: Vec<String>,
    /// Pivot only: (row key, header value) pairs holding >1 source value.
    pub duplicate_coordinates: usize,
    /// Unpivot only: blank cells skipped under `omit_blanks`.
    pub blanks_omitted: usize,
    /// Whether the run would refuse at the current column limit.
    pub over_column_limit: bool,
}

pub fn preview(doc: &Document, spec: &ReshapeSpec) -> AppResult<ReshapePreview> {
    validate(doc, spec)?;
    match spec {
        ReshapeSpec::Unpivot {
            id_columns,
            value_columns,
            attribute_name,
            value_name,
            omit_blanks,
            add_source_row,
        } => {
            let mut blanks = 0usize;
            if *omit_blanks {
                doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
                    blanks += value_columns
                        .iter()
                        .filter(|&&c| row.get(c).map(|v| v.trim().is_empty()).unwrap_or(true))
                        .count();
                    Ok(true)
                })?;
            }
            let headers =
                unpivot_headers(doc, id_columns, attribute_name, value_name, *add_source_row);
            Ok(ReshapePreview {
                output_columns: headers.len(),
                projected_rows: doc.n_rows() * value_columns.len() - blanks,
                column_sample: headers,
                duplicate_coordinates: 0,
                blanks_omitted: blanks,
                over_column_limit: false,
            })
        }
        ReshapeSpec::Pivot {
            row_keys,
            header_column,
            value_column: _,
            aggregation,
            max_columns,
        } => {
            let mut header_values: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            let mut keys: HashSet<Vec<String>> = HashSet::new();
            let mut coords: HashMap<(Vec<String>, String), u64> = HashMap::new();
            doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
                let header = row
                    .get(*header_column)
                    .map(|v| v.trim().to_string())
                    .unwrap_or_default();
                if seen.insert(header.clone()) {
                    header_values.push(header.clone());
                }
                let key: Vec<String> = row_key(row, row_keys);
                *coords.entry((key.clone(), header)).or_insert(0) += 1;
                keys.insert(key);
                Ok(true)
            })?;
            header_values.sort();
            let duplicate_coordinates = coords.values().filter(|&&n| n > 1).count();
            let output_columns = row_keys.len() + header_values.len();
            let mut column_sample: Vec<String> =
                row_keys.iter().map(|&c| header_name(doc, c)).collect();
            column_sample.extend(header_values.iter().take(30).cloned());
            Ok(ReshapePreview {
                output_columns,
                projected_rows: keys.len(),
                column_sample,
                duplicate_coordinates: if *aggregation == PivotAgg::None {
                    duplicate_coordinates
                } else {
                    0
                },
                blanks_omitted: 0,
                over_column_limit: output_columns > *max_columns,
            })
        }
        ReshapeSpec::Transpose { max_columns } => {
            let output_columns = doc.n_rows() + 1;
            Ok(ReshapePreview {
                output_columns,
                projected_rows: doc.n_cols(),
                column_sample: Vec::new(),
                duplicate_coordinates: 0,
                blanks_omitted: 0,
                over_column_limit: output_columns > *max_columns,
            })
        }
    }
}

fn header_name(doc: &Document, c: usize) -> String {
    doc.headers()
        .get(c)
        .cloned()
        .unwrap_or_else(|| format!("Column {}", c + 1))
}

fn row_key(row: &[String], columns: &[usize]) -> Vec<String> {
    columns
        .iter()
        .map(|&c| row.get(c).map(|v| v.trim().to_string()).unwrap_or_default())
        .collect()
}

fn unpivot_headers(
    doc: &Document,
    id_columns: &[usize],
    attribute_name: &str,
    value_name: &str,
    add_source_row: bool,
) -> Vec<String> {
    let mut headers: Vec<String> = id_columns.iter().map(|&c| header_name(doc, c)).collect();
    let attr = unique_column_name(&headers, attribute_name.trim());
    headers.push(attr);
    let value = unique_column_name(&headers, value_name.trim());
    headers.push(value);
    if add_source_row {
        let src = unique_column_name(&headers, "source_row");
        headers.push(src);
    }
    headers
}

/// Run the reshape into a new document (the shared "derive" job path).
pub fn run(
    doc: &Document,
    spec: &ReshapeSpec,
    doc_id: u64,
    cache_root: PathBuf,
    ctx: &JobCtx,
) -> AppResult<Document> {
    validate(doc, spec)?;
    match spec {
        ReshapeSpec::Unpivot {
            id_columns,
            value_columns,
            attribute_name,
            value_name,
            omit_blanks,
            add_source_row,
        } => {
            let headers =
                unpivot_headers(doc, id_columns, attribute_name, value_name, *add_source_row);
            let attr_names: Vec<String> =
                value_columns.iter().map(|&c| header_name(doc, c)).collect();
            let mut builder =
                DerivedDocumentBuilder::new(headers, cache_root, crate::derived::SPILL_BUDGET);
            ctx.set_total(doc.n_rows() as u64);
            doc.visit_rows(0..doc.n_rows(), &mut |r, row| {
                ctx.advance(1)?;
                for (vi, &vc) in value_columns.iter().enumerate() {
                    let value = row.get(vc).cloned().unwrap_or_default();
                    if *omit_blanks && value.trim().is_empty() {
                        continue;
                    }
                    let mut out: Vec<String> = id_columns
                        .iter()
                        .map(|&c| row.get(c).cloned().unwrap_or_default())
                        .collect();
                    out.push(attr_names[vi].clone());
                    out.push(value);
                    if *add_source_row {
                        out.push((r + 1).to_string());
                    }
                    builder.push_row(out)?;
                }
                Ok(true)
            })?;
            ctx.set_message("building the output document");
            builder.finish(doc_id, &mut |_| ctx.check())
        }
        ReshapeSpec::Pivot {
            row_keys,
            header_column,
            value_column,
            aggregation,
            max_columns,
        } => {
            // Pass 1: distinct header values (sorted -> deterministic).
            let mut header_values: Vec<String> = Vec::new();
            let mut seen: HashMap<String, usize> = HashMap::new();
            ctx.set_total(doc.n_rows() as u64 * 2);
            doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
                ctx.advance(1)?;
                let header = row
                    .get(*header_column)
                    .map(|v| v.trim().to_string())
                    .unwrap_or_default();
                if !seen.contains_key(&header) {
                    seen.insert(header.clone(), 0);
                    header_values.push(header);
                }
                Ok(true)
            })?;
            header_values.sort();
            let output_columns = row_keys.len() + header_values.len();
            if output_columns > *max_columns {
                return Err(AppError::invalid(format!(
                    "the pivot would produce {output_columns} columns, over the \
                     limit of {max_columns} — confirm to run anyway"
                )));
            }
            for (i, h) in header_values.iter().enumerate() {
                seen.insert(h.clone(), i);
            }

            // Pass 2: fill (row key -> cells) in first-seen key order.
            let mut key_index: HashMap<Vec<String>, usize> = HashMap::new();
            let mut groups: Vec<(Vec<String>, Vec<Cell>)> = Vec::new();
            let agg = *aggregation;
            let mut duplicate: Option<(Vec<String>, String)> = None;
            doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
                ctx.advance(1)?;
                let key = row_key(row, row_keys);
                let idx = match key_index.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = groups.len();
                        groups.push((
                            key.clone(),
                            header_values.iter().map(|_| Cell::new(agg)).collect(),
                        ));
                        key_index.insert(key.clone(), i);
                        i
                    }
                };
                let header = row
                    .get(*header_column)
                    .map(|v| v.trim().to_string())
                    .unwrap_or_default();
                let col = seen[&header];
                let value = row.get(*value_column).cloned().unwrap_or_default();
                if groups[idx].1[col].feed(&value).is_err() && duplicate.is_none() {
                    duplicate = Some((key, header));
                }
                Ok(true)
            })?;
            if let Some((key, header)) = duplicate {
                return Err(AppError::invalid(format!(
                    "more than one value lands in the cell for key ({}) and \
                     column \"{header}\" — pick an aggregation other than \"none\"",
                    key.join(" · ")
                )));
            }

            let mut headers: Vec<String> = row_keys.iter().map(|&c| header_name(doc, c)).collect();
            for h in &header_values {
                let name = if h.is_empty() { "(blank)" } else { h.as_str() };
                let unique = unique_column_name(&headers, name);
                headers.push(unique);
            }
            let mut builder =
                DerivedDocumentBuilder::new(headers, cache_root, crate::derived::SPILL_BUDGET);
            for (key, cells) in groups {
                ctx.check()?;
                let mut out = key;
                out.extend(cells.into_iter().map(Cell::finish));
                builder.push_row(out)?;
            }
            ctx.set_message("building the output document");
            builder.finish(doc_id, &mut |_| ctx.check())
        }
        ReshapeSpec::Transpose { max_columns } => {
            let output_columns = doc.n_rows() + 1;
            if output_columns > *max_columns {
                return Err(AppError::invalid(format!(
                    "transposing would produce {output_columns} columns, over \
                     the limit of {max_columns} — confirm to run anyway"
                )));
            }
            // Materialise the grid column-major. Bounded by the column guard.
            let n_rows = doc.n_rows();
            let n_cols = doc.n_cols();
            ctx.set_total(n_rows as u64);
            let mut grid: Vec<Vec<String>> = vec![Vec::with_capacity(n_rows); n_cols];
            doc.visit_rows(0..n_rows, &mut |_, row| {
                ctx.advance(1)?;
                for (c, column) in grid.iter_mut().enumerate() {
                    column.push(row.get(c).cloned().unwrap_or_default());
                }
                Ok(true)
            })?;

            let mut headers: Vec<String> = Vec::with_capacity(output_columns);
            headers.push("Column".to_string());
            for r in 0..n_rows {
                headers.push(format!("Row {}", r + 1));
            }
            let mut builder =
                DerivedDocumentBuilder::new(headers, cache_root, crate::derived::SPILL_BUDGET);
            for (c, column) in grid.into_iter().enumerate() {
                ctx.check()?;
                let mut out = Vec::with_capacity(output_columns);
                out.push(header_name(doc, c));
                out.extend(column);
                builder.push_row(out)?;
            }
            ctx.set_message("building the output document");
            builder.finish(doc_id, &mut |_| ctx.check())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn run_reshape(doc: &Document, spec: &ReshapeSpec) -> AppResult<Document> {
        let registry = JobRegistry::default();
        let ctx = registry.begin("derive", None, |_| {});
        let dir = tempfile::tempdir().unwrap();
        run(doc, spec, 9, dir.path().to_path_buf(), &ctx)
    }

    fn unpivot_spec() -> ReshapeSpec {
        ReshapeSpec::Unpivot {
            id_columns: vec![0],
            value_columns: vec![1, 2],
            attribute_name: "metric".into(),
            value_name: "value".into(),
            omit_blanks: false,
            add_source_row: false,
        }
    }

    fn pivot_spec(agg: PivotAgg) -> ReshapeSpec {
        ReshapeSpec::Pivot {
            row_keys: vec![0],
            header_column: 1,
            value_column: 2,
            aggregation: agg,
            max_columns: DEFAULT_MAX_COLUMNS,
        }
    }

    #[test]
    fn unpivot_produces_long_form_with_provenance() {
        let d = doc("name,jan,feb\nann,1,2\nbob,3,\n");
        let out = run_reshape(&d, &unpivot_spec()).unwrap();
        assert_eq!(out.headers(), &["name", "metric", "value"]);
        assert_eq!(out.n_rows(), 4);
        assert_eq!(out.rows()[0], vec!["ann", "jan", "1"]);
        assert_eq!(out.rows()[1], vec!["ann", "feb", "2"]);
        assert_eq!(out.rows()[3], vec!["bob", "feb", ""]);

        let with_omit = ReshapeSpec::Unpivot {
            id_columns: vec![0],
            value_columns: vec![1, 2],
            attribute_name: "metric".into(),
            value_name: "value".into(),
            omit_blanks: true,
            add_source_row: true,
        };
        let out = run_reshape(&d, &with_omit).unwrap();
        assert_eq!(out.headers(), &["name", "metric", "value", "source_row"]);
        assert_eq!(out.n_rows(), 3, "bob's blank feb is omitted");
        assert_eq!(out.rows()[2], vec!["bob", "jan", "3", "2"]);
    }

    #[test]
    fn pivot_and_unpivot_round_trip_when_lossless() {
        let d = doc("name,jan,feb\nann,1,2\nbob,3,4\n");
        let long = run_reshape(&d, &unpivot_spec()).unwrap();
        let wide = run_reshape(&long, &pivot_spec(PivotAgg::None)).unwrap();
        // Sorted distinct headers: feb before jan (deterministic order).
        assert_eq!(wide.headers(), &["name", "feb", "jan"]);
        assert_eq!(wide.n_rows(), 2);
        assert_eq!(wide.rows()[0], vec!["ann", "2", "1"]);
        assert_eq!(wide.rows()[1], vec!["bob", "4", "3"]);
    }

    #[test]
    fn pivot_none_rejects_duplicate_coordinates_with_a_clear_error() {
        let d = doc("k,h,v\na,x,1\na,x,2\n");
        let err = match run_reshape(&d, &pivot_spec(PivotAgg::None)) {
            Err(e) => e,
            Ok(_) => panic!("duplicate coordinate must fail under none"),
        };
        assert!(err.to_string().contains("more than one value"));
        let p = preview(&d, &pivot_spec(PivotAgg::None)).unwrap();
        assert_eq!(p.duplicate_coordinates, 1);

        // With an aggregation, the same input works.
        let out = run_reshape(&d, &pivot_spec(PivotAgg::Sum)).unwrap();
        assert_eq!(out.rows()[0], vec!["a", "3"]);
    }

    #[test]
    fn pivot_aggregations_match_fixtures() {
        let d = doc("k,h,v\na,x,1\na,x,3\na,y,10\nb,x,5\n");
        let sum = run_reshape(&d, &pivot_spec(PivotAgg::Sum)).unwrap();
        assert_eq!(sum.headers(), &["k", "x", "y"]);
        assert_eq!(sum.rows()[0], vec!["a", "4", "10"]);
        assert_eq!(sum.rows()[1], vec!["b", "5", ""]);

        let count = run_reshape(&d, &pivot_spec(PivotAgg::Count)).unwrap();
        assert_eq!(count.rows()[0], vec!["a", "2", "1"]);
        assert_eq!(count.rows()[1], vec!["b", "1", "0"]);

        let mean = run_reshape(&d, &pivot_spec(PivotAgg::Mean)).unwrap();
        assert_eq!(mean.rows()[0], vec!["a", "2", "10"]);

        let median = run_reshape(&d, &pivot_spec(PivotAgg::Median)).unwrap();
        assert_eq!(median.rows()[0], vec!["a", "2", "10"]);
    }

    #[test]
    fn pivot_column_limit_requires_confirmation() {
        let d = doc("k,h,v\na,x,1\na,y,2\na,z,3\n");
        let spec = ReshapeSpec::Pivot {
            row_keys: vec![0],
            header_column: 1,
            value_column: 2,
            aggregation: PivotAgg::First,
            max_columns: 3, // 1 key + 3 headers = 4 > 3
        };
        let err = match run_reshape(&d, &spec) {
            Err(e) => e,
            Ok(_) => panic!("over-limit pivot must fail"),
        };
        assert!(err.to_string().contains("limit"));
        let p = preview(&d, &spec).unwrap();
        assert!(p.over_column_limit);
    }

    #[test]
    fn transpose_swaps_dimensions_and_guards_size() {
        let d = doc("a,b,c\n1,2,3\n4,5,6\n");
        let out = run_reshape(
            &d,
            &ReshapeSpec::Transpose {
                max_columns: DEFAULT_MAX_COLUMNS,
            },
        )
        .unwrap();
        assert_eq!(out.headers(), &["Column", "Row 1", "Row 2"]);
        assert_eq!(out.n_rows(), 3);
        assert_eq!(out.rows()[0], vec!["a", "1", "4"]);
        assert_eq!(out.rows()[2], vec!["c", "3", "6"]);

        assert!(run_reshape(&d, &ReshapeSpec::Transpose { max_columns: 2 }).is_err());
    }

    #[test]
    fn blank_pivot_headers_get_a_readable_name() {
        let d = doc("k,h,v\na,,1\na,x,2\n");
        let out = run_reshape(&d, &pivot_spec(PivotAgg::First)).unwrap();
        assert_eq!(out.headers(), &["k", "(blank)", "x"]);
    }

    #[test]
    fn source_is_never_modified() {
        let d = doc("name,jan,feb\nann,1,2\n");
        let before = d.revision();
        let _ = run_reshape(&d, &unpivot_spec()).unwrap();
        assert_eq!(d.revision(), before);
        assert_eq!(d.n_cols(), 3);
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let d = doc("a,b\n1,2\n");
        let cases = [
            ReshapeSpec::Unpivot {
                id_columns: vec![0],
                value_columns: vec![],
                attribute_name: "m".into(),
                value_name: "v".into(),
                omit_blanks: false,
                add_source_row: false,
            },
            ReshapeSpec::Unpivot {
                id_columns: vec![0],
                value_columns: vec![0],
                attribute_name: "m".into(),
                value_name: "v".into(),
                omit_blanks: false,
                add_source_row: false,
            },
            ReshapeSpec::Unpivot {
                id_columns: vec![],
                value_columns: vec![1],
                attribute_name: " ".into(),
                value_name: "v".into(),
                omit_blanks: false,
                add_source_row: false,
            },
            ReshapeSpec::Pivot {
                row_keys: vec![],
                header_column: 0,
                value_column: 1,
                aggregation: PivotAgg::First,
                max_columns: 10,
            },
            ReshapeSpec::Pivot {
                row_keys: vec![0],
                header_column: 0,
                value_column: 1,
                aggregation: PivotAgg::First,
                max_columns: 10,
            },
        ];
        for spec in cases {
            assert!(preview(&d, &spec).is_err(), "{spec:?}");
        }
    }
}
