//! Duplicate finder and deduplication (F07): group rows by a normalized
//! multi-column key, report the groups, and (separately, revision-guarded)
//! remove the non-kept rows as ONE undoable operation.
//!
//! Fuzzy matching is deliberately out of scope for this release: keys are
//! compared exactly after the chosen normalizations.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;
use crate::job::JobCtx;

/// How many representative groups a report carries, and rows shown per group.
const GROUP_SAMPLE_LIMIT: usize = 50;
const ROWS_PER_GROUP_LIMIT: usize = 8;
const ROW_CHUNK: usize = 4096;

/// Key definition + normalization options for duplicate detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupSpec {
    pub key_columns: Vec<usize>,
    /// Trim key values before comparing.
    #[serde(default)]
    pub trim: bool,
    /// Compare keys case-insensitively.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Collapse repeated whitespace inside key values before comparing.
    #[serde(default)]
    pub collapse_whitespace: bool,
    /// Whether rows whose COMPLETE key is blank group with each other.
    /// When false, blank-key rows are each treated as unique.
    #[serde(default = "default_true")]
    pub blank_keys_equal: bool,
    /// Drop rows whose complete key is blank from consideration entirely.
    #[serde(default)]
    pub exclude_blank_keys: bool,
}

fn default_true() -> bool {
    true
}

/// Which row survives in each duplicate group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DuplicateKeepStrategy {
    First,
    Last,
    /// The row with the most non-blank cells; ties resolve to the earliest.
    MostComplete,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    /// Normalized key values, for display.
    pub key: Vec<String>,
    /// Absolute row indices, in source order (possibly truncated).
    pub rows: Vec<usize>,
    /// Exact size of the group (rows may be truncated).
    pub size: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateReport {
    /// Document revision this report was computed against.
    pub revision: u64,
    /// Rows considered (the scope, minus excluded blank keys).
    pub considered_rows: usize,
    /// Groups containing more than one row.
    pub group_count: usize,
    /// Excess rows: what "remove duplicates" would delete (one kept per group).
    pub duplicate_rows: usize,
    /// Rows that would remain in the scope after removal.
    pub remaining_rows: usize,
    /// First representative groups.
    pub sample_groups: Vec<DuplicateGroup>,
}

/// Internal result of grouping: every duplicate group, in first-seen order.
struct Grouped {
    groups: Vec<(Vec<String>, Vec<usize>)>,
    considered: usize,
}

fn normalize(spec: &DedupSpec, value: &str) -> String {
    let mut v: String = if spec.collapse_whitespace {
        value.split_whitespace().collect::<Vec<_>>().join(" ")
    } else if spec.trim {
        value.trim().to_string()
    } else {
        value.to_string()
    };
    if spec.trim && spec.collapse_whitespace {
        // collapse already trims edges via split_whitespace
    } else if spec.trim {
        v = v.trim().to_string();
    }
    if spec.case_insensitive {
        v = v.to_lowercase();
    }
    v
}

fn group_rows(
    doc: &Document,
    spec: &DedupSpec,
    scope: &ExportScope,
    ctx: Option<&JobCtx>,
) -> AppResult<Grouped> {
    if spec.key_columns.is_empty() {
        return Err(AppError::invalid("pick at least one key column"));
    }
    if spec.key_columns.iter().any(|&c| c >= doc.n_cols()) {
        return Err(AppError::invalid("key column out of range"));
    }

    let resolved = resolve_scope(doc, scope)?;
    if let Some(ctx) = ctx {
        ctx.set_total(resolved.rows.len() as u64);
    }

    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, (Vec<String>, Vec<usize>)> = HashMap::new();
    let mut considered = 0usize;
    let mut unique_blanks = 0usize;

    let mut i = 0usize;
    doc.visit_rows_at(&resolved.rows, &mut |r, row| {
        if let Some(ctx) = ctx {
            if i.is_multiple_of(ROW_CHUNK) {
                ctx.advance(if i == 0 { 0 } else { ROW_CHUNK as u64 })?;
            }
        }
        i += 1;
        let parts: Vec<String> = spec
            .key_columns
            .iter()
            .map(|&c| normalize(spec, &row[c]))
            .collect();
        let blank = parts.iter().all(|p| p.trim().is_empty());
        if blank && spec.exclude_blank_keys {
            return Ok(true);
        }
        considered += 1;
        if blank && !spec.blank_keys_equal {
            // Blank keys never group: give each its own synthetic identity.
            unique_blanks += 1;
            let key = format!("\u{0}blank-{unique_blanks}");
            map.entry(key.clone()).or_insert_with(|| {
                order.push(key);
                (parts.clone(), Vec::new())
            });
            // Deliberately no push: a lone member can never form a group.
            return Ok(true);
        }
        let key = parts.join("\u{1f}");
        map.entry(key.clone())
            .or_insert_with(|| {
                order.push(key);
                (parts, Vec::new())
            })
            .1
            .push(r);
        Ok(true)
    })?;
    if let Some(ctx) = ctx {
        ctx.flush_progress();
    }

    let mut groups = Vec::new();
    for key in order {
        if let Some((parts, members)) = map.remove(&key) {
            if members.len() > 1 {
                groups.push((parts, members));
            }
        }
    }
    Ok(Grouped { groups, considered })
}

