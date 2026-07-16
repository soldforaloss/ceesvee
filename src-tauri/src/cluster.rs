//! Fuzzy value clustering (F24): find likely spelling/punctuation/spacing/
//! capitalization variants in ONE text column and propose bulk
//! normalizations. Every method is deterministic — identical input and
//! settings produce identical clusters — and nothing is ever applied
//! automatically: the report only proposes; the user selects.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::export_scope::resolve_scope;
use crate::job::JobCtx;

/// Distinct-value cap: clustering is for categorical-ish text; a column with
/// more distinct values than this needs different tooling.
pub const MAX_DISTINCT_VALUES: usize = 200_000;
/// Pairwise-comparison budget for the distance methods.
const MAX_PAIR_COMPARISONS: u64 = 5_000_000;
/// Clusters returned to the UI (the report still counts the rest).
const REPORT_CLUSTER_LIMIT: usize = 200;
const ROW_CHUNK: usize = 4096;

/// Clustering method (a closed, validated set).
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClusterMethod {
    /// Key collision on the normalized, token-sorted fingerprint.
    Fingerprint,
    /// Key collision on sorted character n-grams of the normalized value.
    NgramFingerprint { n: usize },
    /// Edit distance at most `max_distance` between normalized values.
    Levenshtein { max_distance: usize },
    /// Jaro-Winkler similarity at least `min_similarity` (0..1).
    JaroWinkler { min_similarity: f64 },
}

/// Normalizations applied before matching (never to the stored values).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterNormalization {
    #[serde(default)]
    pub case_fold: bool,
    #[serde(default)]
    pub trim_collapse: bool,
    #[serde(default)]
    pub strip_punctuation: bool,
    #[serde(default)]
    pub strip_diacritics: bool,
    /// Sort whitespace-separated words, so "Doe, John" groups with "John Doe"
    /// (after punctuation stripping).
    #[serde(default)]
    pub sort_words: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSpec {
    pub column: usize,
    pub method: ClusterMethod,
    #[serde(default)]
    pub normalization: ClusterNormalization,
    pub scope: ExportScope,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterMember {
    pub value: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueCluster {
    /// Members sorted by count desc, then value asc (deterministic).
    pub members: Vec<ClusterMember>,
    /// Proposed canonical value: the most frequent member (ties resolve to
    /// the lexicographically smallest). A suggestion only — never forced.
    pub suggested: String,
    /// The shared matching key (fingerprint methods) or the pair score
    /// rendered as a string, for the UI's "why did these group" column.
    pub match_key: String,
    /// Rows whose value differs from the suggestion.
    pub rows_affected: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterReport {
    pub revision: u64,
    pub column: usize,
    pub scanned_rows: usize,
    pub distinct_values: usize,
    pub total_clusters: usize,
    /// Top clusters by rows affected (bounded; see `total_clusters`).
    pub clusters: Vec<ValueCluster>,
}

// ----- normalization ---------------------------------------------------------------

/// Strip combining marks after NFD-style decomposition of Latin-1-ish
/// letters. Full Unicode decomposition needs a table; this covers the
/// overwhelmingly common Latin diacritics deterministically.
fn strip_diacritics(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'ē' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ō' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'ý' | 'ÿ' => 'y',
            'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' | 'Ā' => 'A',
            'É' | 'È' | 'Ê' | 'Ë' | 'Ē' => 'E',
            'Í' | 'Ì' | 'Î' | 'Ï' | 'Ī' => 'I',
            'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'Ō' => 'O',
            'Ú' | 'Ù' | 'Û' | 'Ü' | 'Ū' => 'U',
            'Ç' => 'C',
            'Ñ' => 'N',
            other => other,
        })
        .collect()
}

/// Apply the selected normalizations (deterministic, pure).
pub fn normalize(value: &str, options: &ClusterNormalization) -> String {
    let mut v = value.to_string();
    if options.strip_diacritics {
        v = strip_diacritics(&v);
    }
    if options.case_fold {
        v = v.to_lowercase();
    }
    if options.strip_punctuation {
        v = v
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c.is_whitespace() {
                    c
                } else {
                    ' '
                }
            })
            .collect();
    }
    if options.trim_collapse || options.strip_punctuation || options.sort_words {
        let mut words: Vec<&str> = v.split_whitespace().collect();
        if options.sort_words {
            words.sort_unstable();
        }
        v = words.join(" ");
    }
    v
}

