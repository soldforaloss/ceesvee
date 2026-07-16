//! Semantic data-type detection (F26): recognise real-world value types
//! beyond number/date/bool/text. Detection NEVER mutates data; a badge is
//! only assigned above a documented confidence threshold, and every
//! type-specific action previews its exact changes before applying (as one
//! undo step through the ordinary edit paths).

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;

/// A column earns a semantic badge only when at least this share of its
/// non-blank cells match the detector…
pub const CONFIDENCE_THRESHOLD: f64 = 0.95;
/// …and it has at least this many non-blank cells (tiny samples lie).
pub const MIN_NON_BLANK: usize = 10;
/// Categorical detection: distinct/non-blank at most this ratio…
const CATEGORICAL_MAX_RATIO: f64 = 0.1;
/// …and at most this many distinct values.
const CATEGORICAL_MAX_DISTINCT: usize = 100;
/// Indexed documents scan this many leading rows only; the report is
/// explicitly labelled as sampled — a sample is evidence, not certainty.
pub const INDEXED_SAMPLE_ROWS: usize = 100_000;
const ROW_CHUNK: usize = 4096;

/// The closed set of detectable semantic types, most-specific first — the
/// declaration order IS the tie-break priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SemanticType {
    Uuid,
    Email,
    Url,
    Ipv4,
    Ipv6,
    Json,
    Percentage,
    Currency,
    PhoneNumber,
    PostalCode,
    Categorical,
    /// Never detected — the explicit override for "treat as plain text".
    FreeText,
}

/// Per-cell detectors in priority order (the statistical `Categorical` and
/// the `FreeText` override are not in this list).
const PATTERN_TYPES: [SemanticType; 10] = [
    SemanticType::Uuid,
    SemanticType::Email,
    SemanticType::Url,
    SemanticType::Ipv4,
    SemanticType::Ipv6,
    SemanticType::Json,
    SemanticType::Percentage,
    SemanticType::Currency,
    SemanticType::PhoneNumber,
    SemanticType::PostalCode,
];

fn regexes() -> &'static HashMap<SemanticType, Regex> {
    static REGEXES: OnceLock<HashMap<SemanticType, Regex>> = OnceLock::new();
    REGEXES.get_or_init(|| {
        let mut map = HashMap::new();
        let mut add = |t: SemanticType, pattern: &str| {
            map.insert(t, Regex::new(pattern).expect("static regex"));
        };
        add(
            SemanticType::Uuid,
            r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
        );
        add(SemanticType::Email, r"^[^@\s]+@[^@\s]+\.[^@\s.]+$");
        add(SemanticType::Url, r"^(https?|ftp)://[^\s/$.?#].[^\s]*$");
        add(SemanticType::Percentage, r"^[+-]?\d+(\.\d+)?\s?%$");
        add(
            SemanticType::Currency,
            r"^[+-]?[$€£¥]\s?\d{1,3}(,\d{3})*(\.\d{1,2})?$|^[+-]?\d{1,3}(,\d{3})*(\.\d{1,2})?\s?(USD|EUR|GBP|JPY|CAD|AUD)$",
        );
        // Phone: optional +country, 7-15 digits with common separators.
        // Detection only — phone values are NEVER converted to numbers.
        add(
            SemanticType::PhoneNumber,
            r"^\+?\d{1,3}?[\s.-]?\(?\d{2,4}\)?([\s.-]?\d{2,4}){2,4}$",
        );
        // US ZIP(+4), UK, and Canadian shapes; conservative on purpose, and
        // postal values are NEVER converted to numbers either.
        add(
            SemanticType::PostalCode,
            r"^(\d{5}(-\d{4})?|[A-Za-z]\d[A-Za-z]\s?\d[A-Za-z]\d|[A-Za-z]{1,2}\d[A-Za-z\d]?\s?\d[A-Za-z]{2})$",
        );
        map
    })
}

