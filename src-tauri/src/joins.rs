//! Relational joins and lookup merges (F21): join two open documents on
//! ordered composite keys into a NEW document (both sources preserved).
//! Key matching reuses the F09 comparison normalizations — trim, case
//! folding, blank equivalence, numeric and date equivalence. The output
//! flows through [`crate::derived::DerivedDocumentBuilder`], so huge
//! results spill to an indexed document automatically.
//!
//! Blank policy: unless `blank_equal` is set, a row whose key contains ANY
//! blank component never matches anything (SQL NULL semantics) — it still
//! appears as an unmatched row where the join type keeps it.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::derived::{unique_column_name, DerivedDocumentBuilder};
use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::index;
use crate::job::JobCtx;
use crate::schema::ColumnSchema;

/// The classic six join types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    LeftAnti,
    RightAnti,
}

/// Key normalizations (mirrors the F09 comparison options).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinNormalization {
    #[serde(default)]
    pub trim: bool,
    #[serde(default)]
    pub case_insensitive: bool,
    /// Blank key components match other blanks. Off = SQL NULL semantics.
    #[serde(default)]
    pub blank_equal: bool,
    /// "1.0" matches "1" when both parse as numbers.
    #[serde(default)]
    pub numeric_equal: bool,
    /// "2024-01-02" matches "01/02/2024" when both parse as dates.
    #[serde(default)]
    pub date_equal: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinSpec {
    pub join: JoinType,
    /// Ordered composite key columns, one list per side (equal lengths).
    pub left_keys: Vec<usize>,
    pub right_keys: Vec<usize>,
    /// Right-side columns to include in the output.
    pub right_columns: Vec<usize>,
    /// Lookup mode: right-side keys must be unique (validated).
    #[serde(default)]
    pub lookup: bool,
    /// Suffix for right column names colliding with left ones.
    #[serde(default = "default_suffix")]
    pub collision_suffix: String,
    #[serde(default)]
    pub normalization: JoinNormalization,
    /// Refuse to run when the projected output exceeds this row count
    /// (`None` = no cap). The UI re-runs with a raised cap after confirming.
    #[serde(default)]
    pub max_output_rows: Option<u64>,
}

fn default_suffix() -> String {
    " (right)".to_string()
}

/// One normalized key component. Tag prefixes keep numeric/date canonical
/// forms from colliding with ordinary text that happens to look the same.
/// When the key column has a DECLARED schema (F31), numeric and date
/// equivalence parse under the declared type — so a locale decimal `1,5` on
/// one side matches `1.5` on the other — and fall back to the heuristics for
/// values the schema rejects.
fn key_component(norm: &JoinNormalization, value: &str, schema: Option<&ColumnSchema>) -> String {
    let base = if norm.trim { value.trim() } else { value };
    if let Some(s) = schema {
        if norm.numeric_equal && s.logical_type.is_numeric() {
            // The f64 canonical form matches the heuristic side's, so typed
            // and untyped documents still join.
            if let crate::schema::NumericCell::Value(n) = crate::schema::numeric_cell(s, base) {
                return format!("n:{n}");
            }
        }
        if norm.date_equal && s.logical_type.is_temporal() {
            if let Some(d) = crate::schema::temporal_cell(s, base) {
                return format!("d:{d}");
            }
        }
    }
    if norm.numeric_equal {
        if let Some(n) = analyze::as_number(base.trim()) {
            return format!("n:{n}");
        }
    }
    if norm.date_equal {
        if let Some(d) = analyze::parse_date(base.trim()) {
            return format!("d:{d}");
        }
    }
    let text = if norm.case_insensitive {
        base.to_lowercase()
    } else {
        base.to_string()
    };
    format!("t:{text}")
}

/// The full key for one row; `None` = "never matches" (blank component
/// under SQL NULL semantics). A configured null token of a DECLARED schema
/// counts as blank: it never matches unless `blank_equal` is set, in which
/// case it matches other blanks. `schemas` runs parallel to `columns`.
fn row_key(
    norm: &JoinNormalization,
    columns: &[usize],
    row: &[String],
    schemas: &[Option<ColumnSchema>],
) -> Option<Vec<String>> {
    let mut key = Vec::with_capacity(columns.len());
    for (i, &c) in columns.iter().enumerate() {
        let value = row.get(c).map(String::as_str).unwrap_or("");
        let schema = schemas.get(i).and_then(Option::as_ref);
        let blank = value.trim().is_empty();
        let nullish = blank || schema.is_some_and(|s| crate::schema::is_null_token(s, value));
        if nullish && !norm.blank_equal {
            return None;
        }
        if nullish && !blank {
            // A null token under blank_equal keys like an EMPTY cell, so
            // "NULL" and "" group together once blanks are declared equal.
            key.push("t:".to_string());
            continue;
        }
        key.push(key_component(norm, value, schema));
    }
    Some(key)
}