/// Scan for duplicates and build the report. Read-only.
pub fn find_duplicates(
    doc: &Document,
    spec: &DedupSpec,
    scope: &ExportScope,
    ctx: Option<&JobCtx>,
) -> AppResult<DuplicateReport> {
    let grouped = group_rows(doc, spec, scope, ctx)?;
    let duplicate_rows: usize = grouped.groups.iter().map(|(_, rows)| rows.len() - 1).sum();
    Ok(DuplicateReport {
        revision: doc.revision(),
        considered_rows: grouped.considered,
        group_count: grouped.groups.len(),
        duplicate_rows,
        remaining_rows: grouped.considered - duplicate_rows,
        sample_groups: grouped
            .groups
            .iter()
            .take(GROUP_SAMPLE_LIMIT)
            .map(|(key, rows)| DuplicateGroup {
                key: key.clone(),
                size: rows.len(),
                rows: rows.iter().copied().take(ROWS_PER_GROUP_LIMIT).collect(),
            })
            .collect(),
    })
}

/// Absolute indices of every row belonging to a duplicate group (for the
/// "filter to duplicates" and "export duplicates" actions).
pub fn duplicate_row_indices(
    doc: &Document,
    spec: &DedupSpec,
    scope: &ExportScope,
) -> AppResult<Vec<usize>> {
    let grouped = group_rows(doc, spec, scope, None)?;
    let mut rows: Vec<usize> = grouped.groups.into_iter().flat_map(|(_, r)| r).collect();
    rows.sort_unstable();
    Ok(rows)
}

/// The rows "remove duplicates" would delete under a keep strategy: everything
/// in each group except the keeper. Deterministic; sorted ascending.
pub fn removal_rows(
    doc: &Document,
    spec: &DedupSpec,
    scope: &ExportScope,
    keep: DuplicateKeepStrategy,
    ctx: Option<&JobCtx>,
) -> AppResult<Vec<usize>> {
    let grouped = group_rows(doc, spec, scope, ctx)?;
    let mut removals = Vec::new();
    for (_, members) in grouped.groups {
        let keeper = match keep {
            DuplicateKeepStrategy::First => members[0],
            DuplicateKeepStrategy::Last => members[members.len() - 1],
            DuplicateKeepStrategy::MostComplete => {
                // Most non-blank cells wins; ties resolve to the EARLIEST row
                // (members are in source order, and max_by_key returns the
                // last maximum, so compare on (count, reverse-position)).
                let group_rows = doc.fetch_rows(&members)?;
                members
                    .iter()
                    .zip(&group_rows)
                    .max_by_key(|(&r, row)| {
                        let non_blank = row.iter().filter(|c| !c.trim().is_empty()).count();
                        (non_blank, std::cmp::Reverse(r))
                    })
                    .map(|(&r, _)| r)
                    .expect("groups have members")
            }
        };
        removals.extend(members.into_iter().filter(|&r| r != keeper));
    }
    removals.sort_unstable();
    Ok(removals)
}

/// Last completed duplicate report per document, managed by Tauri.
#[derive(Default)]
pub struct DedupCache(Arc<Mutex<HashMap<u64, DuplicateReport>>>);

impl DedupCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, DuplicateReport>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<DuplicateReport> {
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
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn spec(cols: Vec<usize>) -> DedupSpec {
        DedupSpec {
            key_columns: cols,
            trim: false,
            case_insensitive: false,
            collapse_whitespace: false,
            blank_keys_equal: true,
            exclude_blank_keys: false,
        }
    }

    #[test]
    fn composite_keys_group_correctly() {
        let d = doc_from("a,b,v\nx,1,p\nx,2,q\nx,1,r\ny,1,s\nx,1,t");
        let report = find_duplicates(&d, &spec(vec![0, 1]), &ExportScope::All, None).unwrap();
        assert_eq!(report.group_count, 1, "only (x,1) repeats");
        assert_eq!(report.duplicate_rows, 2, "three members, one kept");
        assert_eq!(report.remaining_rows, 3);
        assert_eq!(report.sample_groups[0].key, vec!["x", "1"]);
        assert_eq!(report.sample_groups[0].rows, vec![0, 2, 4]);
    }