/// Whether one trimmed, non-blank cell matches a semantic type.
pub fn matches_type(value: &str, semantic: SemanticType) -> bool {
    let trimmed = value.trim();
    match semantic {
        // The std parsers are stricter and more correct than any regex here
        // (compressed IPv6 forms, octet ranges, no leading zeros).
        SemanticType::Ipv4 => trimmed.parse::<Ipv4Addr>().is_ok(),
        SemanticType::Ipv6 => trimmed.contains(':') && trimmed.parse::<Ipv6Addr>().is_ok(),
        SemanticType::Json => {
            (trimmed.starts_with('{') || trimmed.starts_with('['))
                && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
        }
        SemanticType::Uuid => {
            trimmed.len() == 36 && regexes()[&SemanticType::Uuid].is_match(trimmed)
        }
        SemanticType::PhoneNumber => {
            let digits = trimmed.chars().filter(char::is_ascii_digit).count();
            (7..=15).contains(&digits) && regexes()[&SemanticType::PhoneNumber].is_match(trimmed)
        }
        // Statistical / override-only kinds have no per-cell test.
        SemanticType::Categorical | SemanticType::FreeText => false,
        other => regexes()[&other].is_match(trimmed),
    }
}

/// Per-column detection result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSemantics {
    pub column: usize,
    /// The badge, when a type cleared the documented threshold.
    pub detected: Option<SemanticType>,
    /// Best-scoring candidate even when nothing cleared the threshold, so
    /// the UI can say "Email — 82%, below the badge threshold".
    pub best_candidate: Option<SemanticType>,
    /// Matching share of non-blank cells for the detected (or best) type.
    pub confidence: f64,
    pub matching: usize,
    pub conflicting: usize,
    pub non_blank: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticReport {
    pub revision: u64,
    /// True when only a leading sample was scanned (large indexed documents).
    pub sampled: bool,
    pub scanned_rows: usize,
    pub threshold: f64,
    pub columns: Vec<ColumnSemantics>,
}

/// Scan every column. Read-only; progress and cancellation via `ctx`.
pub fn scan(doc: &Document, ctx: &JobCtx) -> AppResult<SemanticReport> {
    let n_cols = doc.n_cols();
    let total = doc.n_rows();
    let sampled = !doc.is_editable() && total > INDEXED_SAMPLE_ROWS;
    let scan_rows = if sampled { INDEXED_SAMPLE_ROWS } else { total };
    ctx.set_total(scan_rows as u64);

    #[derive(Default)]
    struct Acc {
        non_blank: usize,
        per_type: HashMap<SemanticType, usize>,
        distinct: std::collections::HashSet<String>,
        distinct_overflow: bool,
    }
    let mut accs: Vec<Acc> = (0..n_cols).map(|_| Acc::default()).collect();

    let mut pending = 0u64;
    doc.visit_rows(0..scan_rows, &mut |_, row| {
        for (c, acc) in accs.iter_mut().enumerate() {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                continue;
            }
            acc.non_blank += 1;
            for t in PATTERN_TYPES {
                if matches_type(trimmed, t) {
                    *acc.per_type.entry(t).or_insert(0) += 1;
                }
            }
            if !acc.distinct_overflow {
                acc.distinct.insert(trimmed.to_string());
                if acc.distinct.len() > CATEGORICAL_MAX_DISTINCT {
                    acc.distinct_overflow = true;
                    acc.distinct.clear(); // past categorical range; free it
                }
            }
        }
        pending += 1;
        if pending >= ROW_CHUNK as u64 {
            ctx.advance(pending)?;
            pending = 0;
        }
        Ok(true)
    })?;
    ctx.advance(pending)?;

    let columns = accs
        .into_iter()
        .enumerate()
        .map(|(column, acc)| {
            // Best candidate: highest match count, priority order breaking
            // ties (deterministic).
            let mut best: Option<(SemanticType, usize)> = None;
            for t in PATTERN_TYPES {
                let count = acc.per_type.get(&t).copied().unwrap_or(0);
                if count > 0 && best.is_none_or(|(_, b)| count > b) {
                    best = Some((t, count));
                }
            }
            let mut detected = None;
            if acc.non_blank >= MIN_NON_BLANK {
                if let Some((t, count)) = best {
                    if count as f64 / acc.non_blank as f64 >= CONFIDENCE_THRESHOLD {
                        detected = Some(t);
                    }
                }
                if detected.is_none()
                    && !acc.distinct_overflow
                    && (acc.distinct.len() as f64 / acc.non_blank as f64) <= CATEGORICAL_MAX_RATIO
                {
                    detected = Some(SemanticType::Categorical);
                    best = Some((SemanticType::Categorical, acc.non_blank));
                }
            }
            let (best_candidate, matching) = match best {
                Some((t, count)) => (Some(t), count),
                None => (None, 0),
            };
            ColumnSemantics {
                column,
                detected,
                best_candidate,
                confidence: if acc.non_blank == 0 {
                    0.0
                } else {
                    matching as f64 / acc.non_blank as f64
                },
                matching,
                conflicting: acc.non_blank.saturating_sub(matching),
                non_blank: acc.non_blank,
            }
        })
        .collect();

    Ok(SemanticReport {
        revision: doc.revision(),
        sampled,
        scanned_rows: scan_rows,
        threshold: CONFIDENCE_THRESHOLD,
        columns,
    })
}

