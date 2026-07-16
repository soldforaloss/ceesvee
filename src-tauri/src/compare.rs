//! CSV compare and diff (F09): classify the rows of two open documents as
//! added / removed / changed / unchanged / conflict, positionally or by a
//! multi-column key, with column mapping and value normalization.
//!
//! Strictly read-only: neither source document is ever mutated, and nothing
//! is merged automatically. Results are tied to BOTH document revisions: the
//! classification is computed once (as a cancellable job) and stored
//! compactly; result pages recompute keys and cell diffs on demand and are
//! rejected once either document moves on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLockReadGuard};

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::state::SharedDocument;

const ROW_CHUNK: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompareMode {
    /// Row 1 vs row 1, row 2 vs row 2, … — for order-meaningful exports.
    Positional,
    /// Rows paired by a multi-column key.
    Keyed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSpec {
    pub mode: CompareMode,
    /// LEFT columns forming the row key (keyed mode).
    #[serde(default)]
    pub key_columns: Vec<usize>,
    /// (left column, right column) pairs compared against each other.
    /// Empty = identity mapping over the shared column range.
    #[serde(default)]
    pub column_mapping: Vec<(usize, usize)>,
    // Normalization options.
    #[serde(default)]
    pub trim: bool,
    #[serde(default)]
    pub case_insensitive: bool,
    /// Blank cells (after trimming) compare equal regardless of spelling.
    #[serde(default)]
    pub blank_equal: bool,
    /// `1` equals `1.0` (both parse as the same finite number).
    #[serde(default)]
    pub numeric_equal: bool,
    /// `03/01/2024` equals `2024-03-01` (both parse as the same date).
    #[serde(default)]
    pub date_equal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffStatus {
    Added,
    Removed,
    Changed,
    Unchanged,
    Conflict,
}

impl DiffStatus {
    pub fn parse(s: &str) -> Option<DiffStatus> {
        Some(match s {
            "added" => DiffStatus::Added,
            "removed" => DiffStatus::Removed,
            "changed" => DiffStatus::Changed,
            "unchanged" => DiffStatus::Unchanged,
            "conflict" => DiffStatus::Conflict,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellDifference {
    pub left_col: usize,
    pub right_col: usize,
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffRecord {
    pub status: DiffStatus,
    pub key: Vec<String>,
    pub left_row: Option<usize>,
    pub right_row: Option<usize>,
    pub cells: Vec<CellDifference>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSummary {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub total: usize,
}

/// One page of hydrated results.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparePage {
    pub records: Vec<DiffRecord>,
    pub total_filtered: usize,
}

/// Header of a stored comparison, echoed to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareInfo {
    pub compare_id: u64,
    pub left_doc: u64,
    pub right_doc: u64,
    pub left_revision: u64,
    pub right_revision: u64,
    pub summary: CompareSummary,
}

/// One classified row pair, stored compactly (keys/cells recomputed per page).
#[derive(Debug, Clone, Copy)]
struct Entry {
    status: DiffStatus,
    left_row: Option<u32>,
    right_row: Option<u32>,
}

/// A completed comparison.
pub struct CompareResult {
    pub spec: CompareSpec,
    pub left_doc: u64,
    pub right_doc: u64,
    pub left_revision: u64,
    pub right_revision: u64,
    summary: CompareSummary,
    entries: Vec<Entry>,
}

impl CompareResult {
    pub fn info(&self, compare_id: u64) -> CompareInfo {
        CompareInfo {
            compare_id,
            left_doc: self.left_doc,
            right_doc: self.right_doc,
            left_revision: self.left_revision,
            right_revision: self.right_revision,
            summary: self.summary,
        }
    }
}

// ----- normalization / equality ---------------------------------------------------

fn normalize(spec: &CompareSpec, value: &str) -> String {
    let mut v = if spec.trim {
        value.trim().to_string()
    } else {
        value.to_string()
    };
    if spec.case_insensitive {
        v = v.to_lowercase();
    }
    v
}

/// Whether two cells compare equal under the spec's normalizations.
fn cells_equal(spec: &CompareSpec, left: &str, right: &str) -> bool {
    if spec.blank_equal && left.trim().is_empty() && right.trim().is_empty() {
        return true;
    }
    if spec.numeric_equal {
        if let (Some(a), Some(b)) = (analyze::as_number(left), analyze::as_number(right)) {
            return a == b;
        }
    }
    if spec.date_equal {
        if let (Some(a), Some(b)) = (analyze::parse_date(left), analyze::parse_date(right)) {
            return a == b;
        }
    }
    normalize(spec, left) == normalize(spec, right)
}

/// The effective (left, right) column pairs to compare.
fn effective_mapping(spec: &CompareSpec, left: &Document, right: &Document) -> Vec<(usize, usize)> {
    if spec.column_mapping.is_empty() {
        let n = left.n_cols().min(right.n_cols());
        (0..n).map(|c| (c, c)).collect()
    } else {
        spec.column_mapping.clone()
    }
}

fn validate(
    spec: &CompareSpec,
    left: &Document,
    right: &Document,
) -> AppResult<Vec<(usize, usize)>> {
    let mapping = effective_mapping(spec, left, right);
    if mapping.is_empty() {
        return Err(AppError::invalid("no columns are mapped for comparison"));
    }
    for &(l, r) in &mapping {
        if l >= left.n_cols() || r >= right.n_cols() {
            return Err(AppError::invalid("column mapping is out of range"));
        }
    }
    if spec.mode == CompareMode::Keyed {
        if spec.key_columns.is_empty() {
            return Err(AppError::invalid("pick at least one key column"));
        }
        for &k in &spec.key_columns {
            if k >= left.n_cols() {
                return Err(AppError::invalid("key column out of range"));
            }
            if !mapping.iter().any(|&(l, _)| l == k) {
                return Err(AppError::invalid(
                    "every key column must be mapped to a right-hand column",
                ));
            }
        }
    }
    Ok(mapping)
}

fn key_of(spec: &CompareSpec, row: &[String], columns: &[usize]) -> Vec<String> {
    columns.iter().map(|&c| normalize(spec, &row[c])).collect()
}

/// Canonical form of one key part under EVERY enabled equivalence, so keyed
/// matching pairs exactly the values [`cells_equal`] would call equal
/// (`1`/`1.0` under numeric-equal, `03/01/2024`/`2024-03-01` under
/// date-equal, any blanks under blanks-equal). The prefixes keep a canonical
/// number/date from colliding with identical literal text. Display keys stay
/// on [`key_of`]; this is for matching only.
fn match_key_part(spec: &CompareSpec, value: &str) -> String {
    if spec.blank_equal && value.trim().is_empty() {
        return String::new();
    }
    if spec.numeric_equal {
        if let Some(n) = analyze::as_number(value) {
            return format!("\u{1}n{n}");
        }
    }
    if spec.date_equal {
        if let Some(d) = analyze::parse_date(value) {
            return format!("\u{1}d{}", d.format("%Y-%m-%dT%H:%M:%S"));
        }
    }
    normalize(spec, value)
}

/// The matching key for a row (see [`match_key_part`]).
fn match_key_of(spec: &CompareSpec, row: &[String], columns: &[usize]) -> Vec<String> {
    columns
        .iter()
        .map(|&c| match_key_part(spec, &row[c]))
        .collect()
}

// ----- classification ----------------------------------------------------------------

/// Run the comparison. Read-only; progress and cancellation via `ctx`.
pub fn compare(
    left: &Document,
    right: &Document,
    spec: &CompareSpec,
    ctx: &JobCtx,
) -> AppResult<CompareResult> {
    let mapping = validate(spec, left, right)?;
    let n_left = left.n_rows();
    let n_right = right.n_rows();
    ctx.set_total((n_left + n_right) as u64);

    let mut entries: Vec<Entry> = Vec::new();
    // Compare a fetched pair of rows under the column mapping.
    let pair_changed = |lrow: &[String], rrow: &[String]| -> bool {
        mapping
            .iter()
            .any(|&(lc, rc)| !cells_equal(spec, &lrow[lc], &rrow[rc]))
    };

    match spec.mode {
        CompareMode::Positional => {
            // Walk both documents in lockstep blocks so indexed backings read
            // sequentially instead of row-by-row.
            let max = n_left.max(n_right);
            let mut i = 0usize;
            while i < max {
                let end = (i + ROW_CHUNK).min(max);
                let l_end = end.min(n_left);
                let r_end = end.min(n_right);
                let lblock: Vec<Vec<String>> = if i < l_end {
                    left.fetch_rows(&(i..l_end).collect::<Vec<_>>())?
                } else {
                    Vec::new()
                };
                let rblock: Vec<Vec<String>> = if i < r_end {
                    right.fetch_rows(&(i..r_end).collect::<Vec<_>>())?
                } else {
                    Vec::new()
                };
                for k in i..end {
                    let lrow = (k < n_left).then(|| &lblock[k - i]);
                    let rrow = (k < n_right).then(|| &rblock[k - i]);
                    let entry = match (lrow, rrow) {
                        (Some(lrow), Some(rrow)) => Entry {
                            status: if pair_changed(lrow, rrow) {
                                DiffStatus::Changed
                            } else {
                                DiffStatus::Unchanged
                            },
                            left_row: Some(k as u32),
                            right_row: Some(k as u32),
                        },
                        (Some(_), None) => Entry {
                            status: DiffStatus::Removed,
                            left_row: Some(k as u32),
                            right_row: None,
                        },
                        (None, Some(_)) => Entry {
                            status: DiffStatus::Added,
                            left_row: None,
                            right_row: Some(k as u32),
                        },
                        (None, None) => unreachable!(),
                    };
                    entries.push(entry);
                }
                ctx.advance((end - i) as u64)?;
                i = end;
            }
        }
        CompareMode::Keyed => {
            // The right-hand key columns come from the mapping.
            let right_key_cols: Vec<usize> = spec
                .key_columns
                .iter()
                .map(|&k| {
                    mapping
                        .iter()
                        .find(|&&(l, _)| l == k)
                        .map(|&(_, r)| r)
                        .expect("validated: key columns are mapped")
                })
                .collect();
            ctx.set_total((2 * n_left + n_right) as u64);

            let mut right_index: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
            right.visit_rows(0..n_right, &mut |r, row| {
                if r.is_multiple_of(ROW_CHUNK) {
                    ctx.advance(if r == 0 { 0 } else { ROW_CHUNK as u64 })?;
                }
                right_index
                    .entry(match_key_of(spec, row, &right_key_cols))
                    .or_default()
                    .push(r);
                Ok(true)
            })?;

            // Count left keys up front so EVERY row of a duplicated key is a
            // conflict — including the first one, which must not be silently
            // paired.
            let mut left_counts: HashMap<Vec<String>, usize> = HashMap::new();
            left.visit_rows(0..n_left, &mut |l, row| {
                if l.is_multiple_of(ROW_CHUNK) {
                    ctx.advance(if l == 0 { 0 } else { ROW_CHUNK as u64 })?;
                }
                *left_counts
                    .entry(match_key_of(spec, row, &spec.key_columns))
                    .or_insert(0) += 1;
                Ok(true)
            })?;

            // Classify left rows block-by-block. Unique pairs defer their
            // cell comparison until the block's right rows are fetched in one
            // batched read.
            let mut right_matched: Vec<bool> = vec![false; n_right];
            let mut l = 0usize;
            while l < n_left {
                let end = (l + ROW_CHUNK).min(n_left);
                let lblock = left.fetch_rows(&(l..end).collect::<Vec<_>>())?;
                // (entry index, left row within block, right row) to hydrate.
                let mut pending: Vec<(usize, usize, usize)> = Vec::new();
                for (bi, lrow) in lblock.iter().enumerate() {
                    let li = l + bi;
                    let key = match_key_of(spec, lrow, &spec.key_columns);
                    let left_dup = left_counts.get(&key).copied().unwrap_or(0) > 1;
                    match right_index.get(&key) {
                        Some(matches) => {
                            if left_dup || matches.len() > 1 {
                                // Duplicate keys on either side: surface as a
                                // conflict rather than silently pairing rows.
                                // Only the referenced right row is consumed;
                                // the REMAINING right duplicates fall through
                                // to the unmatched pass below so every record
                                // is classified (as its own conflict).
                                if let Some(&first) = matches.first() {
                                    right_matched[first] = true;
                                }
                                entries.push(Entry {
                                    status: DiffStatus::Conflict,
                                    left_row: Some(li as u32),
                                    right_row: matches.first().map(|&r| r as u32),
                                });
                            } else {
                                let r = matches[0];
                                right_matched[r] = true;
                                pending.push((entries.len(), bi, r));
                                entries.push(Entry {
                                    status: DiffStatus::Unchanged, // provisional
                                    left_row: Some(li as u32),
                                    right_row: Some(r as u32),
                                });
                            }
                        }
                        None => {
                            entries.push(Entry {
                                status: if left_dup {
                                    DiffStatus::Conflict
                                } else {
                                    DiffStatus::Removed
                                },
                                left_row: Some(li as u32),
                                right_row: None,
                            });
                        }
                    }
                }
                let wanted: Vec<usize> = pending.iter().map(|&(_, _, r)| r).collect();
                let rrows = right.fetch_rows(&wanted)?;
                for (&(entry_idx, bi, _), rrow) in pending.iter().zip(&rrows) {
                    if pair_changed(&lblock[bi], rrow) {
                        entries[entry_idx].status = DiffStatus::Changed;
                    }
                }
                ctx.advance((end - l) as u64)?;
                l = end;
            }

            // Unmatched right rows: added — unless their key is duplicated on
            // the right, which is a conflict. Derived from the key index (no
            // second scan of the right document).
            let mut unmatched: Vec<(usize, bool)> = Vec::new();
            for indices in right_index.values() {
                let dup_on_right = indices.len() > 1;
                for &r in indices {
                    if !right_matched[r] {
                        unmatched.push((r, dup_on_right));
                    }
                }
            }
            unmatched.sort_unstable();
            for (r, dup_on_right) in unmatched {
                entries.push(Entry {
                    status: if dup_on_right {
                        DiffStatus::Conflict
                    } else {
                        DiffStatus::Added
                    },
                    left_row: None,
                    right_row: Some(r as u32),
                });
            }
        }
    }
    ctx.flush_progress();

    let mut summary = CompareSummary {
        added: 0,
        removed: 0,
        changed: 0,
        unchanged: 0,
        conflicts: 0,
        total: entries.len(),
    };
    for e in &entries {
        match e.status {
            DiffStatus::Added => summary.added += 1,
            DiffStatus::Removed => summary.removed += 1,
            DiffStatus::Changed => summary.changed += 1,
            DiffStatus::Unchanged => summary.unchanged += 1,
            DiffStatus::Conflict => summary.conflicts += 1,
        }
    }

    Ok(CompareResult {
        spec: spec.clone(),
        left_doc: left.id,
        right_doc: right.id,
        left_revision: left.revision(),
        right_revision: right.revision(),
        summary,
        entries,
    })
}

// ----- page materialization ------------------------------------------------------------

/// Hydrate one page of results (keys + cell-level differences) against the
/// live documents. Errors when either document moved past the compared
/// revision — stale results are never served.
pub fn results_page(
    result: &CompareResult,
    left: &Document,
    right: &Document,
    offset: usize,
    count: usize,
    statuses: Option<&[DiffStatus]>,
) -> AppResult<(Vec<DiffRecord>, usize)> {
    left.check_revision(result.left_revision)?;
    right.check_revision(result.right_revision)?;

    let matches_filter = |e: &Entry| statuses.is_none_or(|s| s.contains(&e.status));
    let total_filtered = result.entries.iter().filter(|e| matches_filter(e)).count();

    let mapping = effective_mapping(&result.spec, left, right);
    let page: Vec<&Entry> = result
        .entries
        .iter()
        .filter(|e| matches_filter(e))
        .skip(offset)
        .take(count)
        .collect();

    // Hydrate every row this page touches in two batched reads (one per side).
    let fetch_side = |doc: &Document, pick: fn(&Entry) -> Option<u32>| {
        let mut need: Vec<usize> = page
            .iter()
            .filter_map(|e| pick(e))
            .map(|v| v as usize)
            .collect();
        need.sort_unstable();
        need.dedup();
        let rows = doc.fetch_rows(&need)?;
        Ok::<HashMap<usize, Vec<String>>, AppError>(need.into_iter().zip(rows).collect())
    };
    let left_rows = fetch_side(left, |e| e.left_row)?;
    let right_rows = fetch_side(right, |e| e.right_row)?;

    let records = page
        .into_iter()
        .map(|e| {
            let key = match (result.spec.mode, e.left_row, e.right_row) {
                (CompareMode::Keyed, Some(l), _) => key_of(
                    &result.spec,
                    &left_rows[&(l as usize)],
                    &result.spec.key_columns,
                ),
                (CompareMode::Keyed, None, Some(r)) => {
                    let right_key_cols: Vec<usize> = result
                        .spec
                        .key_columns
                        .iter()
                        .filter_map(|&k| {
                            mapping.iter().find(|&&(lc, _)| lc == k).map(|&(_, rc)| rc)
                        })
                        .collect();
                    key_of(&result.spec, &right_rows[&(r as usize)], &right_key_cols)
                }
                (CompareMode::Positional, l, r) => {
                    vec![format!("row {}", l.or(r).map(|i| i + 1).unwrap_or(0))]
                }
                _ => Vec::new(),
            };
            let cells = match (e.status, e.left_row, e.right_row) {
                (DiffStatus::Changed, Some(l), Some(r)) => {
                    let lrow = &left_rows[&(l as usize)];
                    let rrow = &right_rows[&(r as usize)];
                    mapping
                        .iter()
                        .filter(|&&(lc, rc)| !cells_equal(&result.spec, &lrow[lc], &rrow[rc]))
                        .map(|&(lc, rc)| CellDifference {
                            left_col: lc,
                            right_col: rc,
                            left: lrow[lc].clone(),
                            right: rrow[rc].clone(),
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            DiffRecord {
                status: e.status,
                key,
                left_row: e.left_row.map(|v| v as usize),
                right_row: e.right_row.map(|v| v as usize),
                cells,
            }
        })
        .collect();

    Ok((records, total_filtered))
}

/// Row indices (on the relevant side) for an export of one status class.
pub fn rows_for_status(result: &CompareResult, status: DiffStatus) -> Vec<usize> {
    result
        .entries
        .iter()
        .filter(|e| e.status == status)
        .filter_map(|e| match status {
            DiffStatus::Added => e.right_row,
            _ => e.left_row,
        })
        .map(|v| v as usize)
        .collect()
}

// ----- lock ordering / cache -------------------------------------------------------------

/// Acquire read locks on two documents in a globally consistent order (lower
/// document id first) so concurrent comparisons can never deadlock.
pub fn read_both<'a>(
    left_handle: &'a SharedDocument,
    right_handle: &'a SharedDocument,
    left_id: u64,
    right_id: u64,
) -> AppResult<(RwLockReadGuard<'a, Document>, RwLockReadGuard<'a, Document>)> {
    let poisoned = |_: std::sync::PoisonError<RwLockReadGuard<'a, Document>>| {
        AppError::Other("internal document lock error".into())
    };
    if left_id <= right_id {
        let l = left_handle.read().map_err(poisoned)?;
        let r = right_handle.read().map_err(poisoned)?;
        Ok((l, r))
    } else {
        let r = right_handle.read().map_err(poisoned)?;
        let l = left_handle.read().map_err(poisoned)?;
        Ok((l, r))
    }
}

/// Completed comparisons, keyed by compare id (= the job id that produced
/// them). Pruned when either referenced document closes.
#[derive(Default)]
pub struct CompareCache(Arc<Mutex<HashMap<u64, CompareResult>>>);

impl CompareCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, CompareResult>>> {
        Arc::clone(&self.0)
    }

    pub fn with<T>(&self, compare_id: u64, f: impl FnOnce(&CompareResult) -> T) -> Option<T> {
        let map = self.0.lock().ok()?;
        map.get(&compare_id).map(f)
    }

    pub fn remove_doc(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.retain(|_, r| r.left_doc != doc_id && r.right_doc != doc_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};

    fn doc(id: u64, csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(id, None, parsed, true)
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("compare", None, |_| {});
        (registry, ctx)
    }

    fn keyed_spec(keys: Vec<usize>) -> CompareSpec {
        CompareSpec {
            mode: CompareMode::Keyed,
            key_columns: keys,
            column_mapping: Vec::new(),
            trim: false,
            case_insensitive: false,
            blank_equal: false,
            numeric_equal: false,
            date_equal: false,
        }
    }

    fn statuses(result: &CompareResult) -> Vec<DiffStatus> {
        result.entries.iter().map(|e| e.status).collect()
    }

    #[test]
    fn positional_classifies_all_four_states() {
        let left = doc(1, "a,b\n1,x\n2,y\n3,z");
        let right = doc(2, "a,b\n1,x\n2,CHANGED\n3,z\n4,new");
        let (_r, ctx) = ctx();
        let spec = CompareSpec {
            mode: CompareMode::Positional,
            ..keyed_spec(vec![])
        };
        let result = compare(&left, &right, &spec, &ctx).unwrap();
        assert_eq!(
            statuses(&result),
            vec![
                DiffStatus::Unchanged,
                DiffStatus::Changed,
                DiffStatus::Unchanged,
                DiffStatus::Added,
            ]
        );
        assert_eq!(result.summary.added, 1);
        assert_eq!(result.summary.changed, 1);

        // And a shrinking right side yields removed.
        let shorter = doc(3, "a,b\n1,x");
        let result = compare(&left, &shorter, &spec, &ctx).unwrap();
        assert_eq!(result.summary.removed, 2);
    }

    #[test]
    fn keyed_composite_classification() {
        let left = doc(1, "id,region,v\n1,e,10\n2,w,20\n3,e,30");
        let right = doc(2, "id,region,v\n1,e,10\n3,e,31\n4,w,40");
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &keyed_spec(vec![0, 1]), &ctx).unwrap();
        assert_eq!(
            statuses(&result),
            vec![
                DiffStatus::Unchanged, // (1,e)
                DiffStatus::Removed,   // (2,w)
                DiffStatus::Changed,   // (3,e): 30 -> 31
                DiffStatus::Added,     // (4,w)
            ]
        );
    }

    #[test]
    fn duplicate_keys_surface_as_conflicts() {
        let left = doc(1, "id,v\n1,a\n1,b\n2,c");
        let right = doc(2, "id,v\n1,z\n2,c\n3,d\n3,e");
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &keyed_spec(vec![0]), &ctx).unwrap();
        let s = statuses(&result);
        // Both left rows with id=1 conflict (duplicate on the left).
        assert_eq!(s[0], DiffStatus::Conflict);
        assert_eq!(s[1], DiffStatus::Conflict);
        assert_eq!(s[2], DiffStatus::Unchanged); // id=2
                                                 // Right-only id=3 appears twice -> conflicts, not silent adds.
        assert_eq!(s[3], DiffStatus::Conflict);
        assert_eq!(s[4], DiffStatus::Conflict);
        assert_eq!(result.summary.conflicts, 4);
    }

    #[test]
    fn every_right_duplicate_of_a_matched_key_is_classified() {
        // One left row, two right rows with the same key: the pair entry
        // consumes only the referenced right row; the OTHER right duplicate
        // must still surface as its own conflict instead of vanishing.
        let left = doc(1, "id,v\n1,a");
        let right = doc(2, "id,v\n1,x\n1,y");
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &keyed_spec(vec![0]), &ctx).unwrap();
        assert_eq!(result.summary.conflicts, 2, "{:?}", statuses(&result));
        assert_eq!(result.summary.total, 2);
        // Both right rows appear across the entries.
        let right_rows: Vec<Option<u32>> = result.entries.iter().map(|e| e.right_row).collect();
        assert!(right_rows.contains(&Some(0)));
        assert!(right_rows.contains(&Some(1)));
    }

    #[test]
    fn keyed_matching_honours_numeric_and_date_equivalence() {
        // With numeric/date equivalence on, `1` and `1.0` (and the two date
        // spellings) must PAIR as the same key, not classify as added+removed.
        let left = doc(1, "id,v\n1,a\n2024-03-01,b");
        let right = doc(2, "id,v\n1.0,a\n03/01/2024,b");
        let mut spec = keyed_spec(vec![0]);
        spec.numeric_equal = true;
        spec.date_equal = true;
        let (_r, equivalent_ctx) = ctx();
        let result = compare(&left, &right, &spec, &equivalent_ctx).unwrap();
        assert_eq!(result.summary.unchanged, 2, "{:?}", statuses(&result));
        assert_eq!(result.summary.added, 0);
        assert_eq!(result.summary.removed, 0);

        // Without the toggles the same rows do NOT pair.
        let (_r2, strict_ctx) = ctx();
        let strict = compare(&left, &right, &keyed_spec(vec![0]), &strict_ctx).unwrap();
        assert_eq!(strict.summary.removed, 2);
        assert_eq!(strict.summary.added, 2);
    }

    #[test]
    fn column_mapping_handles_reordered_columns() {
        let left = doc(1, "id,name\n1,Ada\n2,Bob");
        let right = doc(2, "name,id\nAda,1\nBOB,2");
        let mut spec = keyed_spec(vec![0]);
        spec.column_mapping = vec![(0, 1), (1, 0)]; // id->id, name->name
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &spec, &ctx).unwrap();
        assert_eq!(
            statuses(&result),
            vec![DiffStatus::Unchanged, DiffStatus::Changed]
        );

        // Case-insensitive normalization makes Bob == BOB.
        spec.case_insensitive = true;
        let result = compare(&left, &right, &spec, &ctx).unwrap();
        assert_eq!(result.summary.unchanged, 2);
    }

    #[test]
    fn normalization_equivalences() {
        let left = doc(1, "id,n,d,t\n1,1,2024-03-01, pad \n2,2,2024-01-01,x");
        let right = doc(2, "id,n,d,t\n1,1.0,03/01/2024,pad\n2,2.5,2024-01-01,x");
        let mut spec = keyed_spec(vec![0]);
        spec.numeric_equal = true;
        spec.date_equal = true;
        spec.trim = true;
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &spec, &ctx).unwrap();
        // Row 1: 1 == 1.0, dates equal, " pad " == "pad" after trim.
        // Row 2: 2 != 2.5 -> changed.
        assert_eq!(
            statuses(&result),
            vec![DiffStatus::Unchanged, DiffStatus::Changed]
        );

        // Blank equivalence.
        let left = doc(1, "id,v\n1,");
        let right = doc(2, "id,v\n1,   ");
        let mut spec = keyed_spec(vec![0]);
        spec.blank_equal = true;
        let result = compare(&left, &right, &spec, &ctx).unwrap();
        assert_eq!(result.summary.unchanged, 1);
    }

    #[test]
    fn pages_hydrate_cells_and_respect_filters_and_revisions() {
        let left = doc(1, "id,v,w\n1,a,q\n2,b,q\n3,c,q");
        let mut right = doc(2, "id,v,w\n1,a,q\n2,B!,q\n4,d,q");
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &keyed_spec(vec![0]), &ctx).unwrap();

        let (all, total) = results_page(&result, &left, &right, 0, 10, None).unwrap();
        assert_eq!(total, 4);
        assert_eq!(all.len(), 4);

        // Changed row hydrates exactly the differing cell.
        let changed = all
            .iter()
            .find(|r| r.status == DiffStatus::Changed)
            .unwrap();
        assert_eq!(changed.key, vec!["2"]);
        assert_eq!(changed.cells.len(), 1);
        assert_eq!(changed.cells[0].left, "b");
        assert_eq!(changed.cells[0].right, "B!");

        // Status filter + pagination.
        let (page, filtered_total) = results_page(
            &result,
            &left,
            &right,
            0,
            1,
            Some(&[DiffStatus::Added, DiffStatus::Removed]),
        )
        .unwrap();
        assert_eq!(filtered_total, 2);
        assert_eq!(page.len(), 1);

        // Results are tied to both revisions: editing the right doc kills them.
        right.set_cell(0, 0, "9".into()).unwrap();
        assert!(results_page(&result, &left, &right, 0, 10, None).is_err());
    }

    #[test]
    fn export_row_sets_come_from_the_right_side_for_added() {
        let left = doc(1, "id\n1\n2");
        let right = doc(2, "id\n2\n3");
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &keyed_spec(vec![0]), &ctx).unwrap();
        assert_eq!(
            rows_for_status(&result, DiffStatus::Added),
            vec![1],
            "right row of id=3"
        );
        assert_eq!(
            rows_for_status(&result, DiffStatus::Removed),
            vec![0],
            "left row of id=1"
        );
    }

    #[test]
    fn comparison_never_mutates_sources() {
        let left = doc(1, "id\n1");
        let right = doc(2, "id\n2");
        let (lrev, rrev) = (left.revision(), right.revision());
        let (_r, ctx) = ctx();
        let _ = compare(&left, &right, &keyed_spec(vec![0]), &ctx).unwrap();
        assert_eq!(left.revision(), lrev);
        assert_eq!(right.revision(), rrev);
    }

    #[test]
    fn compare_is_cancellable() {
        let left = doc(1, "id\n1");
        let right = doc(2, "id\n2");
        let registry = JobRegistry::default();
        let ctx = registry.begin("compare", None, |_| {});
        registry.cancel(ctx.id);
        assert!(matches!(
            compare(&left, &right, &keyed_spec(vec![0]), &ctx),
            Err(AppError::Cancelled)
        ));
    }

    #[test]
    fn cache_prunes_by_document() {
        let left = doc(1, "id\n1");
        let right = doc(2, "id\n1");
        let (_r, ctx) = ctx();
        let result = compare(&left, &right, &keyed_spec(vec![0]), &ctx).unwrap();
        let cache = CompareCache::default();
        cache.share().lock().unwrap().insert(42, result);
        assert!(cache.with(42, |_| ()).is_some());
        cache.remove_doc(2);
        assert!(cache.with(42, |_| ()).is_none());
    }
}
