//! Group-by aggregations (F22): summarise a document into a NEW grouped
//! document without formulas. A closed aggregate set with explicit
//! policies: invalid numeric cells are ignored by the math but counted,
//! blank group keys are kept (as a blank group) or excluded per the spec,
//! concatenation is separator-configurable and length-capped, and group
//! counts are bounded with a clear error. The source is never modified.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::derived::{unique_column_name, DerivedDocumentBuilder};
use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;
use crate::job::JobCtx;

/// Hard cap on the number of groups (bounded memory, clear error).
pub const MAX_GROUPS: usize = 1_000_000;
/// Hard cap on distinct-value tracking across all groups and aggregates.
pub const MAX_DISTINCT_TRACKED: usize = 5_000_000;
/// Sample rows included in a preview.
const PREVIEW_SAMPLE: usize = 10;

/// The closed aggregate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Aggregate {
    /// Rows in the group (needs no column).
    Count,
    CountNonBlank,
    CountDistinct,
    Sum,
    Mean,
    Min,
    Max,
    Median,
    First,
    Last,
    Concat,
    ConcatDistinct,
}

impl Aggregate {
    fn needs_column(self) -> bool {
        !matches!(self, Aggregate::Count)
    }

    fn is_numeric(self) -> bool {
        matches!(
            self,
            Aggregate::Sum | Aggregate::Mean | Aggregate::Min | Aggregate::Max | Aggregate::Median
        )
    }

    fn label(self) -> &'static str {
        match self {
            Aggregate::Count => "count",
            Aggregate::CountNonBlank => "count_nonblank",
            Aggregate::CountDistinct => "count_distinct",
            Aggregate::Sum => "sum",
            Aggregate::Mean => "mean",
            Aggregate::Min => "min",
            Aggregate::Max => "max",
            Aggregate::Median => "median",
            Aggregate::First => "first",
            Aggregate::Last => "last",
            Aggregate::Concat => "concat",
            Aggregate::ConcatDistinct => "concat_distinct",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateSpec {
    pub aggregate: Aggregate,
    /// The aggregated column (ignored for `count`).
    #[serde(default)]
    pub column: Option<usize>,
    /// Custom output column name (defaults to "agg(column)").
    #[serde(default)]
    pub output_name: Option<String>,
}

/// What happens to rows whose group key contains a blank component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlankKeys {
    /// Group them under the blank value like any other key.
    Keep,
    /// Drop those rows from the aggregation (their count is reported).
    Exclude,
}

/// Output group ordering (deterministic; ties resolve by key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupOrdering {
    /// Sort by the group key values.
    ByKey,
    /// Largest groups first.
    ByCountDesc,
    /// The order groups first appeared in the data.
    FirstSeen,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupBySpec {
    /// One or more grouping columns.
    pub group_columns: Vec<usize>,
    pub aggregates: Vec<AggregateSpec>,
    /// All rows or the visible (filtered) rows.
    pub scope: ExportScope,
    /// Case-insensitive, trimmed grouping (display keeps the first-seen raw
    /// value).
    #[serde(default)]
    pub normalized_grouping: bool,
    pub blank_keys: BlankKeys,
    pub ordering: GroupOrdering,
    #[serde(default = "default_separator")]
    pub concat_separator: String,
    /// Concatenated outputs are truncated (with an ellipsis) past this.
    #[serde(default = "default_concat_cap")]
    pub concat_max_len: usize,
}

fn default_separator() -> String {
    ", ".to_string()
}

fn default_concat_cap() -> usize {
    2000
}

fn validate(doc: &Document, spec: &GroupBySpec) -> AppResult<()> {
    if spec.group_columns.is_empty() {
        return Err(AppError::invalid("pick at least one grouping column"));
    }
    if spec.aggregates.is_empty() {
        return Err(AppError::invalid("add at least one aggregate"));
    }
    if let Some(&bad) = spec.group_columns.iter().find(|&&c| c >= doc.n_cols()) {
        return Err(AppError::invalid(format!(
            "grouping column {bad} is out of range"
        )));
    }
    for agg in &spec.aggregates {
        match agg.column {
            Some(c) if c >= doc.n_cols() => {
                return Err(AppError::invalid(format!(
                    "aggregate column {c} is out of range"
                )));
            }
            None if agg.aggregate.needs_column() => {
                return Err(AppError::invalid(format!(
                    "{} needs a column",
                    agg.aggregate.label()
                )));
            }
            _ => {}
        }
    }
    if spec.concat_max_len == 0 {
        return Err(AppError::invalid("concat length cap must be positive"));
    }
    Ok(())
}

/// Per-group accumulator for one aggregate.
enum Acc {
    Count(u64),
    CountNonBlank(u64),
    CountDistinct(HashSet<String>),
    Sum(f64, bool),
    Mean(f64, u64),
    Min(Option<f64>),
    Max(Option<f64>),
    Median(Vec<f64>),
    First(Option<String>),
    Last(Option<String>),
    Concat(String, bool),
    ConcatDistinct(String, bool, HashSet<String>),
}

impl Acc {
    fn new(aggregate: Aggregate) -> Acc {
        match aggregate {
            Aggregate::Count => Acc::Count(0),
            Aggregate::CountNonBlank => Acc::CountNonBlank(0),
            Aggregate::CountDistinct => Acc::CountDistinct(HashSet::new()),
            Aggregate::Sum => Acc::Sum(0.0, false),
            Aggregate::Mean => Acc::Mean(0.0, 0),
            Aggregate::Min => Acc::Min(None),
            Aggregate::Max => Acc::Max(None),
            Aggregate::Median => Acc::Median(Vec::new()),
            Aggregate::First => Acc::First(None),
            Aggregate::Last => Acc::Last(None),
            Aggregate::Concat => Acc::Concat(String::new(), false),
            Aggregate::ConcatDistinct => Acc::ConcatDistinct(String::new(), false, HashSet::new()),
        }
    }