// ----- rows for the filter actions --------------------------------------------------

/// Absolute row indices whose cell in `column` is (in)valid for `semantic`.
/// Blank cells count as neither: they are excluded from both filters.
pub fn semantic_rows(
    doc: &Document,
    column: usize,
    semantic: SemanticType,
    valid: bool,
) -> AppResult<Vec<usize>> {
    if column >= doc.n_cols() {
        return Err(AppError::invalid("column out of range"));
    }
    let mut out = Vec::new();
    doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
        let trimmed = row[column].trim();
        if !trimmed.is_empty() && matches_type(trimmed, semantic) == valid {
            out.push(i);
        }
        Ok(true)
    })?;
    Ok(out)
}

// ----- type-specific actions ---------------------------------------------------------

/// The closed set of previewable semantic actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SemanticAction {
    /// Canonical form per type: lowercase emails/UUIDs, trimmed values.
    Normalize,
    /// Replace percentages ("12.5%") with their decimal value ("0.125").
    PercentToDecimal,
    /// New column holding each URL's host.
    ExtractUrlHost,
    /// New column holding each email's domain.
    ExtractEmailDomain,
}

/// What an action would change, computed without mutating anything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticActionPreview {
    pub affected: usize,
    /// Leading examples as (before, after) pairs.
    pub examples: Vec<(String, String)>,
    /// The new column's name, for the extraction actions.
    pub new_column: Option<String>,
}

const PREVIEW_EXAMPLES: usize = 20;

fn url_host(value: &str) -> Option<String> {
    let rest = value.trim().split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.split('@').next_back()?; // strip userinfo
    let host = host.split(':').next()?; // strip port
    (!host.is_empty()).then(|| host.to_string())
}

fn email_domain(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let domain = trimmed.rsplit('@').next()?;
    (domain.len() < trimmed.len() && !domain.is_empty() && domain.contains('.'))
        .then(|| domain.to_string())
}

fn percent_to_decimal(value: &str) -> Option<String> {
    let t = value.trim().strip_suffix('%')?.trim();
    let n: f64 = t.parse().ok()?;
    Some((n / 100.0).to_string())
}

fn normalized(value: &str, semantic: SemanticType) -> Option<String> {
    let trimmed = value.trim();
    let out = match semantic {
        SemanticType::Email | SemanticType::Uuid => trimmed.to_lowercase(),
        _ => trimmed.to_string(),
    };
    (out != value).then_some(out)
}