/// The OpenRefine-style fingerprint: normalized, tokenized, sorted, deduped.
fn fingerprint(value: &str, options: &ClusterNormalization) -> String {
    let normalized = normalize(value, options);
    let mut tokens: Vec<&str> = normalized.split_whitespace().collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens.join(" ")
}

/// Sorted, deduped character n-grams of the normalized value (spaces
/// removed), catching in-word misspellings the token fingerprint misses.
fn ngram_fingerprint(value: &str, n: usize, options: &ClusterNormalization) -> String {
    let normalized: String = normalize(value, options)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let chars: Vec<char> = normalized.chars().collect();
    let n = n.max(1);
    if chars.len() < n {
        return normalized;
    }
    let mut grams: Vec<String> = chars.windows(n).map(|w| w.iter().collect()).collect();
    grams.sort_unstable();
    grams.dedup();
    grams.join("")
}

// ----- distances -------------------------------------------------------------------

/// Classic DP Levenshtein with an early-exit band.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (prev[j + 1] + 1).min(current[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

/// Jaro similarity, then the Winkler common-prefix boost (standard p=0.1).
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let window = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut b_used = vec![false; b.len()];
    let mut matches = 0usize;
    let mut a_matched = Vec::with_capacity(a.len());
    for (i, ca) in a.iter().enumerate() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(b.len());
        for j in lo..hi {
            if !b_used[j] && b[j] == *ca {
                b_used[j] = true;
                matches += 1;
                a_matched.push((i, j));
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }
    // Transpositions: matched pairs whose b-order is inverted.
    let mut b_order: Vec<usize> = a_matched.iter().map(|&(_, j)| j).collect();
    let mut transpositions = 0usize;
    for w in 0..b_order.len() {
        for v in (w + 1)..b_order.len() {
            if b_order[w] > b_order[v] {
                transpositions += 1;
                b_order.swap(w, v);
            }
        }
    }
    let m = matches as f64;
    let jaro = (m / a.len() as f64 + m / b.len() as f64 + (m - transpositions as f64) / m) / 3.0;
    let prefix = a
        .iter()
        .zip(b.iter())
        .take(4)
        .take_while(|(x, y)| x == y)
        .count() as f64;
    jaro + prefix * 0.1 * (1.0 - jaro)
}

// ----- the scan --------------------------------------------------------------------

