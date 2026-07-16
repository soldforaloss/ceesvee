//! Interactive column profiling (F05): type distribution, blanks, distinct
//! counts, top values, numeric quartiles, date extremes and text lengths for
//! one column, over all rows or just the visible (filtered) ones.
//!
//! High-cardinality columns stay bounded in memory: top values use the
//! Misra-Gries (space-saving) sketch with [`TOP_K_CAPACITY`] counters, and
//! the distinct count switches from an exact set to a HyperLogLog estimate
//! above [`DISTINCT_EXACT_LIMIT`] values. Approximate results are flagged so
//! the UI can label them.
//!
//! Profiles are cached per (document, column, scope) tagged with the document
//! revision they were computed against; validity is judged against the
//! COLUMN's last-change revision (plus the filter revision for visible-rows
//! scope), so editing column A does not invalidate column B's cached profile.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::analyze::{self, CellClass};
use crate::document::Document;
use crate::dto::ColumnKind;
use crate::error::AppResult;
use crate::job::JobCtx;

/// Above this many distinct values the profiler stops keeping an exact set
/// and reports a HyperLogLog estimate (labelled approximate).
pub const DISTINCT_EXACT_LIMIT: usize = 100_000;
/// Counter capacity of the Misra-Gries top-K sketch. Counts are exact while
/// the column's distinct-value count fits the capacity; beyond it they are
/// lower bounds (labelled approximate).
pub const TOP_K_CAPACITY: usize = 1024;
/// Cancellation/progress granularity.
const ROW_CHUNK: usize = 4096;

/// Which rows a profile covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProfileScope {
    All,
    VisibleRows,
}

/// Options for a profile request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileOptions {
    /// How many top values to return (bounded by the sketch capacity).
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_top_k() -> usize {
    50
}