/// Compute the exact changes an action would make: `(cell_changes,
/// new_column)`. The extraction actions fill a NEW column (committed via
/// `replace_columns` as one undo step); the in-place actions return cell
/// changes (committed via `set_cells`, also one undo step).
#[allow(clippy::type_complexity)]
pub fn action_changes(
    doc: &Document,
    column: usize,
    semantic: SemanticType,
    action: SemanticAction,
) -> AppResult<(Vec<(usize, usize, String)>, Option<(String, Vec<String>)>)> {
    if column >= doc.n_cols() {
        return Err(AppError::invalid("column out of range"));
    }
    let header = doc
        .headers()
        .get(column)
        .cloned()
        .unwrap_or_else(|| format!("Column {}", column + 1));

    match action {
        SemanticAction::Normalize | SemanticAction::PercentToDecimal => {
            let mut changes = Vec::new();
            doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
                let cell = row[column].as_str();
                if cell.trim().is_empty() {
                    return Ok(true);
                }
                let next = match action {
                    SemanticAction::PercentToDecimal => {
                        matches_type(cell, SemanticType::Percentage)
                            .then(|| percent_to_decimal(cell))
                            .flatten()
                    }
                    _ => normalized(cell, semantic),
                };
                if let Some(next) = next {
                    if next != cell {
                        changes.push((i, column, next));
                    }
                }
                Ok(true)
            })?;
            Ok((changes, None))
        }
        SemanticAction::ExtractUrlHost | SemanticAction::ExtractEmailDomain => {
            let (suffix, extract): (&str, fn(&str) -> Option<String>) = match action {
                SemanticAction::ExtractUrlHost => ("host", url_host),
                _ => ("domain", email_domain),
            };
            let mut values = Vec::with_capacity(doc.n_rows());
            doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
                values.push(extract(&row[column]).unwrap_or_default());
                Ok(true)
            })?;
            Ok((Vec::new(), Some((format!("{header} {suffix}"), values))))
        }
    }
}

/// Bounded preview of [`action_changes`] — counts plus leading examples.
pub fn preview_action(
    doc: &Document,
    column: usize,
    semantic: SemanticType,
    action: SemanticAction,
) -> AppResult<SemanticActionPreview> {
    let (changes, new_column) = action_changes(doc, column, semantic, action)?;
    match new_column {
        Some((name, values)) => {
            // Examples come from the first rows that actually extract.
            let sample: Vec<usize> = values
                .iter()
                .enumerate()
                .filter(|(_, v)| !v.is_empty())
                .take(PREVIEW_EXAMPLES)
                .map(|(i, _)| i)
                .collect();
            let examples = doc
                .fetch_rows(&sample)?
                .into_iter()
                .zip(sample.iter())
                .map(|(row, &i)| (row[column].clone(), values[i].clone()))
                .collect();
            Ok(SemanticActionPreview {
                affected: values.iter().filter(|v| !v.is_empty()).count(),
                examples,
                new_column: Some(name),
            })
        }
        None => {
            let sample: Vec<usize> = changes
                .iter()
                .take(PREVIEW_EXAMPLES)
                .map(|(r, _, _)| *r)
                .collect();
            let examples = doc
                .fetch_rows(&sample)?
                .into_iter()
                .zip(changes.iter())
                .map(|(row, (_, _, next))| (row[column].clone(), next.clone()))
                .collect();
            Ok(SemanticActionPreview {
                affected: changes.len(),
                examples,
                new_column: None,
            })
        }
    }
}

/// Last completed semantic report per document, managed by Tauri.
#[derive(Default)]
pub struct SemanticCache(Arc<Mutex<HashMap<u64, SemanticReport>>>);

impl SemanticCache {
    pub fn share(&self) -> Arc<Mutex<HashMap<u64, SemanticReport>>> {
        Arc::clone(&self.0)
    }

    pub fn get(&self, doc_id: u64) -> Option<SemanticReport> {
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

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let ctx = registry.begin("semantic", None, |_| {});
        (registry, ctx)
    }

