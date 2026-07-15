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

// ----- classification ----------------------------------------------------------------

/// Run the comparison. Read-only; progress and cancellation via `ctx`.
pub fn compare(
    left: &Document,
    right: &Document,
    spec: &CompareSpec,
    ctx: &JobCtx,
) -> AppResult<CompareResult> {
    let mapping = validate(spec, left, right)?;
    let left_rows = left.rows();
    let right_rows = right.rows();
    ctx.set_total((left_rows.len() + right_rows.len()) as u64);

    let mut entries: Vec<Entry> = Vec::new();
    let row_changed = |l: usize, r: usize| -> bool {
        mapping
            .iter()
            .any(|&(lc, rc)| !cells_equal(spec, &left_rows[l][lc], &right_rows[r][rc]))
    };

    match spec.mode {
        CompareMode::Positional => {
            let max = left_rows.len().max(right_rows.len());
            for i in 0..max {
                if i % ROW_CHUNK == 0 {
                    ctx.advance(if i == 0 { 0 } else { ROW_CHUNK as u64 })?;
                }
                let entry = match (i < left_rows.len(), i < right_rows.len()) {
                    (true, true) => Entry {
                        status: if row_changed(i, i) {
                            DiffStatus::Changed
                        } else {
                            DiffStatus::Unchanged
                        },
                        left_row: Some(i as u32),
                        right_row: Some(i as u32),
                    },
                    (true, false) => Entry {
                        status: DiffStatus::Removed,
                        left_row: Some(i as u32),
                        right_row: None,
                    },
                    (false, true) => Entry {
                        status: DiffStatus::Added,
                        left_row: None,
                        right_row: Some(i as u32),
                    },
                    (false, false) => unreachable!(),
                };
                entries.push(entry);
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
            ctx.set_total((2 * left_rows.len() + right_rows.len()) as u64);

            let mut right_index: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
            for (r, row) in right_rows.iter().enumerate() {
                if r % ROW_CHUNK == 0 {
                    ctx.advance(if r == 0 { 0 } else { ROW_CHUNK as u64 })?;
                }
                right_index
                    .entry(key_of(spec, row, &right_key_cols))
                    .or_default()
                    .push(r);
            }

            // Count left keys up front so EVERY row of a duplicated key is a
            // conflict — including the first one, which must not be silently
            // paired.
            let mut left_counts: HashMap<Vec<String>, usize> = HashMap::new();
            for (l, row) in left_rows.iter().enumerate() {
                if l % ROW_CHUNK == 0 {
                    ctx.advance(if l == 0 { 0 } else { ROW_CHUNK as u64 })?;
                }
                *left_counts
                    .entry(key_of(spec, row, &spec.key_columns))
                    .or_insert(0) += 1;
            }

            let mut right_matched: Vec<bool> = vec![false; right_rows.len()];
            for (l, row) in left_rows.iter().enumerate() {
                if l % ROW_CHUNK == 0 {
                    ctx.advance(if l == 0 { 0 } else { ROW_CHUNK as u64 })?;
                }
                let key = key_of(spec, row, &spec.key_columns);
                let left_dup = left_counts.get(&key).copied().unwrap_or(0) > 1;
                match right_index.get(&key) {
                    Some(matches) => {
                        for &r in matches {
                            right_matched[r] = true;
                        }
                        if left_dup || matches.len() > 1 {
                            // Duplicate keys on either side: surface as a
                            // conflict rather than silently pairing rows.
                            entries.push(Entry {
                                status: DiffStatus::Conflict,
                                left_row: Some(l as u32),
                                right_row: matches.first().map(|&r| r as u32),
                            });
                        } else {
                            let r = matches[0];
                            entries.push(Entry {
                                status: if row_changed(l, r) {
                                    DiffStatus::Changed
                                } else {
                                    DiffStatus::Unchanged
                                },
                                left_row: Some(l as u32),
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
                            left_row: Some(l as u32),
                            right_row: None,
                        });
                    }
                }
            }
            // Unmatched right rows: added — unless their key is duplicated on
            // the right, which is a conflict.
            for (r, row) in right_rows.iter().enumerate() {
                if right_matched[r] {
                    continue;
                }
                let key = key_of(spec, row, &right_key_cols);
                let dup_on_right = right_index.get(&key).is_some_and(|v| v.len() > 1);
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
    let left_rows = left.rows();
    let right_rows = right.rows();

    let records = result
        .entries
        .iter()
        .filter(|e| matches_filter(e))
        .skip(offset)
        .take(count)
        .map(|e| {
            let key = match (result.spec.mode, e.left_row, e.right_row) {
                (CompareMode::Keyed, Some(l), _) => key_of(
                    &result.spec,
                    &left_rows[l as usize],
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
                    key_of(&result.spec, &right_rows[r as usize], &right_key_cols)
                }
                (CompareMode::Positional, l, r) => {
                    vec![format!("row {}", l.or(r).map(|i| i + 1).unwrap_or(0))]
                }
                _ => Vec::new(),
            };
            let cells = match (e.status, e.left_row, e.right_row) {
                (DiffStatus::Changed, Some(l), Some(r)) => mapping
                    .iter()
                    .filter(|&&(lc, rc)| {
                        !cells_equal(
                            &result.spec,
                            &left_rows[l as usize][lc],
                            &right_rows[r as usize][rc],
                        )
                    })
                    .map(|&(lc, rc)| CellDifference {
                        left_col: lc,
                        right_col: rc,
                        left: left_rows[l as usize][lc].clone(),
                        right: right_rows[r as usize][rc].clone(),
                    })
                    .collect(),
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