/// Cloned declared schemas (F31) for a key-column list.
fn key_schemas(doc: &Document, columns: &[usize]) -> Vec<Option<ColumnSchema>> {
    columns
        .iter()
        .map(|&c| doc.column_schema_at(c).cloned())
        .collect()
}

fn validate(spec: &JoinSpec, left: &Document, right: &Document) -> AppResult<()> {
    if spec.left_keys.is_empty() {
        return Err(AppError::invalid("pick at least one key column"));
    }
    if spec.left_keys.len() != spec.right_keys.len() {
        return Err(AppError::invalid(
            "left and right key lists must have the same length",
        ));
    }
    if let Some(&bad) = spec.left_keys.iter().find(|&&c| c >= left.n_cols()) {
        return Err(AppError::invalid(format!(
            "left key column {bad} is out of range"
        )));
    }
    if let Some(&bad) = spec
        .right_keys
        .iter()
        .chain(spec.right_columns.iter())
        .find(|&&c| c >= right.n_cols())
    {
        return Err(AppError::invalid(format!(
            "right column {bad} is out of range"
        )));
    }
    Ok(())
}

/// The output schema: every left column, then the selected right columns
/// with collision-safe names.
fn output_headers(spec: &JoinSpec, left: &Document, right: &Document) -> Vec<String> {
    let mut headers: Vec<String> = left.headers().to_vec();
    for &c in &spec.right_columns {
        let base = right
            .headers()
            .get(c)
            .cloned()
            .unwrap_or_else(|| format!("Column {}", c + 1));
        let name = if headers.iter().any(|h| h == &base) {
            unique_column_name(&headers, &format!("{base}{}", spec.collision_suffix))
        } else {
            base
        };
        headers.push(name);
    }
    headers
}

/// The materialised right side of a hash join: selected cells per row plus
/// the key index. Bounded by the in-memory budget with a clear error.
struct RightTable {
    /// Selected right-column values per right row.
    rows: Vec<Vec<String>>,
    /// Normalized key -> indices into `rows`. Null-key rows (blank
    /// components under SQL semantics) are absent, so they can never match
    /// and always count as unmatched.
    by_key: HashMap<Vec<String>, Vec<u32>>,
}

fn build_right_table(
    right: &Document,
    spec: &JoinSpec,
    ctx: Option<&JobCtx>,
) -> AppResult<RightTable> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut by_key: HashMap<Vec<String>, Vec<u32>> = HashMap::new();
    let mut bytes = 0u64;
    let right_schemas = key_schemas(right, &spec.right_keys);
    right.visit_rows(0..right.n_rows(), &mut |_, row| {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let idx = rows.len() as u32;
        let cells: Vec<String> = spec
            .right_columns
            .iter()
            .map(|&c| row.get(c).cloned().unwrap_or_default())
            .collect();
        bytes += cells.iter().map(|c| c.len() as u64 + 32).sum::<u64>();
        let key = row_key(&spec.normalization, &spec.right_keys, row, &right_schemas);
        if let Some(key) = &key {
            // The hash table owns the normalized key strings plus map/vec
            // overhead — for key-only joins (empty rightColumns, e.g. an
            // anti-join existence check) this IS the memory, so it must
            // count against the budget too.
            bytes += key.iter().map(|k| k.len() as u64 + 32).sum::<u64>() + 48;
        }
        if bytes > index::SIZE_DECISION_THRESHOLD {
            return Err(AppError::invalid(
                "the right side is too large to hold in memory — swap the sides \
                 or reduce the included right columns",
            ));
        }
        if let Some(key) = key {
            by_key.entry(key).or_default().push(idx);
        }
        rows.push(cells);
        Ok(true)
    })?;
    Ok(RightTable { rows, by_key })
}

