//! Outlier and anomaly finder (F30): flag suspicious values WITHOUT treating
//! them as errors. Robust methods by default (IQR, MAD), group-wise or
//! whole-column, with an explicit blank/non-numeric policy: blanks and
//! unparseable cells are excluded from statistics, never flagged, and both
//! are counted in the report. Scanning is read-only and never dirties the
//! document; every corrective action previews first and applies as one
//! undo step.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;
use crate::job::JobCtx;

/// Flagged samples carried by the report (full counts still reported).
const SAMPLE_LIMIT: usize = 200;
/// Group summaries carried by the report.
const GROUP_LIMIT: usize = 200;
const EXAMPLE_LIMIT: usize = 20;
const ROW_CHUNK: usize = 4096;
/// 0.6745 ≈ Φ⁻¹(0.75): scales MAD to be comparable with a z-score.
const MAD_SCALE: f64 = 0.6745;

/// The closed set of detection methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OutlierMethod {
    /// Outside `[Q1 - k·IQR, Q3 + k·IQR]` (robust; the classic k is 1.5).
    Iqr { k: f64 },
    /// Modified z-score `0.6745·(x − median)/MAD` beyond the threshold
    /// (robust; the classic threshold is 3.5). When MAD is zero, values
    /// differing from the median are flagged; a constant group flags none.
    Mad { threshold: f64 },
    /// Standard z-score `(x − mean)/σ` beyond the threshold (σ from the
    /// sample; a constant group has σ = 0 and flags nothing).
    ZScore { threshold: f64 },
    /// Outside the `[lower, upper]` percentile bounds (linear-interpolated).
    Percentile { lower: f64, upper: f64 },
    /// Categorical: values whose share of the group is below `max_share`.
    RareCategory { max_share: f64 },
    /// Categorical: values not in the allowed list (trimmed exact match).
    UnexpectedCategory { allowed: Vec<String> },
    /// Categorical: values not fully matching a regex — e.g. the matching
    /// file profile's rule for this column.
    PatternMismatch { pattern: String },
}