    #[test]
    fn detectors_classify_fixtures_and_reject_near_misses() {
        use SemanticType::*;
        let cases: &[(SemanticType, &str, &str)] = &[
            (Email, "a.user@example.co.uk", "not-an-email@nowhere"),
            (Url, "https://example.com/x?q=1", "example.com/no-scheme"),
            (
                Uuid,
                "550e8400-e29b-41d4-a716-446655440000",
                "550e8400-e29b-41d4-a716",
            ),
            (Ipv4, "192.168.0.1", "999.1.1.1"),
            (Ipv6, "2001:db8::ff00:42:8329", "2001:::1"),
            (Percentage, "12.5%", "12.5"),
            (Currency, "$1,234.56", "1234x"),
            (PhoneNumber, "+1 555-123-4567", "12"),
            (PostalCode, "90210-1234", "9021"),
            (Json, r#"{"a": 1}"#, "{broken"),
        ];
        for (t, good, bad) in cases {
            assert!(matches_type(good, *t), "{t:?} should match {good}");
            assert!(!matches_type(bad, *t), "{t:?} should reject {bad}");
        }
    }

    #[test]
    fn scan_badges_only_above_the_threshold() {
        // 12 emails + 1 conflicting value -> 12/13 ≈ 0.92 < 0.95: NO badge,
        // but the best candidate is still reported for the UI.
        let mut csv = String::from("contact\n");
        for i in 0..12 {
            csv.push_str(&format!("user{i}@example.com\n"));
        }
        csv.push_str("not an email\n");
        let d = doc(&csv);
        let (_r, c) = ctx();
        let report = scan(&d, &c).unwrap();
        assert!(report.columns[0].detected.is_none());
        assert_eq!(report.columns[0].best_candidate, Some(SemanticType::Email));
        assert_eq!(report.columns[0].conflicting, 1);
        assert!(!report.sampled);

        // All 13 valid -> badge.
        let all_valid = doc(&format!(
            "contact\n{}",
            (0..13)
                .map(|i| format!("user{i}@example.com"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        let (_r2, c2) = ctx();
        let report = scan(&all_valid, &c2).unwrap();
        assert_eq!(report.columns[0].detected, Some(SemanticType::Email));
        assert!(report.columns[0].confidence >= CONFIDENCE_THRESHOLD);
    }

    #[test]
    fn small_columns_never_badge() {
        // 5 perfectly valid emails — still below MIN_NON_BLANK.
        let d = doc("contact\na@b.co\nc@d.co\ne@f.co\ng@h.co\ni@j.co\n");
        let (_r, c) = ctx();
        let report = scan(&d, &c).unwrap();
        assert!(report.columns[0].detected.is_none());
        assert_eq!(report.columns[0].best_candidate, Some(SemanticType::Email));
    }

    #[test]
    fn phone_and_postal_are_detected_without_touching_values() {
        let mut csv = String::from("phone\n");
        for i in 0..12 {
            csv.push_str(&format!("+1 555-000-{i:04}\n"));
        }
        let d = doc(&csv);
        let before = d.rows().to_vec();
        let (_r, c) = ctx();
        let report = scan(&d, &c).unwrap();
        assert_eq!(report.columns[0].detected, Some(SemanticType::PhoneNumber));
        assert_eq!(d.rows(), &before[..], "detection never mutates");
        assert_eq!(d.rows()[0][0], "+1 555-000-0000", "no numeric conversion");
    }

    #[test]
    fn categorical_falls_out_of_low_cardinality_text() {
        let mut csv = String::from("status\n");
        for i in 0..200 {
            csv.push_str(if i % 3 == 0 { "open\n" } else { "closed\n" });
        }
        let d = doc(&csv);
        let (_r, c) = ctx();
        let report = scan(&d, &c).unwrap();
        assert_eq!(report.columns[0].detected, Some(SemanticType::Categorical));

        // High-cardinality free text earns no badge at all.
        let mut wide = String::from("notes\n");
        for i in 0..200 {
            wide.push_str(&format!("unique sentence number {i} with words\n"));
        }
        let d = doc(&wide);
        let (_r2, c2) = ctx();
        let report = scan(&d, &c2).unwrap();
        assert!(report.columns[0].detected.is_none());
    }

    #[test]
    fn filter_rows_split_valid_and_invalid_excluding_blanks() {
        let d = doc("email,n\na@b.co,1\nbroken,2\n,3\nc@d.io,4\n");
        let valid = semantic_rows(&d, 0, SemanticType::Email, true).unwrap();
        let invalid = semantic_rows(&d, 0, SemanticType::Email, false).unwrap();
        assert_eq!(valid, vec![0, 3]);
        assert_eq!(invalid, vec![1]); // the blank cell is in neither
    }

    #[test]
    fn percent_to_decimal_and_normalize_previews_are_exact() {
        let d = doc("pct\n12.5%\n7%\nnot a pct\n");
        let preview = preview_action(
            &d,
            0,
            SemanticType::Percentage,
            SemanticAction::PercentToDecimal,
        )
        .unwrap();
        assert_eq!(preview.affected, 2);
        assert_eq!(
            preview.examples[0],
            ("12.5%".to_string(), "0.125".to_string())
        );
        assert_eq!(preview.examples[1], ("7%".to_string(), "0.07".to_string()));

        let d = doc("email\nUser@Example.COM\nok@ok.io\n");
        let preview =
            preview_action(&d, 0, SemanticType::Email, SemanticAction::Normalize).unwrap();
        assert_eq!(preview.affected, 1);
        assert_eq!(
            preview.examples[0],
            (
                "User@Example.COM".to_string(),
                "user@example.com".to_string()
            )
        );
    }

    #[test]
    fn extraction_actions_build_a_new_column() {
        let d = doc("url\nhttps://user:pw@sub.example.com:8080/path?q=1\nplain text\n");
        let (changes, new_column) =
            action_changes(&d, 0, SemanticType::Url, SemanticAction::ExtractUrlHost).unwrap();
        assert!(changes.is_empty());
        let (name, values) = new_column.unwrap();
        assert_eq!(name, "url host");
        assert_eq!(values[0], "sub.example.com");
        assert_eq!(values[1], "");

        let d = doc("email\na@sub.example.org\nno-at-sign.org\n");
        let (_, new_column) = action_changes(
            &d,
            0,
            SemanticType::Email,
            SemanticAction::ExtractEmailDomain,
        )
        .unwrap();
        let (_, values) = new_column.unwrap();
        assert_eq!(values[0], "sub.example.org");
        assert_eq!(values[1], "", "a value without '@' extracts nothing");
    }

    #[test]
    fn applying_changes_is_one_undo_step() {
        let mut d = doc("pct\n50%\n25%\n");
        let (changes, _) = action_changes(
            &d,
            0,
            SemanticType::Percentage,
            SemanticAction::PercentToDecimal,
        )
        .unwrap();
        d.set_cells(changes).unwrap();
        assert_eq!(d.rows()[0][0], "0.5");
        assert_eq!(d.rows()[1][0], "0.25");
        d.undo().unwrap();
        assert_eq!(d.rows()[0][0], "50%");
        assert!(!d.can_undo(), "one undo step");
    }

    #[test]
    fn extraction_commit_is_one_undo_step() {
        let mut d = doc("url,x\nhttps://a.example.com/p,1\nplain,2\n");
        let (_, new_column) =
            action_changes(&d, 0, SemanticType::Url, SemanticAction::ExtractUrlHost).unwrap();
        let (name, values) = new_column.unwrap();
        d.replace_columns(Vec::new(), 1, vec![(name, values)])
            .unwrap();
        assert_eq!(d.headers(), &["url", "url host", "x"]);
        assert_eq!(d.rows()[0][1], "a.example.com");
        assert_eq!(d.rows()[1][1], "");
        d.undo().unwrap();
        assert_eq!(d.headers(), &["url", "x"]);
        assert!(!d.can_undo(), "one undo step");
    }

    #[test]
    fn uuid_normalization_lowercases_and_preserves_version() {
        let d = doc("id\n550E8400-E29B-41D4-A716-446655440000\n");
        let (changes, _) =
            action_changes(&d, 0, SemanticType::Uuid, SemanticAction::Normalize).unwrap();
        assert_eq!(changes[0].2, "550e8400-e29b-41d4-a716-446655440000");
        // The version nibble (leader of the 3rd group) survives untouched.
        assert_eq!(
            changes[0].2.split('-').nth(2).unwrap().chars().next(),
            Some('4')
        );
    }
}