    /// Feed one cell. Returns how many NEW distinct entries were tracked
    /// (for the global bound) and whether a non-blank cell failed to parse
    /// as a number for a numeric aggregate.
    fn feed(&mut self, value: &str, spec: &GroupBySpec) -> (usize, bool) {
        let trimmed = value.trim();
        let blank = trimmed.is_empty();
        let mut tracked = 0usize;
        let mut invalid = false;
        let mut number = || -> Option<f64> {
            if blank {
                return None;
            }
            match analyze::as_number(trimmed) {
                Some(n) => Some(n),
                None => {
                    invalid = true;
                    None
                }
            }
        };
        match self {
            Acc::Count(n) => *n += 1,
            Acc::CountNonBlank(n) => {
                if !blank {
                    *n += 1;
                }
            }
            Acc::CountDistinct(set) => {
                if !blank && set.insert(trimmed.to_string()) {
                    tracked = 1;
                }
            }
            Acc::Sum(total, any) => {
                if let Some(n) = number() {
                    *total += n;
                    *any = true;
                }
            }
            Acc::Mean(total, count) => {
                if let Some(n) = number() {
                    *total += n;
                    *count += 1;
                }
            }
            Acc::Min(min) => {
                if let Some(n) = number() {
                    *min = Some(min.map_or(n, |m: f64| m.min(n)));
                }
            }
            Acc::Max(max) => {
                if let Some(n) = number() {
                    *max = Some(max.map_or(n, |m: f64| m.max(n)));
                }
            }
            Acc::Median(values) => {
                if let Some(n) = number() {
                    values.push(n);
                }
            }
            Acc::First(first) => {
                if first.is_none() {
                    *first = Some(value.to_string());
                }
            }
            Acc::Last(last) => *last = Some(value.to_string()),
            Acc::Concat(out, truncated) => {
                if !blank && !*truncated {
                    append_concat(out, truncated, trimmed, spec);
                }
            }
            Acc::ConcatDistinct(out, truncated, seen) => {
                if !blank && seen.insert(trimmed.to_string()) {
                    tracked = 1;
                    if !*truncated {
                        append_concat(out, truncated, trimmed, spec);
                    }
                }
            }
        }
        (tracked, invalid)
    }