impl OutlierMethod {
    fn is_numeric(&self) -> bool {
        matches!(
            self,
            OutlierMethod::Iqr { .. }
                | OutlierMethod::Mad { .. }
                | OutlierMethod::ZScore { .. }
                | OutlierMethod::Percentile { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlierSpec {
    pub column: usize,
    pub method: OutlierMethod,
    /// Group-wise analysis: statistics computed per group key.
    #[serde(default)]
    pub group_columns: Vec<usize>,
    pub scope: ExportScope,
}

/// Corrective actions — all previewed, all one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutlierAction {
    /// Replace flagged values with a blank.
    ReplaceBlank,
    /// Replace flagged values with their group's median (numeric methods).
    ReplaceMedian,
    /// Clamp flagged values to their group's bounds (bounded methods).
    CapToBounds,
    /// Remove the rows containing flagged values.
    RemoveRows,
}

/// Summary statistics for one analysed group.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSummary {
    /// Group-key values ([] for whole-column analysis).
    pub key: Vec<String>,
    /// Numeric values (or non-blank categorical values) considered.
    pub count: usize,
    pub flagged: usize,
    pub mean: Option<f64>,
    pub median: Option<f64>,
    pub std_dev: Option<f64>,
    pub q1: Option<f64>,
    pub q3: Option<f64>,
    pub mad: Option<f64>,
    /// The effective bounds values were checked against, when the method
    /// defines them.
    pub lower: Option<f64>,
    pub upper: Option<f64>,
}

/// One flagged value.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedValue {
    /// Absolute row index.
    pub row: usize,
    pub value: String,
    pub group: Vec<String>,
    /// Why this value was flagged (bounds, score, share, …).
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlierReport {
    pub revision: u64,
    pub scanned_rows: usize,
    /// Non-blank cells that entered the analysis.
    pub considered: usize,
    pub flagged: usize,
    /// Blank cells (excluded by policy, never flagged).
    pub blanks: usize,
    /// Non-blank cells that could not be read as numbers under a numeric
    /// method (excluded by policy, never flagged).
    pub invalid_numeric: usize,
    /// Per-group summaries (bounded; `groups_total` counts all).
    pub groups: Vec<GroupSummary>,
    pub groups_total: usize,
    /// First flagged values (bounded).
    pub sample: Vec<FlaggedValue>,
}

fn validate(doc: &Document, spec: &OutlierSpec) -> AppResult<()> {
    if spec.column >= doc.n_cols() {
        return Err(AppError::invalid("column out of range"));
    }
    if let Some(&bad) = spec.group_columns.iter().find(|&&c| c >= doc.n_cols()) {
        return Err(AppError::invalid(format!(
            "grouping column {bad} is out of range"
        )));
    }
    match &spec.method {
        OutlierMethod::Iqr { k } => {
            if !k.is_finite() || *k <= 0.0 {
                return Err(AppError::invalid("k must be a positive number"));
            }
        }
        OutlierMethod::Mad { threshold } | OutlierMethod::ZScore { threshold } => {
            if !threshold.is_finite() || *threshold <= 0.0 {
                return Err(AppError::invalid("threshold must be a positive number"));
            }
        }
        OutlierMethod::Percentile { lower, upper } => {
            if !lower.is_finite()
                || !upper.is_finite()
                || *lower < 0.0
                || *upper > 100.0
                || lower >= upper
            {
                return Err(AppError::invalid(
                    "percentile bounds must satisfy 0 ≤ lower < upper ≤ 100",
                ));
            }
        }
        OutlierMethod::RareCategory { max_share } => {
            if !max_share.is_finite() || !(0.0..=1.0).contains(max_share) {
                return Err(AppError::invalid("share must be between 0 and 1"));
            }
        }
        OutlierMethod::UnexpectedCategory { allowed } => {
            if allowed.is_empty() {
                return Err(AppError::invalid("add at least one allowed value"));
            }
        }
        OutlierMethod::PatternMismatch { pattern } => {
            regex::Regex::new(pattern)
                .map_err(|e| AppError::invalid(format!("invalid pattern: {e}")))?;
        }
    }
    Ok(())
}

/// Percentile with linear interpolation over a SORTED slice (R-7).
fn percentile(sorted: &[f64], p: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let t = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * t
}

fn median_of(sorted: &[f64]) -> f64 {
    percentile(sorted, 50.0)
}

/// Per-group derived statistics and the flagging predicate.
struct GroupStats {
    summary: GroupSummary,
    /// For numeric methods: (lower, upper) bounds when defined.
    bounds: Option<(f64, f64)>,
    median: Option<f64>,
    /// For MAD: (median, mad, threshold).
    mad: Option<(f64, f64, f64)>,
    /// For z-score: (mean, std, threshold).
    z: Option<(f64, f64, f64)>,
    /// For categorical: per-value share below which a value is rare.
    rare: Option<(HashMap<String, usize>, usize, f64)>,
}

/// One (group key -> collected values) pass, shared by scan and actions.
struct Collected {
    /// Scope row -> (abs row, raw cell, group key).
    rows: Vec<(usize, String, Vec<String>)>,
    blanks: usize,
    invalid_numeric: usize,
}

fn collect(doc: &Document, spec: &OutlierSpec, ctx: Option<&JobCtx>) -> AppResult<Collected> {
    let scope_rows = resolve_scope(doc, &spec.scope)?.rows;
    if let Some(ctx) = ctx {
        ctx.set_total(scope_rows.len() as u64);
    }
    let mut rows = Vec::with_capacity(scope_rows.len());
    let mut pending = 0u64;
    doc.visit_rows_at(&scope_rows, &mut |i, row| {
        let cell = row.get(spec.column).cloned().unwrap_or_default();
        let key: Vec<String> = spec
            .group_columns
            .iter()
            .map(|&c| row.get(c).map(|v| v.trim().to_string()).unwrap_or_default())
            .collect();
        rows.push((i, cell, key));
        pending += 1;
        if pending >= ROW_CHUNK as u64 {
            if let Some(ctx) = ctx {
                ctx.advance(pending)?;
            }
            pending = 0;
        }
        Ok(true)
    })?;
    if let Some(ctx) = ctx {
        ctx.advance(pending)?;
    }
    Ok(Collected {
        rows,
        blanks: 0,
        invalid_numeric: 0,
    })
}

/// Build per-group statistics for the collected values.
fn group_stats(
    collected: &mut Collected,
    method: &OutlierMethod,
) -> AppResult<HashMap<Vec<String>, GroupStats>> {
    // Partition values per group under the blank/invalid policy.
    let mut numeric: HashMap<Vec<String>, Vec<f64>> = HashMap::new();
    let mut categorical: HashMap<Vec<String>, HashMap<String, usize>> = HashMap::new();
    let mut totals: HashMap<Vec<String>, usize> = HashMap::new();
    for (_, cell, key) in &collected.rows {
        let t = cell.trim();
        if t.is_empty() {
            collected.blanks += 1;
            continue;
        }
        *totals.entry(key.clone()).or_insert(0) += 1;
        if method.is_numeric() {
            match analyze::as_number(t) {
                Some(n) => numeric.entry(key.clone()).or_default().push(n),
                None => collected.invalid_numeric += 1,
            }
        } else {
            *categorical
                .entry(key.clone())
                .or_default()
                .entry(t.to_string())
                .or_insert(0) += 1;
        }
    }

    let mut out = HashMap::new();
    if method.is_numeric() {
        for (key, mut values) in numeric {
            values.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
            let n = values.len();
            let mean = values.iter().sum::<f64>() / n as f64;
            let median = median_of(&values);
            let std_dev = if n > 1 {
                (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
            } else {
                0.0
            };
            let q1 = percentile(&values, 25.0);
            let q3 = percentile(&values, 75.0);
            let mut deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
            deviations.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
            let mad = median_of(&deviations);

            let (bounds, mad_check, z_check) = match method {
                OutlierMethod::Iqr { k } => {
                    let iqr = q3 - q1;
                    (Some((q1 - k * iqr, q3 + k * iqr)), None, None)
                }
                OutlierMethod::Percentile { lower, upper } => (
                    Some((percentile(&values, *lower), percentile(&values, *upper))),
                    None,
                    None,
                ),
                OutlierMethod::Mad { threshold } => {
                    let b = if mad > 0.0 {
                        let span = threshold * mad / MAD_SCALE;
                        Some((median - span, median + span))
                    } else {
                        None // constant-ish: bounds collapse to the median
                    };
                    (b, Some((median, mad, *threshold)), None)
                }
                OutlierMethod::ZScore { threshold } => {
                    let bounds = if std_dev > 0.0 {
                        Some((mean - threshold * std_dev, mean + threshold * std_dev))
                    } else {
                        None
                    };
                    (bounds, None, Some((mean, std_dev, *threshold)))
                }
                _ => unreachable!("numeric methods only"),
            };
            out.insert(
                key.clone(),
                GroupStats {
                    summary: GroupSummary {
                        key,
                        count: n,
                        flagged: 0,
                        mean: Some(mean),
                        median: Some(median),
                        std_dev: Some(std_dev),
                        q1: Some(q1),
                        q3: Some(q3),
                        mad: Some(mad),
                        lower: bounds.map(|b| b.0),
                        upper: bounds.map(|b| b.1),
                    },
                    bounds,
                    median: Some(median),
                    mad: mad_check,
                    z: z_check,
                    rare: None,
                },
            );
        }
    } else {
        for (key, counts) in categorical {
            let total = totals.get(&key).copied().unwrap_or(0);
            out.insert(
                key.clone(),
                GroupStats {
                    summary: GroupSummary {
                        key,
                        count: total,
                        flagged: 0,
                        mean: None,
                        median: None,
                        std_dev: None,
                        q1: None,
                        q3: None,
                        mad: None,
                        lower: None,
                        upper: None,
                    },
                    bounds: None,
                    median: None,
                    mad: None,
                    z: None,
                    rare: Some((counts, total, 0.0)),
                },
            );
        }
    }
    Ok(out)
}

/// Why a value is flagged, or `None` when it passes. Blank and (for numeric
/// methods) unparseable cells are NEVER flagged — that is the policy.
fn flag_reason(
    method: &OutlierMethod,
    stats: &GroupStats,
    matcher: Option<&regex::Regex>,
    cell: &str,
) -> Option<String> {
    let t = cell.trim();
    if t.is_empty() {
        return None;
    }
    match method {
        OutlierMethod::Iqr { .. } | OutlierMethod::Percentile { .. } => {
            let n = analyze::as_number(t)?;
            let (lo, hi) = stats.bounds?;
            if n < lo {
                Some(format!("{n} < lower bound {}", round(lo)))
            } else if n > hi {
                Some(format!("{n} > upper bound {}", round(hi)))
            } else {
                None
            }
        }
        OutlierMethod::Mad { .. } => {
            let n = analyze::as_number(t)?;
            let (median, mad, threshold) = stats.mad?;
            if mad > 0.0 {
                let score = MAD_SCALE * (n - median) / mad;
                (score.abs() > threshold)
                    .then(|| format!("modified z-score {} beyond ±{threshold}", round(score)))
            } else {
                // MAD of zero: an otherwise-constant group; any deviation
                // from the median is suspicious, and a constant group
                // flags nothing (no division happens either way).
                (n != median).then(|| format!("differs from constant group value {median}"))
            }
        }
        OutlierMethod::ZScore { .. } => {
            let n = analyze::as_number(t)?;
            let (mean, std_dev, threshold) = stats.z?;
            if std_dev > 0.0 {
                let score = (n - mean) / std_dev;
                (score.abs() > threshold)
                    .then(|| format!("z-score {} beyond ±{threshold}", round(score)))
            } else {
                None // constant group: nothing is anomalous
            }
        }
        OutlierMethod::RareCategory { max_share } => {
            let (counts, total, _) = stats.rare.as_ref()?;
            let count = counts.get(t).copied().unwrap_or(0);
            if *total == 0 {
                return None;
            }
            let share = count as f64 / *total as f64;
            (share < *max_share)
                .then(|| format!("appears {count}× ({:.2}% of group)", share * 100.0))
        }
        OutlierMethod::UnexpectedCategory { allowed } => allowed
            .iter()
            .all(|a| a.trim() != t)
            .then(|| "not in the allowed values".to_string()),
        OutlierMethod::PatternMismatch { .. } => {
            let re = matcher?;
            let full = re
                .find(t)
                .map(|m| m.start() == 0 && m.end() == t.len())
                .unwrap_or(false);
            (!full).then(|| "does not match the expected pattern".to_string())
        }
    }
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Run an outlier scan. Read-only; never dirties the document.
pub fn scan(doc: &Document, spec: &OutlierSpec, ctx: &JobCtx) -> AppResult<OutlierReport> {
    validate(doc, spec)?;
    let matcher = compile_matcher(&spec.method)?;
    let mut collected = collect(doc, spec, Some(ctx))?;
    let mut stats = group_stats(&mut collected, &spec.method)?;

    let mut sample = Vec::new();
    let mut flagged_total = 0usize;
    for (abs, cell, key) in &collected.rows {
        let Some(group) = stats.get_mut(key) else {
            continue;
        };
        if let Some(reason) = flag_reason(&spec.method, group, matcher.as_ref(), cell) {
            flagged_total += 1;
            group.summary.flagged += 1;
            if sample.len() < SAMPLE_LIMIT {
                sample.push(FlaggedValue {
                    row: *abs,
                    value: cell.clone(),
                    group: key.clone(),
                    reason,
                });
            }
        }
    }

    let considered: usize = stats.values().map(|g| g.summary.count).sum();
    let groups_total = stats.len();
    let mut groups: Vec<GroupSummary> = stats.into_values().map(|g| g.summary).collect();
    // Deterministic: most-flagged first, then by key.
    groups.sort_by(|a, b| b.flagged.cmp(&a.flagged).then_with(|| a.key.cmp(&b.key)));
    groups.truncate(GROUP_LIMIT);

    Ok(OutlierReport {
        revision: doc.revision(),
        scanned_rows: collected.rows.len(),
        considered,
        flagged: flagged_total,
        blanks: collected.blanks,
        invalid_numeric: collected.invalid_numeric,
        groups,
        groups_total,
        sample,
    })
}

fn compile_matcher(method: &OutlierMethod) -> AppResult<Option<regex::Regex>> {
    match method {
        OutlierMethod::PatternMismatch { pattern } => regex::Regex::new(pattern)
            .map(Some)
            .map_err(|e| AppError::invalid(format!("invalid pattern: {e}"))),
        _ => Ok(None),
    }
}

/// Absolute rows whose value in the column is flagged (for the filter).
pub fn flagged_rows(doc: &Document, spec: &OutlierSpec) -> AppResult<Vec<usize>> {
    validate(doc, spec)?;
    let matcher = compile_matcher(&spec.method)?;
    let mut collected = collect(doc, spec, None)?;
    let stats = group_stats(&mut collected, &spec.method)?;
    let mut out = Vec::new();
    for (abs, cell, key) in &collected.rows {
        if let Some(group) = stats.get(key) {
            if flag_reason(&spec.method, group, matcher.as_ref(), cell).is_some() {
                out.push(*abs);
            }
        }
    }
    Ok(out)
}

/// One before/after example for the action preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlierExample {
    pub row: usize,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlierActionPreview {
    pub revision: u64,
    pub cells_affected: usize,
    pub rows_removed: usize,
    pub examples: Vec<OutlierExample>,
}

/// Everything a corrective action would do, computed without mutating.
pub struct OutlierComputed {
    pub changes: Vec<(usize, usize, String)>,
    pub remove_rows: Vec<usize>,
    pub preview: OutlierActionPreview,
}

/// Compute a corrective action over the CURRENT document state.
pub fn action_changes(
    doc: &Document,
    spec: &OutlierSpec,
    action: OutlierAction,
) -> AppResult<OutlierComputed> {
    validate(doc, spec)?;
    if matches!(
        action,
        OutlierAction::ReplaceMedian | OutlierAction::CapToBounds
    ) && !spec.method.is_numeric()
    {
        return Err(AppError::invalid(
            "median/cap corrections need a numeric method",
        ));
    }
    let matcher = compile_matcher(&spec.method)?;
    let mut collected = collect(doc, spec, None)?;
    let stats = group_stats(&mut collected, &spec.method)?;

    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut remove_rows: Vec<usize> = Vec::new();
    let mut examples: Vec<OutlierExample> = Vec::new();
    for (abs, cell, key) in &collected.rows {
        let Some(group) = stats.get(key) else {
            continue;
        };
        if flag_reason(&spec.method, group, matcher.as_ref(), cell).is_none() {
            continue;
        }
        let after: Option<String> = match action {
            OutlierAction::ReplaceBlank => Some(String::new()),
            OutlierAction::ReplaceMedian => group.median.map(format_number),
            OutlierAction::CapToBounds => {
                let n = analyze::as_number(cell.trim());
                match (n, group.bounds) {
                    (Some(n), Some((lo, hi))) => Some(format_number(n.clamp(lo, hi))),
                    // No finite bounds (e.g. MAD = 0): fall back to median.
                    (Some(_), None) => group.median.map(format_number),
                    _ => None,
                }
            }
            OutlierAction::RemoveRows => {
                remove_rows.push(*abs);
                None
            }
        };
        if let Some(after) = after {
            if after != *cell {
                if examples.len() < EXAMPLE_LIMIT {
                    examples.push(OutlierExample {
                        row: *abs,
                        before: cell.clone(),
                        after: after.clone(),
                    });
                }
                changes.push((*abs, spec.column, after));
            }
        }
    }

    let preview = OutlierActionPreview {
        revision: doc.revision(),
        cells_affected: changes.len(),
        rows_removed: remove_rows.len(),
        examples,
    };
    Ok(OutlierComputed {
        changes,
        remove_rows,
        preview,
    })
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// Last completed outlier report + the spec that produced it, per document.
pub type CachedOutlier = (OutlierSpec, OutlierReport);

#[derive(Default)]
pub struct OutlierCache(Arc<Mutex<HashMap<u64, CachedOutlier>>>);

impl OutlierCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, CachedOutlier>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<CachedOutlier> {
        self.0.lock().ok()?.get(&doc_id).cloned()
    }

    pub fn remove(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&doc_id);
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

    fn spec(method: OutlierMethod, column: usize) -> OutlierSpec {
        OutlierSpec {
            column,
            method,
            group_columns: vec![],
            scope: ExportScope::All,
        }
    }

    fn run(d: &Document, s: &OutlierSpec) -> OutlierReport {
        let registry = JobRegistry::default();
        let ctx = registry.begin("outlier", None, |_| {});
        scan(d, s, &ctx).unwrap()
    }

    #[test]
    fn iqr_flags_the_classic_outlier() {
        // 1..=9 plus 100: Q1=2.75(ish), Q3=7.25(ish) with R-7 -> 100 is out.
        let mut csv = String::from("n,x\n");
        for i in 1..=9 {
            csv.push_str(&format!("{i},1\n"));
        }
        csv.push_str("100,1\n");
        let d = doc(&csv);
        let r = run(&d, &spec(OutlierMethod::Iqr { k: 1.5 }, 0));
        assert_eq!(r.flagged, 1);
        assert_eq!(r.sample[0].value, "100");
        assert!(r.sample[0].reason.contains("upper bound"));
        assert_eq!(r.considered, 10);
    }

    #[test]
    fn percentile_and_zscore_match_fixtures() {
        let d = doc("n,x\n1,1\n2,1\n3,1\n4,1\n5,1\n6,1\n7,1\n8,1\n9,1\n10,1\n");
        // p10..p90 of 1..=10 (R-7): 1.9 and 9.1 -> flags 1 and 10.
        let r = run(
            &d,
            &spec(
                OutlierMethod::Percentile {
                    lower: 10.0,
                    upper: 90.0,
                },
                0,
            ),
        );
        assert_eq!(r.flagged, 2);

        // z-score: mean 5.5, sample σ ≈ 3.0277 -> nothing beyond ±2.
        let z = run(&d, &spec(OutlierMethod::ZScore { threshold: 2.0 }, 0));
        assert_eq!(z.flagged, 0);
        // …but a wild value is beyond ±2.
        let d2 = doc("n,x\n1,1\n2,1\n3,1\n4,1\n5,1\n1000,1\n");
        let z2 = run(&d2, &spec(OutlierMethod::ZScore { threshold: 2.0 }, 0));
        assert_eq!(z2.flagged, 1);
        assert_eq!(z2.sample[0].value, "1000");
    }

    #[test]
    fn mad_flags_and_handles_zero_mad_without_panicking() {
        // Median 3, MAD 1: 100 has a huge modified z-score.
        let d = doc("n,x\n1,1\n2,1\n3,1\n4,1\n5,1\n100,1\n");
        let r = run(&d, &spec(OutlierMethod::Mad { threshold: 3.5 }, 0));
        assert_eq!(r.flagged, 1);

        // MAD = 0 (nearly constant): the deviant is still caught…
        let d0 = doc("n,x\n7,1\n7,1\n7,1\n7,1\n9,1\n");
        let r0 = run(&d0, &spec(OutlierMethod::Mad { threshold: 3.5 }, 0));
        assert_eq!(r0.flagged, 1);
        assert!(r0.sample[0].reason.contains("constant"));

        // …and a fully constant column flags nothing (no division by zero).
        let dc = doc("n,x\n7,1\n7,1\n7,1\n");
        let rc = run(&dc, &spec(OutlierMethod::Mad { threshold: 3.5 }, 0));
        assert_eq!(rc.flagged, 0);
        let zc = run(&dc, &spec(OutlierMethod::ZScore { threshold: 2.0 }, 0));
        assert_eq!(zc.flagged, 0, "constant column, σ=0: nothing flagged");
    }

    #[test]
    fn group_wise_analysis_uses_each_groups_own_statistics() {
        // Group a lives around 10, group b around 1000. 60 would be an
        // outlier in a but not in the pooled data.
        let d = doc("g,n\na,9\na,10\na,11\na,10\na,60\nb,950\nb,1000\nb,1100\nb,1000\nb,1050\n");
        let mut s = spec(OutlierMethod::Iqr { k: 1.5 }, 1);
        s.group_columns = vec![0];
        let r = run(&d, &s);
        assert_eq!(r.flagged, 1);
        assert_eq!(r.sample[0].value, "60");
        assert_eq!(r.sample[0].group, vec!["a".to_string()]);
        assert_eq!(r.groups_total, 2);

        // Pooled (no grouping): 60 sits between the clusters and the IQR
        // fences are far apart, so nothing is flagged.
        let pooled = run(&d, &spec(OutlierMethod::Iqr { k: 1.5 }, 1));
        assert_eq!(pooled.flagged, 0);
    }

    #[test]
    fn blanks_and_invalid_numerics_follow_the_documented_policy() {
        let d = doc("n,x\n1,1\n2,1\n,1\nabc,1\n3,1\n100,1\n");
        let r = run(&d, &spec(OutlierMethod::Iqr { k: 1.5 }, 0));
        assert_eq!(r.blanks, 1);
        assert_eq!(r.invalid_numeric, 1);
        assert_eq!(r.considered, 4, "1, 2, 3, 100 enter the statistics");
        // Neither the blank nor "abc" is ever flagged.
        assert!(r
            .sample
            .iter()
            .all(|f| f.value != "abc" && !f.value.is_empty()));
    }

    #[test]
    fn categorical_methods_flag_rare_and_unexpected_values() {
        let mut csv = String::from("s,x\n");
        for _ in 0..50 {
            csv.push_str("common,1\n");
        }
        csv.push_str("rare,1\n");
        let d = doc(&csv);
        let rare = run(
            &d,
            &spec(OutlierMethod::RareCategory { max_share: 0.05 }, 0),
        );
        assert_eq!(rare.flagged, 1);
        assert_eq!(rare.sample[0].value, "rare");

        let unexpected = run(
            &d,
            &spec(
                OutlierMethod::UnexpectedCategory {
                    allowed: vec!["common".into()],
                },
                0,
            ),
        );
        assert_eq!(unexpected.flagged, 1);

        let pattern = run(
            &d,
            &spec(
                OutlierMethod::PatternMismatch {
                    pattern: "^common$".into(),
                },
                0,
            ),
        );
        assert_eq!(pattern.flagged, 1);
    }

    #[test]
    fn scanning_never_dirties_or_mutates() {
        let d = doc("n,x\n1,1\n100,1\n");
        let before_rows = d.rows().to_vec();
        let before_rev = d.revision();
        let _ = run(&d, &spec(OutlierMethod::Iqr { k: 1.5 }, 0));
        assert_eq!(d.rows(), &before_rows[..]);
        assert_eq!(d.revision(), before_rev, "scan never bumps the revision");
    }

    #[test]
    fn corrective_actions_are_previewed_and_one_undo() {
        let mut d = doc("n,x\n1,1\n2,1\n3,1\n4,1\n5,1\n100,1\n");
        let s = spec(OutlierMethod::Mad { threshold: 3.5 }, 0);

        let median = action_changes(&d, &s, OutlierAction::ReplaceMedian).unwrap();
        assert_eq!(median.preview.cells_affected, 1);
        assert_eq!(median.changes[0].2, "3.5"); // median of 1..5,100
        assert_eq!(median.preview.examples[0].before, "100");

        let cap = action_changes(&d, &s, OutlierAction::CapToBounds).unwrap();
        assert_eq!(cap.changes.len(), 1);
        let capped: f64 = cap.changes[0].2.parse().unwrap();
        assert!(capped < 100.0 && capped > 5.0, "clamped to the upper bound");

        let blank = action_changes(&d, &s, OutlierAction::ReplaceBlank).unwrap();
        assert_eq!(blank.changes[0].2, "");

        let remove = action_changes(&d, &s, OutlierAction::RemoveRows).unwrap();
        assert_eq!(remove.remove_rows, vec![5]);

        // Apply one and undo restores the original value in one step.
        d.set_cells(median.changes).unwrap();
        assert_eq!(d.rows()[5][0], "3.5");
        d.undo().unwrap();
        assert_eq!(d.rows()[5][0], "100");
        assert!(!d.can_undo());
    }

    #[test]
    fn invalid_configurations_are_rejected_before_scanning() {
        let d = doc("n,x\n1,1\n");
        let bad: Vec<OutlierMethod> = vec![
            OutlierMethod::Iqr { k: 0.0 },
            OutlierMethod::Mad { threshold: -1.0 },
            OutlierMethod::Percentile {
                lower: 90.0,
                upper: 10.0,
            },
            OutlierMethod::RareCategory { max_share: 2.0 },
            OutlierMethod::UnexpectedCategory { allowed: vec![] },
            OutlierMethod::PatternMismatch {
                pattern: "(".into(),
            },
        ];
        for method in bad {
            let registry = JobRegistry::default();
            let ctx = registry.begin("outlier", None, |_| {});
            assert!(
                scan(&d, &spec(method.clone(), 0), &ctx).is_err(),
                "{method:?}"
            );
        }
        let registry = JobRegistry::default();
        let ctx = registry.begin("outlier", None, |_| {});
        assert!(scan(&d, &spec(OutlierMethod::Iqr { k: 1.5 }, 9), &ctx).is_err());
    }

    #[test]
    fn median_and_cap_reject_categorical_methods() {
        let d = doc("s,x\na,1\nb,1\n");
        let s = spec(
            OutlierMethod::UnexpectedCategory {
                allowed: vec!["a".into()],
            },
            0,
        );
        assert!(action_changes(&d, &s, OutlierAction::ReplaceMedian).is_err());
        assert!(action_changes(&d, &s, OutlierAction::CapToBounds).is_err());
        assert!(action_changes(&d, &s, OutlierAction::ReplaceBlank).is_ok());
    }
}