/// Cardinality preview — computed without emitting any row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinPreview {
    pub output_columns: Vec<String>,
    /// Inner pairs the key matching produces.
    pub matched_pairs: u64,
    pub left_rows: usize,
    pub right_rows: usize,
    pub left_unmatched: u64,
    pub right_unmatched: u64,
    /// Keys appearing on more than one row, per side.
    pub left_duplicate_keys: u64,
    pub right_duplicate_keys: u64,
    /// Projected output rows for the selected join type.
    pub projected_rows: u64,
    /// The join expands (some key matches many rows on both sides).
    pub expands: bool,
    /// Lookup mode violation: right keys are not unique.
    pub lookup_conflict: bool,
}

pub fn preview(left: &Document, right: &Document, spec: &JoinSpec) -> AppResult<JoinPreview> {
    validate(spec, left, right)?;
    let table = build_right_table(right, spec, None)?;
    let mut left_key_counts: HashMap<Vec<String>, u64> = HashMap::new();
    let mut matched_pairs = 0u64;
    let mut left_unmatched = 0u64;
    let mut left_rows = 0usize;
    let mut matched_right: Vec<bool> = vec![false; table.rows.len()];
    let left_schemas = key_schemas(left, &spec.left_keys);
    left.visit_rows(0..left.n_rows(), &mut |_, row| {
        left_rows += 1;
        match row_key(&spec.normalization, &spec.left_keys, row, &left_schemas) {
            Some(key) => match table.by_key.get(&key) {
                Some(matches) => {
                    matched_pairs += matches.len() as u64;
                    for &m in matches {
                        matched_right[m as usize] = true;
                    }
                    *left_key_counts.entry(key).or_insert(0) += 1;
                }
                None => {
                    left_unmatched += 1;
                    *left_key_counts.entry(key).or_insert(0) += 1;
                }
            },
            None => left_unmatched += 1,
        }
        Ok(true)
    })?;
    let right_unmatched = matched_right.iter().filter(|m| !**m).count() as u64;
    let left_duplicate_keys = left_key_counts.values().filter(|&&n| n > 1).count() as u64;
    let right_duplicate_keys = table.by_key.values().filter(|v| v.len() > 1).count() as u64;

    let projected_rows = match spec.join {
        JoinType::Inner => matched_pairs,
        JoinType::Left => matched_pairs + left_unmatched,
        JoinType::Right => matched_pairs + right_unmatched,
        JoinType::Full => matched_pairs + left_unmatched + right_unmatched,
        JoinType::LeftAnti => left_unmatched,
        JoinType::RightAnti => right_unmatched,
    };
    Ok(JoinPreview {
        output_columns: output_headers(spec, left, right),
        matched_pairs,
        left_rows,
        right_rows: table.rows.len(),
        left_unmatched,
        right_unmatched,
        left_duplicate_keys,
        right_duplicate_keys,
        projected_rows,
        expands: matched_pairs > left_rows.max(table.rows.len()) as u64,
        lookup_conflict: spec.lookup && right_duplicate_keys > 0,
    })
}