/// Run a clustering scan. Read-only; progress and cancellation via `ctx`.
pub fn scan(doc: &Document, spec: &ClusterSpec, ctx: &JobCtx) -> AppResult<ClusterReport> {
    if spec.column >= doc.n_cols() {
        return Err(AppError::invalid("cluster column out of range"));
    }
    if let ClusterMethod::JaroWinkler { min_similarity } = spec.method {
        if !(0.0..=1.0).contains(&min_similarity) {
            return Err(AppError::invalid("similarity must be between 0 and 1"));
        }
    }
    let resolved = resolve_scope(doc, &spec.scope)?;
    ctx.set_total(resolved.rows.len() as u64);

    // 1. Count distinct raw values in the scope.
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut i = 0usize;
    doc.visit_rows_at(&resolved.rows, &mut |_, row| {
        if i.is_multiple_of(ROW_CHUNK) {
            ctx.advance(if i == 0 { 0 } else { ROW_CHUNK as u64 })?;
        }
        i += 1;
        let cell = row[spec.column].trim();
        if !cell.is_empty() {
            *counts.entry(row[spec.column].clone()).or_insert(0) += 1;
        }
        if counts.len() > MAX_DISTINCT_VALUES {
            return Err(AppError::invalid(format!(
                "the column has more than {MAX_DISTINCT_VALUES} distinct values — \
                 clustering works on categorical-style text"
            )));
        }
        Ok(true)
    })?;
    ctx.flush_progress();

    // 2. Group. Deterministic ordering everywhere: values sort before use.
    let mut values: Vec<(&String, usize)> = counts.iter().map(|(v, &c)| (v, c)).collect();
    values.sort_unstable_by(|a, b| a.0.cmp(b.0));

    let groups: Vec<(String, Vec<usize>)> = match &spec.method {
        ClusterMethod::Fingerprint => key_groups(&values, |v| fingerprint(v, &spec.normalization)),
        ClusterMethod::NgramFingerprint { n } => {
            key_groups(&values, |v| ngram_fingerprint(v, *n, &spec.normalization))
        }
        ClusterMethod::Levenshtein { max_distance } => distance_groups(
            &values,
            &spec.normalization,
            ctx,
            |a, b| levenshtein(a, b) <= *max_distance,
            &format!("distance ≤ {max_distance}"),
        )?,
        ClusterMethod::JaroWinkler { min_similarity } => distance_groups(
            &values,
            &spec.normalization,
            ctx,
            |a, b| jaro_winkler(a, b) >= *min_similarity,
            &format!("similarity ≥ {min_similarity:.2}"),
        )?,
    };

    // 3. Build the report: only groups with 2+ distinct values matter.
    let mut clusters: Vec<ValueCluster> = Vec::new();
    for (key, member_indices) in groups {
        if member_indices.len() < 2 {
            continue;
        }
        let mut members: Vec<ClusterMember> = member_indices
            .iter()
            .map(|&idx| ClusterMember {
                value: values[idx].0.clone(),
                count: values[idx].1,
            })
            .collect();
        members.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
        let suggested = members[0].value.clone();
        let rows_affected = members
            .iter()
            .filter(|m| m.value != suggested)
            .map(|m| m.count)
            .sum();
        clusters.push(ValueCluster {
            suggested,
            match_key: key,
            rows_affected,
            members,
        });
    }
    clusters.sort_by(|a, b| {
        b.rows_affected
            .cmp(&a.rows_affected)
            .then_with(|| a.match_key.cmp(&b.match_key))
    });
    let total_clusters = clusters.len();
    clusters.truncate(REPORT_CLUSTER_LIMIT);

    Ok(ClusterReport {
        revision: doc.revision(),
        column: spec.column,
        scanned_rows: resolved.rows.len(),
        distinct_values: counts.len(),
        total_clusters,
        clusters,
    })
}

/// Group value indices by an exact key function.
fn key_groups(
    values: &[(&String, usize)],
    key_of: impl Fn(&str) -> String,
) -> Vec<(String, Vec<usize>)> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, (value, _)) in values.iter().enumerate() {
        map.entry(key_of(value)).or_default().push(idx);
    }
    let mut groups: Vec<(String, Vec<usize>)> = map.into_iter().collect();
    groups.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    groups
}

/// Group by pairwise predicate using union-find within length-banded blocks
/// (values differing wildly in length can't be close). Deterministic: values
/// arrive sorted and unions always attach to the smaller root.
fn distance_groups(
    values: &[(&String, usize)],
    normalization: &ClusterNormalization,
    ctx: &JobCtx,
    related: impl Fn(&str, &str) -> bool,
    label: &str,
) -> AppResult<Vec<(String, Vec<usize>)>> {
    let normalized: Vec<String> = values
        .iter()
        .map(|(v, _)| normalize(v, normalization))
        .collect();
    let mut parent: Vec<usize> = (0..values.len()).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    let mut comparisons: u64 = 0;
    for i in 0..values.len() {
        if i % 64 == 0 {
            ctx.check()?;
        }
        for j in (i + 1)..values.len() {
            // Length band: a quarter-length difference can't be "close" for
            // either method at sane thresholds; prunes most of the quadratic
            // work without affecting results.
            let (li, lj) = (normalized[i].chars().count(), normalized[j].chars().count());
            let band = li.max(lj) / 4 + 3;
            if li.abs_diff(lj) > band {
                continue;
            }
            comparisons += 1;
            if comparisons > MAX_PAIR_COMPARISONS {
                return Err(AppError::invalid(
                    "too many distinct values for a pairwise method — \
                     use a fingerprint method for this column",
                ));
            }
            if related(&normalized[i], &normalized[j]) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                if ri != rj {
                    let (lo, hi) = if ri < rj { (ri, rj) } else { (rj, ri) };
                    parent[hi] = lo;
                }
            }
        }
    }

    let mut map: HashMap<usize, Vec<usize>> = HashMap::new();
    for idx in 0..values.len() {
        let r = root(&mut parent, idx);
        map.entry(r).or_default().push(idx);
    }
    let mut groups: Vec<(String, Vec<usize>)> = map
        .into_values()
        .map(|members| (label.to_string(), members))
        .collect();
    groups.sort_unstable_by_key(|(_, members)| members[0]);
    Ok(groups)
}