    #[test]
    fn normalization_options_are_deterministic() {
        let d = doc_from("k\nAda\n ada \na  da\na da\nada");
        // Exact comparison: nothing repeats literally.
        let none = find_duplicates(&d, &spec(vec![0]), &ExportScope::All, None).unwrap();
        assert_eq!(none.group_count, 0);

        // Trim + case-insensitive groups "Ada", " ada ", "ada".
        let mut s = spec(vec![0]);
        s.trim = true;
        s.case_insensitive = true;
        let report = find_duplicates(&d, &s, &ExportScope::All, None).unwrap();
        assert_eq!(report.group_count, 1);
        assert_eq!(report.sample_groups[0].size, 3);
        assert_eq!(report.sample_groups[0].rows, vec![0, 1, 4]);

        // Adding collapse additionally unifies "a  da" with "a da".
        s.collapse_whitespace = true;
        let report = find_duplicates(&d, &s, &ExportScope::All, None).unwrap();
        assert_eq!(report.group_count, 2);
        assert_eq!(report.sample_groups[0].size, 3, "ada group unchanged");
        assert_eq!(report.sample_groups[1].size, 2, "'a da' group formed");
        assert_eq!(report.sample_groups[1].rows, vec![2, 3]);

        // Determinism: same input, same result.
        let again = find_duplicates(&d, &s, &ExportScope::All, None).unwrap();
        assert_eq!(report.sample_groups[0].rows, again.sample_groups[0].rows);
        assert_eq!(report.sample_groups[1].rows, again.sample_groups[1].rows);
    }

    #[test]
    fn blank_key_policies_are_respected() {
        let d = doc_from("k,v\n,1\n,2\na,3\na,4");
        // Default: blanks are equal -> two groups (blank, a).
        let default = find_duplicates(&d, &spec(vec![0]), &ExportScope::All, None).unwrap();
        assert_eq!(default.group_count, 2);

        // Blanks not equal: each blank is unique -> only "a" groups.
        let mut s = spec(vec![0]);
        s.blank_keys_equal = false;
        let report = find_duplicates(&d, &s, &ExportScope::All, None).unwrap();
        assert_eq!(report.group_count, 1);
        assert_eq!(report.considered_rows, 4);

        // Excluded: blank rows leave the scope entirely.
        let mut s = spec(vec![0]);
        s.exclude_blank_keys = true;
        let report = find_duplicates(&d, &s, &ExportScope::All, None).unwrap();
        assert_eq!(report.considered_rows, 2);
        assert_eq!(report.group_count, 1);
    }

    #[test]
    fn keep_strategies_pick_deterministically() {
        // Group rows 0,2,3: row0 has 2 non-blank, row2 has 3, row3 has 3.
        let d = doc_from("k,a,b\nx,1,\ny,9,9\nx,1,2\nx,3,4");
        let s = spec(vec![0]);
        let first = removal_rows(
            &d,
            &s,
            &ExportScope::All,
            DuplicateKeepStrategy::First,
            None,
        )
        .unwrap();
        assert_eq!(first, vec![2, 3]);
        let last =
            removal_rows(&d, &s, &ExportScope::All, DuplicateKeepStrategy::Last, None).unwrap();
        assert_eq!(last, vec![0, 2]);
        // MostComplete: rows 2 and 3 tie at 3 non-blank cells; the EARLIEST
        // (row 2) wins the tie.
        let complete = removal_rows(
            &d,
            &s,
            &ExportScope::All,
            DuplicateKeepStrategy::MostComplete,
            None,
        )
        .unwrap();
        assert_eq!(complete, vec![0, 3]);
    }

    #[test]
    fn removal_is_one_undo_and_restores_positions() {
        let mut d = doc_from("k,v\na,1\nb,2\na,3\nc,4\na,5");
        let removals = removal_rows(
            &d,
            &spec(vec![0]),
            &ExportScope::All,
            DuplicateKeepStrategy::First,
            None,
        )
        .unwrap();
        assert_eq!(removals, vec![2, 4]);
        d.delete_rows(removals).unwrap();
        assert_eq!(d.n_rows(), 3);
        assert_eq!(d.rows()[1][1], "2");

        d.undo().unwrap();
        assert_eq!(d.n_rows(), 5);
        assert_eq!(d.rows()[2][1], "3", "restored at its original position");
        assert_eq!(d.rows()[4][1], "5", "restored at its original position");
    }

    #[test]
    fn visible_scope_dedupes_only_visible_rows() {
        let mut d = doc_from("k\na\na\na");
        d.set_filter(vec![0, 2]); // middle "a" hidden
        let report = find_duplicates(&d, &spec(vec![0]), &ExportScope::VisibleRows, None).unwrap();
        assert_eq!(report.considered_rows, 2);
        assert_eq!(report.sample_groups[0].rows, vec![0, 2]);
        let removals = removal_rows(
            &d,
            &spec(vec![0]),
            &ExportScope::VisibleRows,
            DuplicateKeepStrategy::First,
            None,
        )
        .unwrap();
        assert_eq!(removals, vec![2], "hidden row 1 is never removed");
    }

    #[test]
    fn scan_is_cancellable() {
        let d = doc_from("k\na\nb");
        let registry = crate::job::JobRegistry::default();
        let ctx = registry.begin("dedup", Some(1), |_| {});
        registry.cancel(ctx.id);
        let result = find_duplicates(&d, &spec(vec![0]), &ExportScope::All, Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
    }

    #[test]
    fn cache_stores_and_prunes() {
        let d = doc_from("k\na\na");
        let report = find_duplicates(&d, &spec(vec![0]), &ExportScope::All, None).unwrap();
        let cache = DedupCache::default();
        cache.share().lock().unwrap().insert(1, report);
        assert!(cache.get(1).is_some());
        cache.remove(1);
        assert!(cache.get(1).is_none());
    }
}