/// Run the join into a new document. Both sources are read-only.
pub fn run(
    left: &Document,
    right: &Document,
    spec: &JoinSpec,
    doc_id: u64,
    cache_root: PathBuf,
    ctx: &JobCtx,
) -> AppResult<Document> {
    validate(spec, left, right)?;
    if let Some(cap) = spec.max_output_rows {
        let projected = preview(left, right, spec)?.projected_rows;
        if projected > cap {
            return Err(AppError::invalid(format!(
                "the join would produce {projected} rows, over the configured \
                 threshold of {cap} — confirm to run anyway"
            )));
        }
    }
    ctx.set_total((left.n_rows() + right.n_rows()) as u64);
    ctx.set_message("indexing the right side");
    let table = build_right_table(right, spec, Some(ctx))?;
    ctx.advance(right.n_rows() as u64)?;

    if spec.lookup {
        if let Some((key, rows)) = table.by_key.iter().find(|(_, v)| v.len() > 1) {
            return Err(AppError::invalid(format!(
                "lookup mode needs unique right-side keys, but one key ({}) \
                 appears on {} rows",
                key.join(" · "),
                rows.len()
            )));
        }
    }

    let headers = output_headers(spec, left, right);
    let left_width = left.n_cols();
    let right_width = spec.right_columns.len();
    let mut builder =
        DerivedDocumentBuilder::new(headers, cache_root, crate::derived::SPILL_BUDGET);
    let mut matched_right: Vec<bool> = vec![false; table.rows.len()];

    ctx.set_message("joining");
    let left_schemas = key_schemas(left, &spec.left_keys);
    left.visit_rows(0..left.n_rows(), &mut |_, row| {
        ctx.advance(1)?;
        let key = row_key(&spec.normalization, &spec.left_keys, row, &left_schemas);
        let matches = key.as_ref().and_then(|k| table.by_key.get(k));
        match matches {
            Some(matches) => {
                for &m in matches {
                    matched_right[m as usize] = true;
                }
                match spec.join {
                    JoinType::LeftAnti | JoinType::RightAnti => {}
                    _ => {
                        for &m in matches {
                            let mut out: Vec<String> = row.to_vec();
                            out.extend(table.rows[m as usize].iter().cloned());
                            builder.push_row(out)?;
                        }
                    }
                }
            }
            None => match spec.join {
                JoinType::Left | JoinType::Full | JoinType::LeftAnti => {
                    let mut out: Vec<String> = row.to_vec();
                    out.extend(std::iter::repeat_n(String::new(), right_width));
                    builder.push_row(out)?;
                }
                _ => {}
            },
        }
        Ok(true)
    })?;

    // Right-side completion for the types that keep unmatched right rows.
    if matches!(
        spec.join,
        JoinType::Right | JoinType::Full | JoinType::RightAnti
    ) {
        for (idx, cells) in table.rows.iter().enumerate() {
            ctx.check()?;
            if !matched_right[idx] {
                let mut out: Vec<String> = vec![String::new(); left_width];
                out.extend(cells.iter().cloned());
                builder.push_row(out)?;
            }
        }
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

    fn spec(join: JoinType) -> JoinSpec {
        JoinSpec {
            join,
            left_keys: vec![0],
            right_keys: vec![0],
            right_columns: vec![1],
            lookup: false,
            collision_suffix: " (right)".into(),
            normalization: JoinNormalization {
                trim: true,
                ..Default::default()
            },
            max_output_rows: None,
        }
    }

    fn run_join(left: &Document, right: &Document, spec: &JoinSpec) -> AppResult<Document> {
        let registry = JobRegistry::default();
        let ctx = registry.begin("derive", None, |_| {});
        let dir = tempfile::tempdir().unwrap();
        run(left, right, spec, 9, dir.path().to_path_buf(), &ctx)
    }

    fn left_fixture() -> Document {
        doc("id,name\n1,alpha\n2,beta\n3,gamma\n")
    }

    fn right_fixture() -> Document {
        // id 2 twice (one-to-many), id 4 unmatched on the right.
        doc("id,score\n2,20\n2,21\n4,40\n")
    }

    #[test]
    fn all_six_join_types_match_fixtures() {
        let left = left_fixture();
        let right = right_fixture();

        let inner = run_join(&left, &right, &spec(JoinType::Inner)).unwrap();
        assert_eq!(inner.headers(), &["id", "name", "score"]);
        assert_eq!(inner.n_rows(), 2, "id 2 matches two right rows");
        assert_eq!(inner.rows()[0], vec!["2", "beta", "20"]);
        assert_eq!(inner.rows()[1], vec!["2", "beta", "21"]);

        let left_join = run_join(&left, &right, &spec(JoinType::Left)).unwrap();
        assert_eq!(left_join.n_rows(), 4); // 2 pairs + ids 1 and 3 blank
        assert_eq!(left_join.rows()[0], vec!["1", "alpha", ""]);

        let right_join = run_join(&left, &right, &spec(JoinType::Right)).unwrap();
        assert_eq!(right_join.n_rows(), 3); // 2 pairs + id 4 with blank left
        assert_eq!(right_join.rows()[2], vec!["", "", "40"]);

        let full = run_join(&left, &right, &spec(JoinType::Full)).unwrap();
        assert_eq!(full.n_rows(), 5);

        let left_anti = run_join(&left, &right, &spec(JoinType::LeftAnti)).unwrap();
        assert_eq!(left_anti.n_rows(), 2); // ids 1 and 3
        assert_eq!(left_anti.rows()[0][0], "1");
        assert_eq!(left_anti.rows()[0][2], "", "right side stays blank");

        let right_anti = run_join(&left, &right, &spec(JoinType::RightAnti)).unwrap();
        assert_eq!(right_anti.n_rows(), 1); // id 4
        assert_eq!(right_anti.rows()[0], vec!["", "", "40"]);
    }

    #[test]
    fn composite_key_order_matters() {
        let left = doc("a,b,v\nx,y,1\n");
        let right = doc("a,b,w\ny,x,2\n");
        let mut s = spec(JoinType::Inner);
        s.left_keys = vec![0, 1];
        s.right_keys = vec![0, 1];
        s.right_columns = vec![2];
        let straight = run_join(&left, &right, &s).unwrap();
        assert_eq!(straight.n_rows(), 0, "x·y does not match y·x");

        // Swapping the RIGHT key order aligns the composite keys.
        s.right_keys = vec![1, 0];
        let crossed = run_join(&left, &right, &s).unwrap();
        assert_eq!(crossed.n_rows(), 1);
        assert_eq!(crossed.rows()[0], vec!["x", "y", "1", "2"]);
    }

    #[test]
    fn duplicate_keys_expand_and_are_never_collapsed() {
        let left = doc("k,l\na,1\na,2\n");
        let right = doc("k,r\na,9\na,8\n");
        let inner = run_join(&left, &right, &spec(JoinType::Inner)).unwrap();
        assert_eq!(inner.n_rows(), 4, "2×2 expansion is preserved");

        let p = preview(&left, &right, &spec(JoinType::Inner)).unwrap();
        assert_eq!(p.matched_pairs, 4);
        assert!(p.expands);
        assert_eq!(p.left_duplicate_keys, 1);
        assert_eq!(p.right_duplicate_keys, 1);
    }

    #[test]
    fn lookup_mode_rejects_non_unique_right_keys() {
        let left = left_fixture();
        let right = right_fixture(); // id 2 twice
        let mut s = spec(JoinType::Left);
        s.lookup = true;
        let err = match run_join(&left, &right, &s) {
            Err(e) => e,
            Ok(_) => panic!("lookup with duplicate right keys must fail"),
        };
        assert!(err.to_string().contains("unique"));
        let p = preview(&left, &right, &s).unwrap();
        assert!(p.lookup_conflict);

        // Unique right keys pass.
        let unique_right = doc("id,score\n1,10\n2,20\n");
        assert!(run_join(&left, &unique_right, &s).is_ok());
    }

    #[test]
    fn blank_keys_follow_the_selected_policy() {
        let left = doc("k,l\n,1\na,2\n");
        let right = doc("k,r\n,9\na,8\n");
        // SQL semantics (default): blank never matches blank.
        let sql = run_join(&left, &right, &spec(JoinType::Inner)).unwrap();
        assert_eq!(sql.n_rows(), 1);
        assert_eq!(sql.rows()[0][0], "a");

        // blank_equal: blanks match each other.
        let mut s = spec(JoinType::Inner);
        s.normalization.blank_equal = true;
        let blanky = run_join(&left, &right, &s).unwrap();
        assert_eq!(blanky.n_rows(), 2);

        // Left join keeps the blank-key left row as unmatched under SQL.
        let left_join = run_join(&left, &right, &spec(JoinType::Left)).unwrap();
        assert_eq!(left_join.n_rows(), 2);
        assert_eq!(left_join.rows()[0], vec!["", "1", ""]);
    }

    #[test]
    fn numeric_and_date_equivalence_normalize_keys() {
        let left = doc("k,l\n1.0,x\n2024-01-02,y\n");
        let right = doc("k,r\n1,10\n01/02/2024,20\n");
        let mut s = spec(JoinType::Inner);
        s.normalization.numeric_equal = true;
        s.normalization.date_equal = true;
        let joined = run_join(&left, &right, &s).unwrap();
        assert_eq!(joined.n_rows(), 2, "1.0=1 numerically, the dates match");
    }

    #[test]
    fn case_insensitive_keys_match() {
        let left = doc("k,l\nAlpha,1\n");
        let right = doc("k,r\nALPHA,2\n");
        let mut s = spec(JoinType::Inner);
        s.normalization.case_insensitive = true;
        assert_eq!(run_join(&left, &right, &s).unwrap().n_rows(), 1);
    }

    #[test]
    fn collision_suffix_renames_right_columns() {
        let left = doc("id,name\n1,x\n");
        let right = doc("id,name\n1,y\n");
        let mut s = spec(JoinType::Inner);
        s.right_columns = vec![0, 1]; // include the colliding "id" and "name"
        let joined = run_join(&left, &right, &s).unwrap();
        assert_eq!(
            joined.headers(),
            &["id", "name", "id (right)", "name (right)"]
        );
    }

    #[test]
    fn output_cap_requires_confirmation() {
        let left = doc("k,l\na,1\na,2\n");
        let right = doc("k,r\na,9\na,8\n");
        let mut s = spec(JoinType::Inner);
        s.max_output_rows = Some(3); // 4 projected
        let err = match run_join(&left, &right, &s) {
            Err(e) => e,
            Ok(_) => panic!("over-threshold join must fail"),
        };
        assert!(err.to_string().contains("threshold"));
        s.max_output_rows = Some(10);
        assert!(run_join(&left, &right, &s).is_ok());
    }

    #[test]
    fn sources_are_never_mutated() {
        let left = left_fixture();
        let right = right_fixture();
        let (lr, rr) = (left.revision(), right.revision());
        let _ = run_join(&left, &right, &spec(JoinType::Full)).unwrap();
        assert_eq!(left.revision(), lr);
        assert_eq!(right.revision(), rr);
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let left = left_fixture();
        let right = right_fixture();
        let mut s = spec(JoinType::Inner);
        s.left_keys = vec![];
        assert!(preview(&left, &right, &s).is_err());
        let mut s = spec(JoinType::Inner);
        s.right_keys = vec![0, 1];
        assert!(preview(&left, &right, &s).is_err());
        let mut s = spec(JoinType::Inner);
        s.right_columns = vec![9];
        assert!(preview(&left, &right, &s).is_err());
    }

    // ----- declared schemas (F31) -------------------------------------------

    fn declare_with(
        d: &mut Document,
        col: usize,
        lt: crate::schema::LogicalType,
        f: impl FnOnce(&mut crate::schema::ColumnSchema),
    ) {
        let mut schema = crate::schema::ColumnSchema::new(
            d.column_ids()[col].clone(),
            d.headers()[col].clone(),
            lt,
        );
        f(&mut schema);
        d.set_column_schema(schema);
    }

    #[test]
    fn declared_locale_decimal_joins_across_locales() {
        // Left keys are de-DE decimals ("1,5" quoted), right keys plain.
        // Declared types make both sides canonicalise to the same numeric
        // key, so the join matches across notations.
        let mut left = doc("amount,who\n\"1,5\",ann\n\"2,25\",bob\n");
        let mut right = doc("amount,tag\n1.5,x\n9.9,y\n");
        declare_with(&mut left, 0, crate::schema::LogicalType::Decimal, |s| {
            s.locale = Some("de-DE".to_string());
        });
        declare_with(&mut right, 0, crate::schema::LogicalType::Decimal, |_| {});
        let mut s = spec(JoinType::Inner);
        s.normalization.numeric_equal = true;
        let p = preview(&left, &right, &s).unwrap();
        assert_eq!(p.matched_pairs, 1, "1,5 (de) matches 1.5 (en)");
        let out = run_join(&left, &right, &s).unwrap();
        assert_eq!(out.n_rows(), 1);
        assert_eq!(out.rows()[0][0], "1,5", "source text is never rewritten");
    }

    #[test]
    fn declared_null_token_keys_use_null_semantics() {
        let mut left = doc("id,name\nNULL,ann\n1,bob\n");
        let mut right = doc("id,tag\nNULL,x\n,y\n1,z\n");
        for d in [&mut left, &mut right] {
            declare_with(d, 0, crate::schema::LogicalType::Integer, |s| {
                s.null_tokens = vec!["NULL".to_string()];
            });
        }
        // Without blank_equal, a NULL-token key NEVER matches (SQL NULL).
        let p = preview(&left, &right, &spec(JoinType::Inner)).unwrap();
        assert_eq!(p.matched_pairs, 1, "only the 1↔1 pair");
        // With blank_equal, NULL keys match blanks AND other NULLs.
        let mut s = spec(JoinType::Inner);
        s.normalization.blank_equal = true;
        let p = preview(&left, &right, &s).unwrap();
        assert_eq!(p.matched_pairs, 3, "NULL↔NULL, NULL↔blank, 1↔1");
    }
}