    fn finish(self) -> String {
        match self {
            Acc::Count(n) | Acc::CountNonBlank(n) => n.to_string(),
            Acc::CountDistinct(set) => set.len().to_string(),
            Acc::Sum(total, any) => {
                if any {
                    format_number(total)
                } else {
                    String::new()
                }
            }
            Acc::Mean(total, count) => {
                if count > 0 {
                    format_number(total / count as f64)
                } else {
                    String::new()
                }
            }
            Acc::Min(v) | Acc::Max(v) => v.map(format_number).unwrap_or_default(),
            Acc::Median(mut values) => {
                if values.is_empty() {
                    String::new()
                } else {
                    values.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
                    let mid = values.len() / 2;
                    let median = if values.len() % 2 == 1 {
                        values[mid]
                    } else {
                        (values[mid - 1] + values[mid]) / 2.0
                    };
                    format_number(median)
                }
            }
            Acc::First(v) | Acc::Last(v) => v.unwrap_or_default(),
            Acc::Concat(out, _) => out,
            Acc::ConcatDistinct(out, _, _) => out,
        }
    }
}

fn append_concat(out: &mut String, truncated: &mut bool, value: &str, spec: &GroupBySpec) {
    if !out.is_empty() {
        out.push_str(&spec.concat_separator);
    }
    out.push_str(value);
    if out.len() > spec.concat_max_len {
        // Cut on a char boundary so multi-byte values can never panic.
        let mut cut = spec.concat_max_len;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
        *truncated = true;
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// One group's state: display key + accumulators.
struct Group {
    display: Vec<String>,
    rows: u64,
    accs: Vec<Acc>,
}

struct Grouped {
    /// Insertion-ordered groups.
    groups: Vec<Group>,
    invalid_numeric: usize,
    blank_key_rows: usize,
    scanned_rows: usize,
}

fn group(doc: &Document, spec: &GroupBySpec, ctx: Option<&JobCtx>) -> AppResult<Grouped> {
    validate(doc, spec)?;
    let rows = resolve_scope(doc, &spec.scope)?.rows;
    if let Some(ctx) = ctx {
        ctx.set_total(rows.len() as u64);
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<Vec<String>, usize> = HashMap::new();
    let mut invalid_numeric = 0usize;
    let mut blank_key_rows = 0usize;
    let mut distinct_tracked = 0usize;
    let mut processed = 0u64;

    doc.visit_rows_at(&rows, &mut |_, row| {
        processed += 1;
        if let Some(ctx) = ctx {
            if processed.is_multiple_of(4096) {
                ctx.advance(4096)?;
            }
        }
        // Keys keep the EXACT cell values: trimming is part of normalized
        // grouping, so a raw group-by must not silently merge "East" with
        // "East " (blank detection still trims — a whitespace-only key is
        // blank for the policy either way).
        let raw: Vec<String> = spec
            .group_columns
            .iter()
            .map(|&c| row.get(c).cloned().unwrap_or_default())
            .collect();
        if raw.iter().any(|v| v.trim().is_empty()) && spec.blank_keys == BlankKeys::Exclude {
            blank_key_rows += 1;
            return Ok(true);
        }
        let key: Vec<String> = if spec.normalized_grouping {
            raw.iter().map(|v| v.trim().to_lowercase()).collect()
        } else {
            raw.clone()
        };
        let group_idx = match index.get(&key) {
            Some(&i) => i,
            None => {
                if groups.len() >= MAX_GROUPS {
                    return Err(AppError::invalid(format!(
                        "more than {MAX_GROUPS} groups — group by fewer or coarser columns"
                    )));
                }
                let idx = groups.len();
                groups.push(Group {
                    display: raw,
                    rows: 0,
                    accs: spec
                        .aggregates
                        .iter()
                        .map(|a| Acc::new(a.aggregate))
                        .collect(),
                });
                index.insert(key, idx);
                idx
            }
        };
        let group = &mut groups[group_idx];
        group.rows += 1;
        for (acc, agg) in group.accs.iter_mut().zip(&spec.aggregates) {
            let cell = agg
                .column
                .and_then(|c| row.get(c))
                .map(String::as_str)
                .unwrap_or("");
            let (tracked, invalid) = acc.feed(cell, spec);
            distinct_tracked += tracked;
            if invalid && agg.aggregate.is_numeric() {
                invalid_numeric += 1;
            }
            if distinct_tracked > MAX_DISTINCT_TRACKED {
                return Err(AppError::invalid(
                    "too many distinct values to track — remove the distinct \
                     aggregates or group by coarser columns",
                ));
            }
        }
        Ok(true)
    })?;
    if let Some(ctx) = ctx {
        ctx.flush_progress();
    }

    // Deterministic ordering; ties resolve by the display key.
    match spec.ordering {
        GroupOrdering::FirstSeen => {}
        GroupOrdering::ByKey => groups.sort_by(|a, b| a.display.cmp(&b.display)),
        GroupOrdering::ByCountDesc => {
            groups.sort_by(|a, b| b.rows.cmp(&a.rows).then_with(|| a.display.cmp(&b.display)));
        }
    }

    Ok(Grouped {
        groups,
        invalid_numeric,
        blank_key_rows,
        scanned_rows: rows.len(),
    })
}

/// The output schema: grouping columns, then one column per aggregate.
fn output_headers(doc: &Document, spec: &GroupBySpec) -> Vec<String> {
    let mut headers: Vec<String> = spec
        .group_columns
        .iter()
        .map(|&c| {
            doc.headers()
                .get(c)
                .cloned()
                .unwrap_or_else(|| format!("Column {}", c + 1))
        })
        .collect();
    for agg in &spec.aggregates {
        let base = match &agg.output_name {
            Some(name) if !name.trim().is_empty() => name.trim().to_string(),
            _ => match agg.column {
                Some(c) => format!(
                    "{}({})",
                    agg.aggregate.label(),
                    doc.headers()
                        .get(c)
                        .cloned()
                        .unwrap_or_else(|| format!("Column {}", c + 1))
                ),
                None => format!("{}(*)", agg.aggregate.label()),
            },
        };
        let name = unique_column_name(&headers, &base);
        headers.push(name);
    }
    headers
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupByPreview {
    pub output_columns: Vec<String>,
    pub group_count: usize,
    pub scanned_rows: usize,
    /// Non-blank cells numeric aggregates had to ignore.
    pub invalid_numeric: usize,
    /// Rows excluded for blank group keys (under the Exclude policy).
    pub blank_key_rows: usize,
    /// First output rows.
    pub sample: Vec<Vec<String>>,
}

pub fn preview(doc: &Document, spec: &GroupBySpec) -> AppResult<GroupByPreview> {
    let grouped = group(doc, spec, None)?;
    let headers = output_headers(doc, spec);
    let group_count = grouped.groups.len();
    let sample = grouped
        .groups
        .into_iter()
        .take(PREVIEW_SAMPLE)
        .map(|g| {
            let mut row = g.display;
            row.extend(g.accs.into_iter().map(Acc::finish));
            row
        })
        .collect();
    Ok(GroupByPreview {
        output_columns: headers,
        group_count,
        scanned_rows: grouped.scanned_rows,
        invalid_numeric: grouped.invalid_numeric,
        blank_key_rows: grouped.blank_key_rows,
        sample,
    })
}

/// Run the group-by into a new document (the shared "derive" job path).
pub fn run(
    doc: &Document,
    spec: &GroupBySpec,
    doc_id: u64,
    cache_root: PathBuf,
    ctx: &JobCtx,
) -> AppResult<Document> {
    let grouped = group(doc, spec, Some(ctx))?;
    let headers = output_headers(doc, spec);
    let mut builder =
        DerivedDocumentBuilder::new(headers, cache_root, crate::derived::SPILL_BUDGET);
    for g in grouped.groups {
        ctx.check()?;
        let mut row = g.display;
        row.extend(g.accs.into_iter().map(Acc::finish));
        builder.push_row(row)?;
    }
    ctx.set_message("building the output document");
    builder.finish(doc_id, &mut |_| ctx.check())
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

    fn agg(aggregate: Aggregate, column: Option<usize>) -> AggregateSpec {
        AggregateSpec {
            aggregate,
            column,
            output_name: None,
        }
    }

    fn spec(group_columns: Vec<usize>, aggregates: Vec<AggregateSpec>) -> GroupBySpec {
        GroupBySpec {
            group_columns,
            aggregates,
            scope: ExportScope::All,
            normalized_grouping: false,
            blank_keys: BlankKeys::Keep,
            ordering: GroupOrdering::ByKey,
            concat_separator: ", ".into(),
            concat_max_len: 2000,
        }
    }

    fn run_group(doc: &Document, spec: &GroupBySpec) -> Document {
        let registry = JobRegistry::default();
        let ctx = registry.begin("derive", None, |_| {});
        let dir = tempfile::tempdir().unwrap();
        run(doc, spec, 9, dir.path().to_path_buf(), &ctx).unwrap()
    }

    fn fixture() -> Document {
        doc("dept,amount,who\nsales,10,ann\nsales,20,bob\nops,5,cid\nsales,,dee\nops,abc,eve\n")
    }

    #[test]
    fn aggregates_match_exact_fixtures() {
        let d = fixture();
        let s = spec(
            vec![0],
            vec![
                agg(Aggregate::Count, None),
                agg(Aggregate::CountNonBlank, Some(1)),
                agg(Aggregate::Sum, Some(1)),
                agg(Aggregate::Mean, Some(1)),
                agg(Aggregate::Min, Some(1)),
                agg(Aggregate::Max, Some(1)),
                agg(Aggregate::First, Some(2)),
                agg(Aggregate::Last, Some(2)),
            ],
        );
        let out = run_group(&d, &s);
        assert_eq!(
            out.headers(),
            &[
                "dept",
                "count(*)",
                "count_nonblank(amount)",
                "sum(amount)",
                "mean(amount)",
                "min(amount)",
                "max(amount)",
                "first(who)",
                "last(who)"
            ]
        );
        // ByKey ordering: ops before sales.
        assert_eq!(
            out.rows()[0],
            vec!["ops", "2", "2", "5", "5", "5", "5", "cid", "eve"]
        );
        // sales: amounts 10, 20, blank -> sum 30, mean 15.
        assert_eq!(
            out.rows()[1],
            vec!["sales", "3", "2", "30", "15", "10", "20", "ann", "dee"]
        );

        let preview = preview(&d, &s).unwrap();
        assert_eq!(preview.group_count, 2);
        assert_eq!(
            preview.invalid_numeric, 4,
            "\"abc\" ignored by 4 numeric aggs"
        );
    }

    #[test]
    fn median_handles_odd_and_even_group_sizes() {
        let d = doc("g,n\na,1\na,3\na,2\nb,10\nb,20\n");
        let s = spec(vec![0], vec![agg(Aggregate::Median, Some(1))]);
        let out = run_group(&d, &s);
        assert_eq!(out.rows()[0], vec!["a", "2"]); // odd: middle of 1,2,3
        assert_eq!(out.rows()[1], vec!["b", "15"]); // even: (10+20)/2
    }

    #[test]
    fn raw_grouping_preserves_key_whitespace() {
        // "East" and "East " are DIFFERENT raw keys (trimming is part of
        // normalized grouping); normalized grouping merges them.
        let d = doc("g,n\nEast,1\nEast ,2\n");
        let s = spec(vec![0], vec![agg(Aggregate::Count, None)]);
        let out = run_group(&d, &s);
        assert_eq!(out.n_rows(), 2, "raw keys keep exact whitespace");
        assert_eq!(out.rows()[0][0], "East");
        assert_eq!(out.rows()[1][0], "East ", "display keeps the raw spaces");

        let mut s2 = spec(vec![0], vec![agg(Aggregate::Count, None)]);
        s2.normalized_grouping = true;
        let out2 = run_group(&d, &s2);
        assert_eq!(out2.n_rows(), 1, "normalized grouping trims");
        assert_eq!(out2.rows()[0][1], "2");
    }

    #[test]
    fn distinct_counts_follow_normalization() {
        let d = doc("g,v\na,X\na,x\na, x \nb,y\n");
        // Raw grouping: values distinct-counted on their trimmed text.
        let s = spec(vec![0], vec![agg(Aggregate::CountDistinct, Some(1))]);
        let out = run_group(&d, &s);
        assert_eq!(out.rows()[0], vec!["a", "2"]); // "X" and "x"

        // Normalized grouping groups keys case-insensitively.
        let d2 = doc("g,v\nSales,1\nsales,2\nSALES,3\n");
        let mut s2 = spec(vec![0], vec![agg(Aggregate::Count, None)]);
        s2.normalized_grouping = true;
        let out2 = run_group(&d2, &s2);
        assert_eq!(out2.n_rows(), 1);
        assert_eq!(out2.rows()[0], vec!["Sales", "3"], "first-seen display key");
    }

    #[test]
    fn visible_scope_excludes_hidden_rows() {
        let mut d = fixture();
        d.set_filter(vec![0, 1]).unwrap(); // only the first two sales rows visible
        let s = GroupBySpec {
            scope: ExportScope::VisibleRows,
            ..spec(vec![0], vec![agg(Aggregate::Sum, Some(1))])
        };
        let out = run_group(&d, &s);
        assert_eq!(out.n_rows(), 1);
        assert_eq!(out.rows()[0], vec!["sales", "30"]);
    }

    #[test]
    fn blank_key_policies() {
        let d = doc("g,n\na,1\n,2\na,3\n");
        let keep = run_group(&d, &spec(vec![0], vec![agg(Aggregate::Sum, Some(1))]));
        assert_eq!(keep.n_rows(), 2, "blank key forms its own group");
        assert_eq!(keep.rows()[0], vec!["", "2"]); // ByKey: blank sorts first

        let mut s = spec(vec![0], vec![agg(Aggregate::Sum, Some(1))]);
        s.blank_keys = BlankKeys::Exclude;
        let excl = run_group(&d, &s);
        assert_eq!(excl.n_rows(), 1);
        let p = preview(&d, &s).unwrap();
        assert_eq!(p.blank_key_rows, 1);
    }

    #[test]
    fn concat_respects_separator_cap_and_distinct() {
        let d = doc("g,v\na,x\na,y\na,x\n");
        let mut s = spec(
            vec![0],
            vec![
                agg(Aggregate::Concat, Some(1)),
                agg(Aggregate::ConcatDistinct, Some(1)),
            ],
        );
        s.concat_separator = " | ".into();
        let out = run_group(&d, &s);
        assert_eq!(out.rows()[0][1], "x | y | x");
        assert_eq!(out.rows()[0][2], "x | y");

        // The cap truncates with an ellipsis.
        let mut capped = spec(vec![0], vec![agg(Aggregate::Concat, Some(1))]);
        capped.concat_max_len = 4;
        let out = run_group(&d, &capped);
        assert!(out.rows()[0][1].ends_with('…'));
        assert!(out.rows()[0][1].len() <= 4 + '…'.len_utf8());
    }

    #[test]
    fn ordering_modes_are_deterministic() {
        let d = doc("g,n\nb,1\na,1\nb,1\nc,1\n");
        let by_count = GroupBySpec {
            ordering: GroupOrdering::ByCountDesc,
            ..spec(vec![0], vec![agg(Aggregate::Count, None)])
        };
        let out = run_group(&d, &by_count);
        assert_eq!(out.rows()[0][0], "b"); // 2 rows
        assert_eq!(out.rows()[1][0], "a"); // ties (a=c=1) resolve by key
        assert_eq!(out.rows()[2][0], "c");

        let first_seen = GroupBySpec {
            ordering: GroupOrdering::FirstSeen,
            ..spec(vec![0], vec![agg(Aggregate::Count, None)])
        };
        let out = run_group(&d, &first_seen);
        assert_eq!(out.rows()[0][0], "b");
        assert_eq!(out.rows()[1][0], "a");
        assert_eq!(out.rows()[2][0], "c");
    }

    #[test]
    fn custom_names_and_collisions_stay_unique() {
        let d = doc("g,n\na,1\n");
        let mut s = spec(
            vec![0],
            vec![agg(Aggregate::Sum, Some(1)), agg(Aggregate::Mean, Some(1))],
        );
        s.aggregates[0].output_name = Some("g".into()); // collides with the key
        s.aggregates[1].output_name = Some("g".into());
        let out = run_group(&d, &s);
        assert_eq!(out.headers(), &["g", "g (2)", "g (3)"]);
    }

    #[test]
    fn source_is_never_modified_and_output_is_new() {
        let d = fixture();
        let before = d.revision();
        let out = run_group(&d, &spec(vec![0], vec![agg(Aggregate::Count, None)]));
        assert_eq!(d.revision(), before);
        assert!(out.is_dirty(), "derived output starts unsaved");
        assert_eq!(d.n_rows(), 5, "source row count unchanged");
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let d = fixture();
        assert!(preview(&d, &spec(vec![], vec![agg(Aggregate::Count, None)])).is_err());
        assert!(preview(&d, &spec(vec![0], vec![])).is_err());
        assert!(preview(&d, &spec(vec![9], vec![agg(Aggregate::Count, None)])).is_err());
        assert!(preview(&d, &spec(vec![0], vec![agg(Aggregate::Sum, None)])).is_err());
        assert!(preview(&d, &spec(vec![0], vec![agg(Aggregate::Sum, Some(9))])).is_err());
        let mut s = spec(vec![0], vec![agg(Aggregate::Concat, Some(1))]);
        s.concat_max_len = 0;
        assert!(preview(&d, &s).is_err());
    }
}