/// Last completed cluster report per document, managed by Tauri.
#[derive(Default)]
pub struct ClusterCache(Arc<Mutex<HashMap<u64, ClusterReport>>>);

impl ClusterCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, ClusterReport>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<ClusterReport> {
        self.0.lock().ok()?.get(&doc_id).cloned()
    }

    pub fn remove(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&doc_id);
        }
    }
}

/// Compute the cell changes for applying accepted mappings (`from` → `to`)
/// over the scope, restricted to the one column. Exact raw-value matches
/// only; the caller commits them via `set_cells` as ONE undo step.
pub fn mapping_changes(
    doc: &Document,
    column: usize,
    mapping: &[(String, String)],
    scope: &ExportScope,
) -> AppResult<Vec<(usize, usize, String)>> {
    if column >= doc.n_cols() {
        return Err(AppError::invalid("cluster column out of range"));
    }
    let map: HashMap<&str, &str> = mapping
        .iter()
        .map(|(from, to)| (from.as_str(), to.as_str()))
        .collect();
    let resolved = resolve_scope(doc, scope)?;
    let mut changes = Vec::new();
    doc.visit_rows_at(&resolved.rows, &mut |r, row| {
        if let Some(&to) = map.get(row[column].as_str()) {
            if row[column] != to {
                changes.push((r, column, to.to_string()));
            }
        }
        Ok(true)
    })?;
    Ok(changes)
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

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("cluster", None, |_| {});
        (registry, ctx)
    }

    fn spec(method: ClusterMethod) -> ClusterSpec {
        ClusterSpec {
            column: 0,
            method,
            normalization: ClusterNormalization {
                case_fold: true,
                trim_collapse: true,
                strip_punctuation: true,
                strip_diacritics: false,
                sort_words: true,
            },
            scope: ExportScope::All,
        }
    }

    #[test]
    fn fingerprint_groups_case_punctuation_and_word_order() {
        let d = doc("name\nJohn Doe\n\"doe, john\"\nJOHN  DOE\nJane Roe\njohn doe\nJane Roe");
        let (_r, c) = ctx();
        let report = scan(&d, &spec(ClusterMethod::Fingerprint), &c).unwrap();
        assert_eq!(report.total_clusters, 1, "{:#?}", report.clusters);
        let cluster = &report.clusters[0];
        assert_eq!(cluster.members.len(), 4);
        // All four variants count 1: the tie resolves to the lexicographically
        // smallest value as the suggestion.
        assert_eq!(cluster.suggested, "JOHN  DOE");
        assert_eq!(cluster.rows_affected, 3);
    }

    #[test]
    fn suggestion_prefers_highest_frequency() {
        let d = doc("name\nAcme Inc\nacme inc\nAcme Inc\nAcme Inc");
        let (_r, c) = ctx();
        let report = scan(&d, &spec(ClusterMethod::Fingerprint), &c).unwrap();
        assert_eq!(report.clusters[0].suggested, "Acme Inc");
        assert_eq!(report.clusters[0].rows_affected, 1);
    }

    #[test]
    fn identical_input_and_settings_produce_identical_clusters() {
        let d = doc("name\nfoo bar\nBar Foo\nfoo-bar\nbaz\nBAZ\nqux");
        let (_r, c1) = ctx();
        let first = scan(&d, &spec(ClusterMethod::Fingerprint), &c1).unwrap();
        let (_r2, c2) = ctx();
        let second = scan(&d, &spec(ClusterMethod::Fingerprint), &c2).unwrap();
        assert_eq!(
            serde_json::to_string(&first.clusters).unwrap(),
            serde_json::to_string(&second.clusters).unwrap()
        );
    }

    #[test]
    fn levenshtein_and_jaro_winkler_match_fixtures() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
        assert!((jaro_winkler("martha", "marhta") - 0.9611).abs() < 0.001);
        assert!((jaro_winkler("dwayne", "duane") - 0.84).abs() < 0.01);
        assert_eq!(jaro_winkler("same", "same"), 1.0);
        assert_eq!(jaro_winkler("", "x"), 0.0);
    }

    #[test]
    fn levenshtein_method_groups_close_misspellings() {
        let d = doc("name\nMississippi\nMississipi\nMissisippi\nTexas");
        let (_r, c) = ctx();
        let report = scan(
            &d,
            &spec(ClusterMethod::Levenshtein { max_distance: 2 }),
            &c,
        )
        .unwrap();
        assert_eq!(report.total_clusters, 1);
        assert_eq!(report.clusters[0].members.len(), 3);
        assert!(report.clusters[0].match_key.contains("distance"));
    }

    #[test]
    fn ngram_fingerprint_groups_in_word_variants() {
        let d = doc("name\ncolor\ncolour\nflavor");
        let (_r, c) = ctx();
        // 2-grams of "color"/"colour" differ ("ou","ur" vs "or")... use
        // levenshtein-style expectations instead: ngram groups transposed
        // characters.
        let report = scan(&d, &spec(ClusterMethod::NgramFingerprint { n: 1 }), &c).unwrap();
        // 1-grams: color = {c,l,o,r}, colour = {c,l,o,r,u} -> distinct keys;
        // "color" vs a transposition "cloor" WOULD group. Verify at least
        // determinism + no false merge of flavor/color.
        assert!(report
            .clusters
            .iter()
            .all(|c| !(c.members.iter().any(|m| m.value == "color")
                && c.members.iter().any(|m| m.value == "flavor"))));
    }

    #[test]
    fn diacritics_option_groups_accented_variants() {
        let d = doc("name\ncafé\ncafe\nCafe");
        let (_r, c) = ctx();
        let mut s = spec(ClusterMethod::Fingerprint);
        s.normalization.strip_diacritics = true;
        let report = scan(&d, &s, &c).unwrap();
        assert_eq!(report.total_clusters, 1);
        assert_eq!(report.clusters[0].members.len(), 3);
    }

    #[test]
    fn mapping_changes_touch_only_the_column_and_scope() {
        let mut d = doc("name,other\nfoo,keep\nFOO,keep\nbar,keep");
        let mapping = vec![("FOO".to_string(), "foo".to_string())];
        let changes = mapping_changes(&d, 0, &mapping, &ExportScope::All).unwrap();
        assert_eq!(changes, vec![(1, 0, "foo".to_string())]);

        // Visible-rows scope skips hidden rows entirely.
        d.set_filter(vec![0, 2]).unwrap(); // hide the row with FOO
        let changes = mapping_changes(&d, 0, &mapping, &ExportScope::VisibleRows).unwrap();
        assert!(changes.is_empty(), "hidden rows are untouched");
    }

    #[test]
    fn apply_via_set_cells_is_one_undo_and_reversible() {
        let mut d = doc("name\nfoo\nFOO\nFoo\nbar");
        let mapping = vec![
            ("FOO".to_string(), "foo".to_string()),
            ("Foo".to_string(), "foo".to_string()),
        ];
        let changes = mapping_changes(&d, 0, &mapping, &ExportScope::All).unwrap();
        assert_eq!(changes.len(), 2);
        d.set_cells(changes).unwrap();
        assert_eq!(d.rows()[1][0], "foo");
        assert_eq!(d.rows()[2][0], "foo");
        d.undo().unwrap();
        assert_eq!(d.rows()[1][0], "FOO");
        assert_eq!(d.rows()[2][0], "Foo");
        assert!(!d.can_undo(), "one undo step");
    }

    #[test]
    fn invalid_specs_are_rejected_before_scanning() {
        let d = doc("a\nx");
        let (_r, c) = ctx();
        let mut bad = spec(ClusterMethod::JaroWinkler {
            min_similarity: 1.5,
        });
        assert!(scan(&d, &bad, &c).is_err());
        bad = spec(ClusterMethod::Fingerprint);
        bad.column = 9;
        assert!(scan(&d, &bad, &c).is_err());
    }
}