impl Default for ProfileOptions {
    fn default() -> Self {
        ProfileOptions {
            top_k: default_top_k(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueCount {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeCounts {
    pub number: usize,
    pub date: usize,
    pub bool: usize,
    pub text: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericProfile {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub median: f64,
    pub q1: f64,
    pub q3: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextProfile {
    pub min_len: usize,
    pub max_len: usize,
    pub avg_len: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnProfile {
    pub column: usize,
    pub scope: ProfileScope,
    /// Document revision this profile was computed against.
    pub revision: u64,
    /// Rows covered by the scope.
    pub row_count: usize,
    pub blank_count: usize,
    pub inferred_kind: ColumnKind,
    pub type_counts: TypeCounts,
    pub distinct_count: u64,
    pub distinct_is_approximate: bool,
    pub top_values: Vec<ValueCount>,
    pub top_is_approximate: bool,
    pub numeric: Option<NumericProfile>,
    /// Original cell text of the earliest / latest parsed date.
    pub earliest_date: Option<String>,
    pub latest_date: Option<String>,
    /// Character lengths over non-blank cells.
    pub text: Option<TextProfile>,
}

// ----- bounded-memory sketches ---------------------------------------------------

/// Misra-Gries heavy hitters: at most `capacity` counters; counts are exact
/// until the first eviction, lower bounds afterwards.
struct TopK {
    capacity: usize,
    counters: HashMap<String, u64>,
    evicted: bool,
}

impl TopK {
    fn new(capacity: usize) -> TopK {
        TopK {
            capacity,
            counters: HashMap::with_capacity(capacity + 1),
            evicted: false,
        }
    }

    fn add(&mut self, value: &str) {
        if let Some(count) = self.counters.get_mut(value) {
            *count += 1;
            return;
        }
        if self.counters.len() < self.capacity {
            self.counters.insert(value.to_string(), 1);
            return;
        }
        // Decrement-all step: shrink every counter, dropping zeros.
        self.evicted = true;
        self.counters.retain(|_, count| {
            *count -= 1;
            *count > 0
        });
    }

    fn top(mut self, k: usize) -> (Vec<ValueCount>, bool) {
        let mut all: Vec<ValueCount> = self
            .counters
            .drain()
            .map(|(value, count)| ValueCount { value, count })
            .collect();
        // Highest count first; ties by value for determinism.
        all.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
        all.truncate(k);
        (all, self.evicted)
    }
}

/// Minimal HyperLogLog (2^12 registers, ~1.6% typical error) for distinct
/// counts past the exact limit.
struct HyperLogLog {
    registers: Vec<u8>,
}

const HLL_BITS: u32 = 12;
const HLL_REGISTERS: usize = 1 << HLL_BITS;

impl HyperLogLog {
    fn new() -> HyperLogLog {
        HyperLogLog {
            registers: vec![0; HLL_REGISTERS],
        }
    }

    fn add_hash(&mut self, hash: u64) {
        let index = (hash >> (64 - HLL_BITS)) as usize;
        let rest = hash << HLL_BITS;
        let rank = (rest.leading_zeros() + 1).min(64 - HLL_BITS + 1) as u8;
        if rank > self.registers[index] {
            self.registers[index] = rank;
        }
    }

    fn estimate(&self) -> f64 {
        let m = HLL_REGISTERS as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let sum: f64 = self
            .registers
            .iter()
            .map(|&r| 2f64.powi(-i32::from(r)))
            .sum();
        let raw = alpha * m * m / sum;
        if raw <= 2.5 * m {
            // Small-range correction (linear counting).
            let zeros = self.registers.iter().filter(|&&r| r == 0).count();
            if zeros > 0 {
                return m * (m / zeros as f64).ln();
            }
        }
        raw
    }
}

fn hash_value(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

// ----- the scan --------------------------------------------------------------------

/// Profile one column. Read-only; progress and cancellation via `ctx`.
pub fn profile_column(
    doc: &Document,
    column: usize,
    scope: ProfileScope,
    options: &ProfileOptions,
    ctx: &JobCtx,
) -> AppResult<ColumnProfile> {
    // `None` = every row (streamed without materialising an index list).
    let view: Option<Vec<usize>> = match scope {
        ProfileScope::All => None,
        ProfileScope::VisibleRows => doc.filter_view().map(<[usize]>::to_vec),
    };
    let total_rows = view.as_ref().map(Vec::len).unwrap_or_else(|| doc.n_rows());
    ctx.set_total(total_rows as u64);

    let mut blank = 0usize;
    let mut counts = TypeCounts {
        number: 0,
        date: 0,
        bool: 0,
        text: 0,
    };
    let mut top = TopK::new(TOP_K_CAPACITY.max(options.top_k));
    let mut exact_distinct: Option<std::collections::HashSet<u64>> =
        Some(std::collections::HashSet::new());
    let mut hll = HyperLogLog::new();

    let mut numbers: Vec<f64> = Vec::new();
    let mut sum = 0.0f64;
    let mut earliest: Option<(chrono::NaiveDateTime, String)> = None;
    let mut latest: Option<(chrono::NaiveDateTime, String)> = None;
    let mut len_min = usize::MAX;
    let mut len_max = 0usize;
    let mut len_sum = 0u64;
    let mut non_blank = 0usize;

    {
        let mut i = 0usize;
        let mut visitor = |_r: usize, row: &[String]| -> AppResult<bool> {
            if i.is_multiple_of(ROW_CHUNK) {
                ctx.advance(if i == 0 { 0 } else { ROW_CHUNK as u64 })?;
            }
            i += 1;
            let cell = row.get(column).map(String::as_str).unwrap_or("");
            let trimmed = cell.trim();
            match analyze::classify(cell) {
                CellClass::Blank => {
                    blank += 1;
                    return Ok(true);
                }
                CellClass::Number => {
                    counts.number += 1;
                    if let Some(n) = analyze::as_number(trimmed) {
                        numbers.push(n);
                        sum += n;
                    }
                }
                CellClass::Date => {
                    counts.date += 1;
                    if let Some(parsed) = analyze::parse_date(trimmed) {
                        if earliest.as_ref().is_none_or(|(e, _)| parsed < *e) {
                            earliest = Some((parsed, trimmed.to_string()));
                        }
                        if latest.as_ref().is_none_or(|(l, _)| parsed > *l) {
                            latest = Some((parsed, trimmed.to_string()));
                        }
                    }
                }
                CellClass::Bool => counts.bool += 1,
                CellClass::Text => counts.text += 1,
            }

            non_blank += 1;
            let chars = trimmed.chars().count();
            len_min = len_min.min(chars);
            len_max = len_max.max(chars);
            len_sum += chars as u64;

            top.add(trimmed);
            let hash = hash_value(trimmed);
            hll.add_hash(hash);
            if let Some(set) = exact_distinct.as_mut() {
                set.insert(hash);
                if set.len() > DISTINCT_EXACT_LIMIT {
                    exact_distinct = None; // fall back to the estimate
                }
            }
            Ok(true)
        };
        match &view {
            None => doc.visit_rows(0..doc.n_rows(), &mut visitor)?,
            Some(v) => doc.visit_rows_at(v, &mut visitor)?,
        }
    }
    ctx.flush_progress();

    let (distinct_count, distinct_is_approximate) = match exact_distinct {
        Some(set) => (set.len() as u64, false),
        None => (hll.estimate().round() as u64, true),
    };

    let numeric = if numbers.is_empty() {
        None
    } else {
        numbers.sort_by(|a, b| a.partial_cmp(b).expect("profiled numbers are finite"));
        Some(NumericProfile {
            min: numbers[0],
            max: numbers[numbers.len() - 1],
            mean: sum / numbers.len() as f64,
            median: quantile(&numbers, 0.5),
            q1: quantile(&numbers, 0.25),
            q3: quantile(&numbers, 0.75),
        })
    };

    let inferred_kind = infer_kind(non_blank, &counts);
    let (top_values, top_is_approximate) = top.top(options.top_k);

    Ok(ColumnProfile {
        column,
        scope,
        revision: doc.revision(),
        row_count: total_rows,
        blank_count: blank,
        inferred_kind,
        type_counts: counts,
        distinct_count,
        distinct_is_approximate,
        top_values,
        top_is_approximate,
        numeric,
        earliest_date: earliest.map(|(_, text)| text),
        latest_date: latest.map(|(_, text)| text),
        text: (non_blank > 0).then_some(TextProfile {
            min_len: len_min,
            max_len: len_max,
            avg_len: len_sum as f64 / non_blank as f64,
        }),
    })
}

/// Same rule as the column summaries: a non-text kind only when every
/// non-blank cell matches it.
fn infer_kind(non_blank: usize, counts: &TypeCounts) -> ColumnKind {
    if non_blank == 0 {
        ColumnKind::Text
    } else if counts.number == non_blank {
        ColumnKind::Number
    } else if counts.bool == non_blank {
        ColumnKind::Bool
    } else if counts.date == non_blank {
        ColumnKind::Date
    } else {
        ColumnKind::Text
    }
}

/// Linear-interpolated quantile over a sorted slice.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
    }
}

// ----- cache ------------------------------------------------------------------------

type CacheKey = (u64, usize, ProfileScope);

/// Completed profiles keyed by (document, column, scope), managed by Tauri.
/// Entries carry the revision they were computed against; validity is judged
/// per column (plus the filter revision for visible scope), so edits to other
/// columns don't evict them.
#[derive(Default)]
pub struct ProfileCache(Arc<Mutex<HashMap<CacheKey, ColumnProfile>>>);

impl ProfileCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<CacheKey, ColumnProfile>>> {
        Arc::clone(&self.0)
    }

    /// A cached profile that is still valid for the document's current state.
    pub fn get_valid(
        &self,
        doc: &Document,
        column: usize,
        scope: ProfileScope,
    ) -> Option<ColumnProfile> {
        let map = self.0.lock().ok()?;
        let cached = map.get(&(doc.id, column, scope))?;
        if profile_is_valid(doc, cached) {
            Some(cached.clone())
        } else {
            None
        }
    }

    pub fn remove_doc(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.retain(|(id, _, _), _| *id != doc_id);
        }
    }
}

/// Whether a cached profile still describes the document: the profiled
/// column must not have changed since, nor (for visible scope) the filter.
pub fn profile_is_valid(doc: &Document, cached: &ColumnProfile) -> bool {
    if cached.column >= doc.n_cols() {
        return false;
    }
    if cached.revision < doc.column_revision(cached.column) {
        return false;
    }
    if cached.scope == ProfileScope::VisibleRows && cached.revision < doc.filter_revision() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("profile", Some(1), |_| {});
        (registry, ctx)
    }

    #[test]
    fn frequency_counts_match_exact_fixtures() {
        // Two columns so the blank-fruit row survives parsing (the csv reader
        // skips completely empty lines).
        let d = doc_from("fruit,n\napple,1\nbanana,2\napple,3\ncherry,4\napple,5\nbanana,6\n,7");
        let (_r, ctx) = ctx();
        let p = profile_column(&d, 0, ProfileScope::All, &ProfileOptions::default(), &ctx).unwrap();
        assert_eq!(p.row_count, 7);
        assert_eq!(p.blank_count, 1);
        assert_eq!(p.distinct_count, 3);
        assert!(!p.distinct_is_approximate);
        assert!(!p.top_is_approximate);
        assert_eq!(
            p.top_values
                .iter()
                .map(|v| (v.value.as_str(), v.count))
                .collect::<Vec<_>>(),
            vec![("apple", 3), ("banana", 2), ("cherry", 1)],
        );
        assert_eq!(p.inferred_kind, ColumnKind::Text);
        let text = p.text.unwrap();
        assert_eq!(text.min_len, 5);
        assert_eq!(text.max_len, 6);
    }

    #[test]
    fn visible_scope_reflects_the_active_filter() {
        let mut d = doc_from("n\n1\n2\n3\n4\n5");
        d.set_filter(vec![0, 2, 4]).unwrap(); // 1, 3, 5
        let (_r, ctx) = ctx();
        let all =
            profile_column(&d, 0, ProfileScope::All, &ProfileOptions::default(), &ctx).unwrap();
        assert_eq!(all.row_count, 5);
        let visible = profile_column(
            &d,
            0,
            ProfileScope::VisibleRows,
            &ProfileOptions::default(),
            &ctx,
        )
        .unwrap();
        assert_eq!(visible.row_count, 3);
        let num = visible.numeric.unwrap();
        assert_eq!(num.min, 1.0);
        assert_eq!(num.max, 5.0);
        assert_eq!(num.median, 3.0);
    }

    #[test]
    fn numeric_quartiles_and_dates_and_types() {
        let d = doc_from("v\n1\n2\n3\n4\n5\n6\n7\n8");
        let (_r, ctx) = ctx();
        let p = profile_column(&d, 0, ProfileScope::All, &ProfileOptions::default(), &ctx).unwrap();
        let num = p.numeric.unwrap();
        assert_eq!(num.median, 4.5);
        assert_eq!(num.q1, 2.75);
        assert_eq!(num.q3, 6.25);
        assert_eq!(p.inferred_kind, ColumnKind::Number);

        let d = doc_from("when\n2024-03-01\n2023-12-25\n2024-01-15");
        let p = profile_column(&d, 0, ProfileScope::All, &ProfileOptions::default(), &ctx).unwrap();
        assert_eq!(p.earliest_date.as_deref(), Some("2023-12-25"));
        assert_eq!(p.latest_date.as_deref(), Some("2024-03-01"));
        assert_eq!(p.inferred_kind, ColumnKind::Date);
        assert_eq!(p.type_counts.date, 3);
    }

    #[test]
    fn top_k_is_bounded_and_flags_approximation() {
        let mut top = TopK::new(4);
        for _ in 0..100 {
            top.add("heavy");
        }
        for i in 0..50 {
            top.add(&format!("rare-{i}"));
        }
        assert!(top.counters.len() <= 4, "sketch stays bounded");
        let (values, approximate) = top.top(3);
        assert!(approximate, "eviction happened, counts are lower bounds");
        assert_eq!(values[0].value, "heavy");
        assert!(
            values[0].count >= 50,
            "heavy hitter survives with most of its count"
        );
    }

    #[test]
    fn hyperloglog_estimates_within_tolerance() {
        let mut hll = HyperLogLog::new();
        let n = 50_000u64;
        for i in 0..n {
            hll.add_hash(hash_value(&format!("value-{i}")));
        }
        let estimate = hll.estimate();
        let error = (estimate - n as f64).abs() / n as f64;
        assert!(
            error < 0.05,
            "estimate {estimate} off by {:.1}%",
            error * 100.0
        );
    }

    #[test]
    fn profiling_is_cancellable() {
        let d = doc_from("v\n1\n2\n3");
        let registry = JobRegistry::default();
        let ctx = registry.begin("profile", Some(1), |_| {});
        registry.cancel(ctx.id);
        let result = profile_column(&d, 0, ProfileScope::All, &ProfileOptions::default(), &ctx);
        assert!(matches!(result, Err(crate::error::AppError::Cancelled)));
    }

    #[test]
    fn cache_validity_is_per_column_and_scope() {
        let mut d = doc_from("a,b\n1,x\n2,y");
        let (_r, ctx) = ctx();
        let profile_b =
            profile_column(&d, 1, ProfileScope::All, &ProfileOptions::default(), &ctx).unwrap();
        let visible_b = profile_column(
            &d,
            1,
            ProfileScope::VisibleRows,
            &ProfileOptions::default(),
            &ctx,
        )
        .unwrap();

        // Editing column A leaves column B's all-rows profile valid.
        d.set_cell(0, 0, "99".into()).unwrap();
        assert!(profile_is_valid(&d, &profile_b));

        // A filter change invalidates only the visible-rows profile.
        d.set_filter(vec![0]).unwrap();
        assert!(profile_is_valid(&d, &profile_b));
        assert!(!profile_is_valid(&d, &visible_b));

        // Editing column B invalidates its profiles.
        d.set_cell(0, 1, "z".into()).unwrap();
        assert!(!profile_is_valid(&d, &profile_b));
    }

    #[test]
    fn cache_stores_and_prunes() {
        let d = doc_from("a\n1");
        let (_r, ctx) = ctx();
        let p = profile_column(&d, 0, ProfileScope::All, &ProfileOptions::default(), &ctx).unwrap();
        let cache = ProfileCache::default();
        cache
            .share()
            .lock()
            .unwrap()
            .insert((d.id, 0, ProfileScope::All), p);
        assert!(cache.get_valid(&d, 0, ProfileScope::All).is_some());
        assert!(cache.get_valid(&d, 0, ProfileScope::VisibleRows).is_none());
        cache.remove_doc(d.id);
        assert!(cache.get_valid(&d, 0, ProfileScope::All).is_none());
    }
}
